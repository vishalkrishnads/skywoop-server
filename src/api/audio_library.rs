//! Shared audio library: search and ingest royalty-free music / sound effects.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::domain::access;
use crate::domain::assets::{self, Asset};
use crate::domain::audio_library::{
    self, AudioLibraryEntry, AudioLibraryKind, NewLibraryEntry, library_blob_path,
};
use crate::domain::new_id;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

fn one_with_url(
    s: &AppState,
    mut a: Asset,
) -> impl std::future::Future<Output = AppResult<Asset>> + '_ {
    async move {
        assets::attach_read_urls(s.blob.as_ref(), std::slice::from_mut(&mut a))
            .await
            .map_err(AppError::Other)?;
        Ok(a)
    }
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    /// sound_effect | music
    pub kind: String,
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SearchResponse {
    pub entries: Vec<AudioLibraryEntry>,
}

#[utoipa::path(
    get,
    path = "/audio-library/search",
    params(
        ("kind" = String, Query, description = "sound_effect | music"),
        ("q" = Option<String>, Query, description = "Free-text query"),
        ("limit" = Option<i64>, Query, description = "Max results (1-25)")
    ),
    responses((status = 200, description = "Ranked library hits", body = SearchResponse))
)]
pub async fn search(
    State(s): State<AppState>,
    _user: AuthUser,
    Query(q): Query<SearchQuery>,
) -> AppResult<Json<SearchResponse>> {
    let kind = AudioLibraryKind::parse(&q.kind)
        .ok_or_else(|| AppError::Invalid("kind must be sound_effect or music".into()))?;
    let entries = audio_library::search(&s.db, kind, &q.q, q.limit.unwrap_or(10)).await?;
    Ok(Json(SearchResponse { entries }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AttachRequest {
    pub library_id: Uuid,
}

#[utoipa::path(
    post,
    path = "/projects/{pid}/audio-library/attach",
    params(("pid" = Uuid, Path, description = "Project id")),
    request_body = AttachRequest,
    responses(
        (status = 201, description = "Project asset pointing at the shared blob", body = Asset),
        (status = 404, description = "Library entry missing")
    )
)]
pub async fn attach(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
    Json(body): Json<AttachRequest>,
) -> AppResult<(StatusCode, Json<Asset>)> {
    access::ensure_project_ready(&s.db, &user.id, pid).await?;
    let entry = audio_library::get(&s.db, body.library_id).await?;
    let mut tx = s.db.begin().await?;
    let asset = audio_library::attach_to_project(&mut tx, pid, &entry).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(one_with_url(&s, asset).await?)))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct IngestRequest {
    /// sound_effect | music
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub duration_ms: Option<i32>,
    /// openverse | fal
    pub source: String,
    pub source_id: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub attribution: String,
    pub mime: String,
    /// Staging upload from `POST /projects/{pid}/assets/uploads`. The client PUTs
    /// the file straight to blob storage; this request stays a small JSON body.
    #[serde(default)]
    pub upload_id: Option<Uuid>,
    /// Optional. Only for tiny clips. Files past the request body limit must use `upload_id`.
    #[serde(default)]
    pub data_base64: Option<String>,
    /// Hex sha256 of the audio bytes. Checked when present.
    #[serde(default)]
    pub sha256: Option<String>,
}

const MAX_INGEST_BYTES: usize = 20 * 1024 * 1024;

#[utoipa::path(
    post,
    path = "/projects/{pid}/audio-library/ingest",
    params(("pid" = Uuid, Path, description = "Project id")),
    request_body = IngestRequest,
    responses(
        (status = 201, description = "Library row + project asset", body = Asset),
        (status = 409, description = "Source already cached; attach instead"),
        (status = 422, description = "Missing upload or audio that is not audio/*")
    )
)]
pub async fn ingest(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
    Json(body): Json<IngestRequest>,
) -> AppResult<(StatusCode, Json<Asset>)> {
    access::ensure_project_ready(&s.db, &user.id, pid).await?;
    let kind = AudioLibraryKind::parse(&body.kind)
        .ok_or_else(|| AppError::Invalid("kind must be sound_effect or music".into()))?;
    if body.source != "openverse" && body.source != "fal" {
        return Err(AppError::Invalid("source must be openverse or fal".into()));
    }
    if body.source_id.trim().is_empty() {
        return Err(AppError::Invalid("source_id is required".into()));
    }
    if body.title.trim().is_empty() {
        return Err(AppError::Invalid("title is required".into()));
    }
    let mime = body.mime.trim().to_ascii_lowercase();
    if !mime.starts_with("audio/") {
        return Err(AppError::Invalid("mime must be an audio/* type".into()));
    }

    if let Some(existing) =
        audio_library::find_by_source(&s.db, &body.source, body.source_id.trim()).await?
    {
        // The file is already cached. Drop a staging upload so it does not linger.
        if let Some(upload_id) = body.upload_id {
            let staging = assets::upload_blob_path(pid, upload_id);
            let _ = s.blob.delete(&staging).await;
        }
        let mut tx = s.db.begin().await?;
        let asset = audio_library::attach_to_project(&mut tx, pid, &existing).await?;
        tx.commit().await?;
        return Ok((StatusCode::CREATED, Json(one_with_url(&s, asset).await?)));
    }

    let (bytes, staging) = load_ingest_bytes(&s, pid, &body).await?;
    if let Some(claimed) = body
        .sha256
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
    {
        let claimed = claimed.to_ascii_lowercase();
        if claimed.len() != 64 || !claimed.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(AppError::Invalid("sha256 must be 64 hex characters".into()));
        }
        let got = hex::encode(Sha256::digest(&bytes));
        if claimed != got {
            if let Some(path) = staging.as_deref() {
                let _ = s.blob.delete(path).await;
            }
            return Err(AppError::Invalid(
                "sha256 does not match the uploaded audio".into(),
            ));
        }
    }

    let id = new_id();
    let blob_path = library_blob_path(id);
    let sha256 = hex::encode(Sha256::digest(&bytes));
    let size = bytes.len() as i64;
    s.blob
        .put(&blob_path, bytes, &mime)
        .await
        .map_err(AppError::Other)?;
    if let Some(path) = staging.as_deref() {
        let _ = s.blob.delete(path).await;
    }

    let mut tx = s.db.begin().await?;
    let entry = match audio_library::insert(
        &mut tx,
        id,
        &blob_path,
        NewLibraryEntry {
            kind,
            title: body.title,
            description: body.description,
            tags: body.tags,
            duration_ms: body.duration_ms,
            mime: mime.clone(),
            bytes: size,
            sha256,
            source: body.source,
            source_id: body.source_id,
            license: body.license,
            attribution: body.attribution,
        },
    )
    .await
    {
        Ok(e) => e,
        Err(e) => {
            let _ = tx.rollback().await;
            let _ = s.blob.delete(&blob_path).await;
            return Err(e);
        }
    };
    let asset = match audio_library::attach_to_project(&mut tx, pid, &entry).await {
        Ok(a) => a,
        Err(e) => {
            let _ = tx.rollback().await;
            let _ = s.blob.delete(&blob_path).await;
            return Err(e);
        }
    };
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(one_with_url(&s, asset).await?)))
}

/// Load audio bytes from a direct blob upload, or from a small base64 body.
/// Returns the staging blob path when the bytes came from `upload_id`.
async fn load_ingest_bytes(
    s: &AppState,
    pid: Uuid,
    body: &IngestRequest,
) -> AppResult<(Vec<u8>, Option<String>)> {
    if let Some(upload_id) = body.upload_id {
        let staging = assets::upload_blob_path(pid, upload_id);
        let size = s
            .blob
            .size(&staging)
            .await
            .map_err(AppError::Other)?
            .filter(|n| *n > 0)
            .ok_or_else(|| {
                AppError::Conflict("upload not found; PUT the audio to the upload URL first".into())
            })?;
        if size > MAX_INGEST_BYTES as u64 {
            let _ = s.blob.delete(&staging).await;
            return Err(AppError::Invalid(format!(
                "audio file too large (max {MAX_INGEST_BYTES} bytes)"
            )));
        }
        let bytes = s.blob.get(&staging).await.map_err(AppError::Other)?;
        if bytes.is_empty() {
            return Err(AppError::Invalid("audio bytes are empty".into()));
        }
        return Ok((bytes, Some(staging)));
    }

    let Some(b64) = body
        .data_base64
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Err(AppError::Invalid(
            "upload_id is required; PUT the audio to an upload URL first".into(),
        ));
    };
    let bytes = BASE64
        .decode(b64)
        .map_err(|_| AppError::Invalid("data_base64 is not valid base64".into()))?;
    if bytes.is_empty() {
        return Err(AppError::Invalid("audio bytes are empty".into()));
    }
    if bytes.len() > MAX_INGEST_BYTES {
        return Err(AppError::Invalid(format!(
            "audio file too large (max {MAX_INGEST_BYTES} bytes)"
        )));
    }
    Ok((bytes, None))
}
