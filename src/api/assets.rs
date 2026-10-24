//! Asset manager API: directory tree, upload flow, metadata edits, `@path` resolution, search.
//!
//! Upload flow (browser → blob storage directly):
//! 1. `POST /projects/{pid}/assets/uploads` → `{ upload_id, upload_url, headers }`
//! 2. client `PUT`s the bytes to `upload_url` with the returned headers
//! 3. `POST /projects/{pid}/assets` (finalize) with `upload_id`, directory, name, description

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::ai::vision;
use crate::auth::AuthUser;
use crate::domain::access;
use crate::domain::assets::{
    self, Asset, AssetDir, AssetDirSummary, AssetKind, AssetPatch, DirPatch, MAX_ASSET_BYTES,
    NewAsset, ResolvedRef, SearchResults,
};
use crate::domain::new_id;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// Largest file hashed server-side when the client did not send a sha256.
const MAX_SERVER_HASH_BYTES: u64 = 64 * 1024 * 1024;
/// Largest image sent to the vision model for an automatic description.
const MAX_DESCRIBE_BYTES: u64 = 8 * 1024 * 1024;
const UPLOAD_TTL_MINUTES: i64 = 30;

fn parse_kind(raw: Option<&str>) -> AppResult<Option<AssetKind>> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(k) => AssetKind::parse(k).map(Some).ok_or_else(|| {
            AppError::Invalid(format!(
                "kind must be image, video, audio or other, got {k}"
            ))
        }),
    }
}

async fn with_urls(s: &AppState, mut a: Vec<Asset>) -> AppResult<Vec<Asset>> {
    assets::attach_read_urls(s.blob.as_ref(), &mut a)
        .await
        .map_err(AppError::Other)?;
    Ok(a)
}

async fn one_with_url(s: &AppState, a: Asset) -> AppResult<Asset> {
    Ok(with_urls(s, vec![a]).await?.remove(0))
}

// ── Directories ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateDirRequest {
    /// Full path of the new directory, e.g. `Characters/Maya`. The parent must exist.
    pub path: String,
    /// Required: what lives here and how it should be used.
    pub description: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PatchDirRequest {
    /// Rename (single path segment).
    pub name: Option<String>,
    /// Move under this directory path (empty string = root).
    pub parent_path: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct DeleteDirQuery {
    /// Required to delete a non-empty directory (deletes its files and blobs).
    #[serde(default)]
    pub recursive: bool,
}

#[utoipa::path(
    get,
    path = "/projects/{pid}/asset-dirs",
    params(("pid" = Uuid, Path, description = "Project id")),
    responses((status = 200, description = "Every directory (flat, ordered by path) with counts", body = [AssetDirSummary]))
)]
pub async fn list_dirs(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
) -> AppResult<Json<Vec<AssetDirSummary>>> {
    access::ensure_project_owner(&s.db, &user.id, pid).await?;
    Ok(Json(assets::list_all_dirs(&s.db, pid).await?))
}

#[utoipa::path(
    post,
    path = "/projects/{pid}/asset-dirs",
    params(("pid" = Uuid, Path, description = "Project id")),
    request_body = CreateDirRequest,
    responses(
        (status = 201, description = "Created", body = AssetDir),
        (status = 409, description = "Path already exists"),
        (status = 422, description = "Invalid path, missing parent, or missing description")
    )
)]
pub async fn create_dir(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
    Json(body): Json<CreateDirRequest>,
) -> AppResult<(StatusCode, Json<AssetDir>)> {
    access::ensure_project_owner(&s.db, &user.id, pid).await?;
    let dir = assets::create_dir(&s.db, pid, &body.path, &body.description).await?;
    Ok((StatusCode::CREATED, Json(dir)))
}

#[utoipa::path(
    patch,
    path = "/asset-dirs/{id}",
    params(("id" = Uuid, Path, description = "Directory id")),
    request_body = PatchDirRequest,
    responses((status = 200, description = "Updated; descendant paths follow", body = AssetDir))
)]
pub async fn patch_dir(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<PatchDirRequest>,
) -> AppResult<Json<AssetDir>> {
    access::ensure_asset_dir_owner(&s.db, &user.id, id).await?;
    let dir = assets::update_dir(
        &s.db,
        id,
        DirPatch {
            name: body.name,
            parent_path: body.parent_path,
            description: body.description,
        },
    )
    .await?;
    Ok(Json(dir))
}

#[utoipa::path(
    delete,
    path = "/asset-dirs/{id}",
    params(
        ("id" = Uuid, Path, description = "Directory id"),
        ("recursive" = Option<bool>, Query, description = "Delete contents too")
    ),
    responses((status = 204, description = "Deleted"), (status = 409, description = "Directory not empty"))
)]
pub async fn delete_dir(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Query(q): Query<DeleteDirQuery>,
) -> AppResult<StatusCode> {
    access::ensure_asset_dir_owner(&s.db, &user.id, id).await?;
    let blobs = assets::delete_dir(&s.db, id, q.recursive).await?;
    for b in blobs {
        if let Err(e) = s.blob.delete(&b).await {
            tracing::warn!(error = %e, blob = %b, "asset blob delete failed");
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

// ── Listing, search, resolve ─────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    /// Directory path to list; empty / omitted lists the root directories.
    pub dir: Option<String>,
    /// image | video | audio | other
    pub kind: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DirListing {
    /// Canonical path that was listed (empty = root).
    pub path: String,
    pub dirs: Vec<AssetDirSummary>,
    pub assets: Vec<Asset>,
}

#[utoipa::path(
    get,
    path = "/projects/{pid}/assets",
    params(
        ("pid" = Uuid, Path, description = "Project id"),
        ("dir" = Option<String>, Query, description = "Directory path (default: root)"),
        ("kind" = Option<String>, Query, description = "Filter by kind")
    ),
    responses((status = 200, description = "Direct children of the directory", body = DirListing))
)]
pub async fn list_assets(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<DirListing>> {
    access::ensure_project_owner(&s.db, &user.id, pid).await?;
    let kind = parse_kind(q.kind.as_deref())?;
    let path = assets::normalize_path(q.dir.as_deref().unwrap_or("")).map_err(AppError::Invalid)?;
    let (dirs, files) = assets::list_children(&s.db, pid, &path, kind).await?;
    Ok(Json(DirListing {
        path,
        dirs,
        assets: with_urls(&s, files).await?,
    }))
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
    pub kind: Option<String>,
    /// Restrict to a directory subtree.
    pub dir: Option<String>,
    pub limit: Option<i64>,
}

#[utoipa::path(
    get,
    path = "/projects/{pid}/assets/search",
    params(
        ("pid" = Uuid, Path, description = "Project id"),
        ("q" = Option<String>, Query, description = "Free text over path, name, description and tags"),
        ("kind" = Option<String>, Query, description = "image | video | audio | other"),
        ("dir" = Option<String>, Query, description = "Directory subtree"),
        ("limit" = Option<i64>, Query, description = "Max assets (default 20, max 100)")
    ),
    responses((status = 200, description = "Matching assets (best first) and directories", body = SearchResults))
)]
pub async fn search_assets(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
    Query(q): Query<SearchQuery>,
) -> AppResult<Json<SearchResults>> {
    access::ensure_project_owner(&s.db, &user.id, pid).await?;
    let kind = parse_kind(q.kind.as_deref())?;
    let dir = match q.dir.as_deref() {
        Some(d) => Some(assets::normalize_path(d).map_err(AppError::Invalid)?),
        None => None,
    };
    let mut res = assets::search(
        &s.db,
        pid,
        q.q.as_deref().unwrap_or(""),
        kind,
        dir.as_deref(),
        q.limit.unwrap_or(20),
    )
    .await?;
    res.assets = with_urls(&s, res.assets).await?;
    Ok(Json(res))
}

#[derive(Debug, Deserialize)]
pub struct ResolveQuery {
    /// `@path` or `path`.
    #[serde(rename = "ref")]
    pub reference: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ResolveRequest {
    /// `@path` references, e.g. `@"Screen recordings/demo.mp4"`.
    pub refs: Vec<String>,
}

#[utoipa::path(
    get,
    path = "/projects/{pid}/assets/resolve",
    params(
        ("pid" = Uuid, Path, description = "Project id"),
        ("ref" = String, Query, description = "`@path` reference")
    ),
    responses((status = 200, description = "Resolved reference (asset, directory with its files, or missing)", body = ResolvedRef))
)]
pub async fn resolve_one(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
    Query(q): Query<ResolveQuery>,
) -> AppResult<Json<ResolvedRef>> {
    access::ensure_project_owner(&s.db, &user.id, pid).await?;
    let mut r = assets::resolve_ref(&s.db, pid, &q.reference).await?;
    attach_resolved_urls(&s, &mut r).await?;
    Ok(Json(r))
}

#[utoipa::path(
    post,
    path = "/projects/{pid}/assets/resolve",
    params(("pid" = Uuid, Path, description = "Project id")),
    request_body = ResolveRequest,
    responses((status = 200, description = "One result per reference, same order", body = [ResolvedRef]))
)]
pub async fn resolve_many(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
    Json(body): Json<ResolveRequest>,
) -> AppResult<Json<Vec<ResolvedRef>>> {
    access::ensure_project_owner(&s.db, &user.id, pid).await?;
    if body.refs.len() > 100 {
        return Err(AppError::Invalid("at most 100 refs per request".into()));
    }
    let mut out = Vec::with_capacity(body.refs.len());
    for r in &body.refs {
        let mut resolved = assets::resolve_ref(&s.db, pid, r).await?;
        attach_resolved_urls(&s, &mut resolved).await?;
        out.push(resolved);
    }
    Ok(Json(out))
}

async fn attach_resolved_urls(s: &AppState, r: &mut ResolvedRef) -> AppResult<()> {
    if let Some(a) = r.asset.take() {
        r.asset = Some(one_with_url(s, a).await?);
    }
    if !r.children.is_empty() {
        let children = std::mem::take(&mut r.children);
        r.children = with_urls(s, children).await?;
    }
    Ok(())
}

// ── Upload flow ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
pub struct UploadRequest {
    /// MIME type the client will send, e.g. `video/mp4`.
    pub mime: String,
    /// Optional size hint in bytes; rejected when above the limit.
    pub bytes: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct UploadTicket {
    pub upload_id: Uuid,
    pub blob_path: String,
    pub upload_url: String,
    /// Always `PUT`.
    pub method: String,
    /// Headers the client must send with the `PUT`.
    pub headers: BTreeMap<String, String>,
    pub expires_at: DateTime<Utc>,
}

fn validate_mime(mime: &str) -> AppResult<String> {
    let m = mime.trim().to_ascii_lowercase();
    let ok = m.len() <= 100
        && m.split_once('/').is_some_and(|(t, sub)| {
            !t.is_empty()
                && !sub.is_empty()
                && m.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'+' | b'-'))
        });
    if !ok {
        return Err(AppError::Invalid(format!("invalid mime type: {mime}")));
    }
    Ok(m)
}

#[utoipa::path(
    post,
    path = "/projects/{pid}/assets/uploads",
    params(("pid" = Uuid, Path, description = "Project id")),
    request_body = UploadRequest,
    responses((status = 201, description = "Signed upload target", body = UploadTicket))
)]
pub async fn request_upload(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
    Json(body): Json<UploadRequest>,
) -> AppResult<(StatusCode, Json<UploadTicket>)> {
    access::ensure_project_owner(&s.db, &user.id, pid).await?;
    let mime = validate_mime(&body.mime)?;
    if let Some(b) = body.bytes
        && (b <= 0 || b as u64 > MAX_ASSET_BYTES)
    {
        return Err(AppError::Invalid(format!(
            "file size must be between 1 byte and {MAX_ASSET_BYTES} bytes"
        )));
    }
    let upload_id = new_id();
    let blob_path = assets::upload_blob_path(pid, upload_id);
    let ttl = Duration::minutes(UPLOAD_TTL_MINUTES);
    let url = s
        .blob
        .upload_url(&blob_path, &mime, ttl)
        .await
        .map_err(AppError::Other)?;
    let mut headers = BTreeMap::new();
    headers.insert("Content-Type".to_string(), mime);
    if s.local_blob.is_none() {
        headers.insert("x-ms-blob-type".to_string(), "BlockBlob".to_string());
    }
    Ok((
        StatusCode::CREATED,
        Json(UploadTicket {
            upload_id,
            blob_path,
            upload_url: url.to_string(),
            method: "PUT".into(),
            headers,
            expires_at: Utc::now() + ttl,
        }),
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct FinalizeRequest {
    pub upload_id: Uuid,
    /// Directory path the file goes into (must exist).
    pub dir: String,
    /// File name including extension.
    pub name: String,
    pub mime: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub duration_ms: Option<i32>,
    /// Hex sha256 of the bytes. Required for files above 64 MB.
    pub sha256: Option<String>,
    pub transcript: Option<Value>,
    /// When the description is empty and the file is a small image, describe it with the
    /// vision model (default true).
    pub auto_describe: Option<bool>,
}

#[utoipa::path(
    post,
    path = "/projects/{pid}/assets",
    params(("pid" = Uuid, Path, description = "Project id")),
    request_body = FinalizeRequest,
    responses(
        (status = 201, description = "Asset registered", body = Asset),
        (status = 409, description = "Upload missing or path taken")
    )
)]
pub async fn finalize_upload(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
    Json(body): Json<FinalizeRequest>,
) -> AppResult<(StatusCode, Json<Asset>)> {
    access::ensure_project_owner(&s.db, &user.id, pid).await?;
    let mime = validate_mime(&body.mime)?;
    let blob_path = assets::upload_blob_path(pid, body.upload_id);

    // Idempotent: finalizing the same upload twice returns the existing asset.
    if let Some(existing) = sqlx::query_as::<_, Asset>(sqlx::AssertSqlSafe(format!(
        "select {} from assets where project_id = $1 and blob_path = $2",
        assets::ASSET_COLUMNS
    )))
    .bind(pid)
    .bind(&blob_path)
    .fetch_optional(&s.db)
    .await?
    {
        return Ok((StatusCode::OK, Json(one_with_url(&s, existing).await?)));
    }

    let dir = assets::normalize_path(&body.dir).map_err(AppError::Invalid)?;
    if assets::find_dir_pool(&s.db, pid, &dir).await?.is_none() {
        return Err(AppError::Invalid(format!(
            "directory \"{dir}\" does not exist"
        )));
    }

    let size = s
        .blob
        .size(&blob_path)
        .await
        .map_err(AppError::Other)?
        .filter(|n| *n > 0)
        .ok_or_else(|| AppError::Conflict("upload not found; PUT the bytes first".into()))?;
    if size > MAX_ASSET_BYTES {
        let _ = s.blob.delete(&blob_path).await;
        return Err(AppError::Invalid(format!(
            "file exceeds the {MAX_ASSET_BYTES} byte limit"
        )));
    }

    let kind = AssetKind::from_mime(&mime);
    let sha256 = match body
        .sha256
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        Some(h) => {
            let h = h.to_ascii_lowercase();
            if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(AppError::Invalid("sha256 must be 64 hex characters".into()));
            }
            h
        }
        None if size <= MAX_SERVER_HASH_BYTES => {
            let bytes = s.blob.get(&blob_path).await.map_err(AppError::Other)?;
            hex::encode(Sha256::digest(&bytes))
        }
        None => {
            return Err(AppError::Invalid(
                "sha256 is required for files above 64 MB".into(),
            ));
        }
    };

    let mut description = body.description.trim().to_owned();
    let mut tags = body.tags.clone();
    let (mut width, mut height) = (body.width, body.height);
    if kind == AssetKind::Image
        && size <= MAX_DESCRIBE_BYTES
        && (width.is_none() || (description.is_empty() && body.auto_describe != Some(false)))
        && let Ok(bytes) = s.blob.get(&blob_path).await
    {
        if width.is_none()
            && let Ok(img) = image::load_from_memory(&bytes)
        {
            width = Some(img.width() as i32);
            height = Some(img.height() as i32);
        }
        if description.is_empty()
            && body.auto_describe != Some(false)
            && let Some(d) =
                vision::describe_bytes(&s.ai, &bytes, &format!("Uploaded file {}", body.name)).await
        {
            description = d.description;
            tags.extend(d.tags);
        }
    }

    let mut conn = s.db.acquire().await?;
    let asset = assets::insert_asset(
        &mut conn,
        NewAsset {
            id: new_id(),
            project_id: pid,
            dir_path: dir,
            name: body.name,
            kind,
            blob_path,
            mime,
            width,
            height,
            duration_ms: body.duration_ms,
            sha256,
            description,
            tags,
            transcript: body.transcript,
            bytes: Some(size as i64),
        },
        false,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(one_with_url(&s, asset).await?)))
}

/// Store raw bytes as a new asset (onboarding, server-generated media). Name collisions
/// get a numeric suffix. Images are described by the vision model when no description is given.
#[allow(clippy::too_many_arguments)]
pub async fn store_bytes(
    s: &AppState,
    conn: &mut sqlx::PgConnection,
    project_id: Uuid,
    dir_path: &str,
    name: &str,
    mime: &str,
    bytes: Vec<u8>,
    description: Option<String>,
    tags: Vec<String>,
    describe_context: &str,
) -> AppResult<Asset> {
    let id = new_id();
    let blob_path = assets::upload_blob_path(project_id, id);
    let kind = AssetKind::from_mime(mime);
    let sha256 = hex::encode(Sha256::digest(&bytes));
    let (mut width, mut height) = (None, None);
    let mut description = description.unwrap_or_default();
    let mut tags = tags;
    if kind == AssetKind::Image {
        if let Ok(img) = image::load_from_memory(&bytes) {
            width = Some(img.width() as i32);
            height = Some(img.height() as i32);
        }
        if description.trim().is_empty()
            && let Some(d) = vision::describe_bytes(&s.ai, &bytes, describe_context).await
        {
            description = d.description;
            tags.extend(d.tags);
        }
    }
    let size = bytes.len() as i64;
    s.blob
        .put(&blob_path, bytes, mime)
        .await
        .map_err(AppError::Other)?;
    assets::insert_asset(
        conn,
        NewAsset {
            id,
            project_id,
            dir_path: dir_path.to_owned(),
            name: name.to_owned(),
            kind,
            blob_path,
            mime: mime.to_owned(),
            width,
            height,
            duration_ms: None,
            sha256,
            description,
            tags,
            transcript: None,
            bytes: Some(size),
        },
        true,
    )
    .await
}

// ── Single asset ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
pub struct PatchAssetRequest {
    /// Rename.
    pub name: Option<String>,
    /// Move to this directory path.
    pub dir: Option<String>,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
}

#[utoipa::path(
    get,
    path = "/assets/{id}",
    params(("id" = Uuid, Path, description = "Asset id")),
    responses((status = 200, description = "Asset with a short-lived read URL", body = Asset), (status = 404, description = "Not found"))
)]
pub async fn get_asset(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Asset>> {
    access::ensure_asset_owner(&s.db, &user.id, id).await?;
    let a = assets::get_asset(&s.db, id).await?;
    Ok(Json(one_with_url(&s, a).await?))
}

#[utoipa::path(
    patch,
    path = "/assets/{id}",
    params(("id" = Uuid, Path, description = "Asset id")),
    request_body = PatchAssetRequest,
    responses((status = 200, description = "Updated", body = Asset), (status = 409, description = "Target path taken"))
)]
pub async fn patch_asset(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<PatchAssetRequest>,
) -> AppResult<Json<Asset>> {
    access::ensure_asset_owner(&s.db, &user.id, id).await?;
    let a = assets::update_asset(
        &s.db,
        id,
        AssetPatch {
            name: body.name,
            dir_path: body.dir,
            description: body.description,
            tags: body.tags,
        },
    )
    .await?;
    Ok(Json(one_with_url(&s, a).await?))
}

#[utoipa::path(
    delete,
    path = "/assets/{id}",
    params(("id" = Uuid, Path, description = "Asset id")),
    responses((status = 204, description = "Deleted"))
)]
pub async fn delete_asset(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    access::ensure_asset_owner(&s.db, &user.id, id).await?;
    let blob = assets::delete_asset(&s.db, id).await?;
    // Shared library blobs are owned by audio_library, not by any one project asset.
    let keep = crate::domain::audio_library::is_library_blob(&blob)
        || crate::domain::audio_library::blob_still_referenced(&s.db, &blob).await?;
    if !keep && let Err(e) = s.blob.delete(&blob).await {
        tracing::warn!(error = %e, %blob, "asset blob delete failed");
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_validation() {
        assert_eq!(validate_mime("Video/MP4").unwrap(), "video/mp4");
        assert!(validate_mime("video").is_err());
        assert!(validate_mime("video/mp4; codecs=avc1").is_err());
    }
}
