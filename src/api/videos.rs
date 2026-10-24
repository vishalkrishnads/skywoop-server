//! Video projects (editable timelines), revision history/undo, and render upload.
//!
//! The browser editor owns rendering; the server stores the timeline JSON (edited through
//! revision-checked JSON Patch ops) and the final MP4 per `(plan_video, rev)`.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::api::plans::{require_editable, require_plan};
use crate::auth::AuthUser;
use crate::domain::access;
use crate::domain::assets;
use crate::domain::video::{
    self, ApplyResult, PatchOp, RevisionView, VideoProject, get_video_project,
};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

const RENDER_UPLOAD_TTL_MINUTES: i64 = 60;
const RENDER_READ_TTL_MINUTES: i64 = 60;
const MAX_RENDER_BYTES: i64 = 1024 * 1024 * 1024;

async fn ensure_plan_video_ready(
    db: &sqlx::PgPool,
    user_id: &str,
    plan_video_id: Uuid,
) -> AppResult<()> {
    let pid: Option<(Uuid,)> = sqlx::query_as(
        "select d.project_id from plan_videos pv \
         join daily_plans d on d.id = pv.plan_id where pv.id = $1",
    )
    .bind(plan_video_id)
    .fetch_optional(db)
    .await?;
    match pid {
        Some((project_id,)) => access::ensure_project_ready(db, user_id, project_id).await,
        None => Err(AppError::NotFound),
    }
}

/// The plan that owns a plan video; errors with 409 when the plan is read-only.
async fn editable_plan_for_video(
    s: &AppState,
    user_id: &str,
    plan_video_id: Uuid,
) -> AppResult<()> {
    ensure_plan_video_ready(&s.db, user_id, plan_video_id).await?;
    let plan_id: Uuid = sqlx::query_scalar("select plan_id from plan_videos where id = $1")
        .bind(plan_video_id)
        .fetch_one(&s.db)
        .await?;
    let plan = require_plan(&s.db, plan_id).await?;
    require_editable(&s.db, &plan).await
}

async fn editable_for_project(s: &AppState, user_id: &str, id: Uuid) -> AppResult<VideoProject> {
    access::ensure_video_project_owner(&s.db, user_id, id).await?;
    let vp = get_video_project(&s.db, id).await?;
    editable_plan_for_video(s, user_id, vp.plan_video_id).await?;
    Ok(vp)
}

// ── Create / read ────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct CreateVideoProjectRequest {
    /// Initial timeline; defaults to an empty 1080x1920 @ 30 fps timeline.
    #[schema(value_type = Option<Object>)]
    pub timeline: Option<Value>,
}

#[utoipa::path(
    post,
    path = "/plan-videos/{id}/video-project",
    params(("id" = Uuid, Path, description = "Plan video id")),
    request_body = CreateVideoProjectRequest,
    responses((status = 201, description = "Created (or existing) video project", body = VideoProject))
)]
pub async fn create_for_plan_video(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    body: Option<Json<CreateVideoProjectRequest>>,
) -> AppResult<(StatusCode, Json<VideoProject>)> {
    editable_plan_for_video(&s, &user.id, id).await?;
    let timeline = body.and_then(|Json(b)| b.timeline);
    let vp = video::create_video_project(&s.db, id, timeline).await?;
    Ok((StatusCode::CREATED, Json(vp)))
}

#[utoipa::path(
    get,
    path = "/plan-videos/{id}/video-project",
    params(("id" = Uuid, Path, description = "Plan video id")),
    responses((status = 200, description = "Video project", body = VideoProject), (status = 404, description = "None yet"))
)]
pub async fn get_for_plan_video(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<VideoProject>> {
    ensure_plan_video_ready(&s.db, &user.id, id).await?;
    video::get_for_plan_video(&s.db, id)
        .await?
        .map(Json)
        .ok_or(AppError::NotFound)
}

#[utoipa::path(
    get,
    path = "/video-projects/{id}",
    params(("id" = Uuid, Path, description = "Video project id")),
    responses((status = 200, description = "Video project with current timeline", body = VideoProject))
)]
pub async fn get_project(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<VideoProject>> {
    access::ensure_video_project_owner(&s.db, &user.id, id).await?;
    Ok(Json(get_video_project(&s.db, id).await?))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PutTimelineRequest {
    pub base_rev: i32,
    #[schema(value_type = Object)]
    pub timeline: Value,
    #[serde(default)]
    pub reason: String,
}

#[utoipa::path(
    put,
    path = "/video-projects/{id}",
    params(("id" = Uuid, Path, description = "Video project id")),
    request_body = PutTimelineRequest,
    responses(
        (status = 200, description = "Whole timeline replaced as a new revision", body = ApplyResult),
        (status = 409, description = "Stale base_rev or plan locked")
    )
)]
pub async fn put_project(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<PutTimelineRequest>,
) -> AppResult<Json<ApplyResult>> {
    editable_for_project(&s, &user.id, id).await?;
    video::validate_timeline(&body.timeline).map_err(AppError::Invalid)?;
    let reason = if body.reason.trim().is_empty() {
        "replace timeline"
    } else {
        body.reason.trim()
    };
    let op = PatchOp {
        op: "replace".into(),
        path: String::new(),
        from: None,
        value: Some(body.timeline),
    };
    Ok(Json(
        video::apply(&s.db, id, body.base_rev, vec![op], reason).await?,
    ))
}

#[utoipa::path(
    delete,
    path = "/video-projects/{id}",
    params(("id" = Uuid, Path, description = "Video project id")),
    responses((status = 204, description = "Deleted with its render"))
)]
pub async fn delete_project(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    let vp = editable_for_project(&s, &user.id, id).await?;
    sqlx::query("delete from video_projects where id = $1")
        .bind(id)
        .execute(&s.db)
        .await?;
    if let Some(b) = &vp.render_blob_path
        && let Err(e) = s.blob.delete(b).await
    {
        tracing::warn!(error = %e, blob = %b, "render blob delete failed");
    }
    Ok(StatusCode::NO_CONTENT)
}

// ── Edit ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
pub struct ApplyRequest {
    /// The revision the client's ops were computed against.
    pub base_rev: i32,
    /// RFC 6902 JSON Patch operations (add, remove, replace, move, copy, test).
    pub ops: Vec<PatchOp>,
    #[serde(default)]
    pub reason: String,
}

#[utoipa::path(
    post,
    path = "/video-projects/{id}/apply",
    params(("id" = Uuid, Path, description = "Video project id")),
    request_body = ApplyRequest,
    responses(
        (status = 200, description = "New revision with its inverse ops", body = ApplyResult),
        (status = 409, description = "Stale base_rev"),
        (status = 422, description = "An op failed; nothing was applied")
    )
)]
pub async fn apply(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<ApplyRequest>,
) -> AppResult<Json<ApplyResult>> {
    editable_for_project(&s, &user.id, id).await?;
    Ok(Json(
        video::apply(&s.db, id, body.base_rev, body.ops, body.reason.trim()).await?,
    ))
}

#[derive(Debug, Deserialize)]
pub struct RevisionsQuery {
    pub limit: Option<i64>,
}

#[utoipa::path(
    get,
    path = "/video-projects/{id}/revisions",
    params(
        ("id" = Uuid, Path, description = "Video project id"),
        ("limit" = Option<i64>, Query, description = "Max revisions (default 100)")
    ),
    responses((status = 200, description = "Revision history, newest first", body = [RevisionView]))
)]
pub async fn list_revisions(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Query(q): Query<RevisionsQuery>,
) -> AppResult<Json<Vec<RevisionView>>> {
    access::ensure_video_project_owner(&s.db, &user.id, id).await?;
    get_video_project(&s.db, id).await?;
    Ok(Json(
        video::list_revisions(&s.db, id, q.limit.unwrap_or(100)).await?,
    ))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RevisionTimeline {
    pub rev: i32,
    #[schema(value_type = Object)]
    pub timeline: Value,
}

#[utoipa::path(
    get,
    path = "/video-projects/{id}/revisions/{rev}",
    params(
        ("id" = Uuid, Path, description = "Video project id"),
        ("rev" = i32, Path, description = "Revision number")
    ),
    responses((status = 200, description = "Timeline as of that revision", body = RevisionTimeline))
)]
pub async fn get_revision(
    State(s): State<AppState>,
    user: AuthUser,
    Path((id, rev)): Path<(Uuid, i32)>,
) -> AppResult<Json<RevisionTimeline>> {
    access::ensure_video_project_owner(&s.db, &user.id, id).await?;
    let timeline = video::get_revision_timeline(&s.db, id, rev).await?;
    Ok(Json(RevisionTimeline { rev, timeline }))
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct UndoRequest {
    /// Revision to undo; defaults to the latest revision that is not an undo and not undone.
    pub rev: Option<i32>,
}

#[utoipa::path(
    post,
    path = "/video-projects/{id}/undo",
    params(("id" = Uuid, Path, description = "Video project id")),
    request_body = UndoRequest,
    responses(
        (status = 200, description = "Inverse ops applied as a new revision", body = ApplyResult),
        (status = 409, description = "Nothing to undo")
    )
)]
pub async fn undo(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    body: Option<Json<UndoRequest>>,
) -> AppResult<Json<ApplyResult>> {
    editable_for_project(&s, &user.id, id).await?;
    let rev = body.and_then(|Json(b)| b.rev);
    Ok(Json(video::undo(&s.db, id, rev).await?))
}

// ── Render ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
pub struct RenderRevRequest {
    /// Timeline revision that was rendered; must equal the current revision.
    pub rev: i32,
    /// Optional size hint (upload-url) or expected size (finalize).
    pub bytes: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RenderUploadTicket {
    pub rev: i32,
    pub blob_path: String,
    pub upload_url: String,
    /// Always `PUT`.
    pub method: String,
    pub headers: BTreeMap<String, String>,
    pub expires_at: DateTime<Utc>,
}

#[utoipa::path(
    post,
    path = "/video-projects/{id}/render/upload-url",
    params(("id" = Uuid, Path, description = "Video project id")),
    request_body = RenderRevRequest,
    responses(
        (status = 201, description = "Signed PUT target for renders/{plan_video_id}/{rev}.mp4", body = RenderUploadTicket),
        (status = 409, description = "rev is not the current revision")
    )
)]
pub async fn render_upload_url(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<RenderRevRequest>,
) -> AppResult<(StatusCode, Json<RenderUploadTicket>)> {
    let vp = editable_for_project(&s, &user.id, id).await?;
    if vp.rev != body.rev {
        return Err(AppError::Conflict(format!(
            "Timeline is at rev {}, not {}; render the latest revision.",
            vp.rev, body.rev
        )));
    }
    if let Some(b) = body.bytes
        && !(1..=MAX_RENDER_BYTES).contains(&b)
    {
        return Err(AppError::Invalid(format!(
            "render size must be between 1 byte and {MAX_RENDER_BYTES} bytes"
        )));
    }
    let blob_path = assets::render_blob_path(vp.plan_video_id, body.rev);
    let ttl = Duration::minutes(RENDER_UPLOAD_TTL_MINUTES);
    let url = s
        .blob
        .upload_url(&blob_path, "video/mp4", ttl)
        .await
        .map_err(AppError::Other)?;
    sqlx::query("update video_projects set status = 'building', updated_at = now() where id = $1 and rev = $2")
        .bind(id)
        .bind(body.rev)
        .execute(&s.db)
        .await?;
    let mut headers = BTreeMap::new();
    headers.insert("Content-Type".to_string(), "video/mp4".to_string());
    if s.local_blob.is_none() {
        headers.insert("x-ms-blob-type".to_string(), "BlockBlob".to_string());
    }
    Ok((
        StatusCode::CREATED,
        Json(RenderUploadTicket {
            rev: body.rev,
            blob_path,
            upload_url: url.to_string(),
            method: "PUT".into(),
            headers,
            expires_at: Utc::now() + ttl,
        }),
    ))
}

#[utoipa::path(
    post,
    path = "/video-projects/{id}/render/finalize",
    params(("id" = Uuid, Path, description = "Video project id")),
    request_body = RenderRevRequest,
    responses(
        (status = 200, description = "Render registered", body = VideoProject),
        (status = 409, description = "Upload missing or size mismatch")
    )
)]
pub async fn render_finalize(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<RenderRevRequest>,
) -> AppResult<Json<VideoProject>> {
    let vp = editable_for_project(&s, &user.id, id).await?;
    let blob_path = assets::render_blob_path(vp.plan_video_id, body.rev);
    let size = s
        .blob
        .size(&blob_path)
        .await
        .map_err(AppError::Other)?
        .filter(|n| *n > 0)
        .ok_or_else(|| AppError::Conflict("Render upload not found; PUT the file first.".into()))?;
    if let Some(expected) = body.bytes
        && expected as u64 != size
    {
        return Err(AppError::Conflict(format!(
            "Render size mismatch: uploaded {size} bytes, expected {expected}."
        )));
    }

    let mut tx = s.db.begin().await?;
    let current = sqlx::query_as::<_, VideoProject>(sqlx::AssertSqlSafe(format!(
        "select {} from video_projects where id = $1 for update",
        video::VP_COLUMNS
    )))
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    // An older revision's render never overwrites a newer one.
    if current.render_rev.is_some_and(|r| r > body.rev) {
        return Err(AppError::Conflict(
            "A newer render is already registered.".into(),
        ));
    }
    let is_current = current.rev == body.rev;
    let status = if is_current { "ready" } else { "draft" };
    let updated = sqlx::query_as::<_, VideoProject>(sqlx::AssertSqlSafe(format!(
        "update video_projects set render_blob_path = $2, render_rev = $3, \
                render_mime = 'video/mp4', render_bytes = $4, status = $5, rendered_at = now(), \
                updated_at = now() \
          where id = $1 returning {}",
        video::VP_COLUMNS
    )))
    .bind(id)
    .bind(&blob_path)
    .bind(body.rev)
    .bind(size as i64)
    .bind(status)
    .fetch_one(&mut *tx)
    .await?;

    // Auto-promote the plan once every video has a render for its current revision.
    let plan_id: Uuid = sqlx::query_scalar("select plan_id from plan_videos where id = $1")
        .bind(current.plan_video_id)
        .fetch_one(&mut *tx)
        .await?;
    let all_ready: bool = sqlx::query_scalar(
        "select not exists(select 1 from plan_videos pv \
           left join video_projects vp on vp.plan_video_id = pv.id \
          where pv.plan_id = $1 \
            and (vp.id is null or vp.render_rev is distinct from vp.rev \
                 or vp.render_blob_path is null))",
    )
    .bind(plan_id)
    .fetch_one(&mut *tx)
    .await?;
    if all_ready {
        sqlx::query(
            "update daily_plans set status = 'built', updated_at = now() \
              where id = $1 and status = 'building'",
        )
        .bind(plan_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    // The previous render (older rev) is now garbage.
    if let Some(old) = &current.render_blob_path
        && *old != blob_path
        && let Err(e) = s.blob.delete(old).await
    {
        tracing::warn!(error = %e, blob = %old, "old render delete failed");
    }
    Ok(Json(updated))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct RenderFailRequest {
    #[serde(default)]
    pub error: String,
}

#[utoipa::path(
    post,
    path = "/video-projects/{id}/render/fail",
    params(("id" = Uuid, Path, description = "Video project id")),
    request_body = RenderFailRequest,
    responses((status = 200, description = "Marked failed", body = VideoProject))
)]
pub async fn render_fail(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<RenderFailRequest>,
) -> AppResult<Json<VideoProject>> {
    editable_for_project(&s, &user.id, id).await?;
    tracing::warn!(video_project = %id, error = %body.error, "client render failed");
    let vp = sqlx::query_as::<_, VideoProject>(sqlx::AssertSqlSafe(format!(
        "update video_projects set status = 'failed', updated_at = now() where id = $1 \
         returning {}",
        video::VP_COLUMNS
    )))
    .bind(id)
    .fetch_one(&s.db)
    .await?;
    Ok(Json(vp))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RenderInfo {
    pub rev: i32,
    pub render_rev: Option<i32>,
    /// True when `render_rev == rev`.
    pub current: bool,
    pub mime: Option<String>,
    pub bytes: Option<i64>,
    /// Short-lived read URL for previewing the render. Absent after cleanup or before render.
    pub url: Option<String>,
}

#[utoipa::path(
    get,
    path = "/video-projects/{id}/render",
    params(("id" = Uuid, Path, description = "Video project id")),
    responses((status = 200, description = "Render metadata and read URL", body = RenderInfo))
)]
pub async fn get_render(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<RenderInfo>> {
    access::ensure_video_project_owner(&s.db, &user.id, id).await?;
    let vp = get_video_project(&s.db, id).await?;
    let url = match &vp.render_blob_path {
        Some(b) => Some(
            s.blob
                .public_read_url(b, Duration::minutes(RENDER_READ_TTL_MINUTES))
                .await
                .map_err(AppError::Other)?
                .to_string(),
        ),
        None => None,
    };
    Ok(Json(RenderInfo {
        rev: vp.rev,
        render_rev: vp.render_rev,
        current: vp.render_rev == Some(vp.rev),
        mime: vp.render_mime,
        bytes: vp.render_bytes,
        url,
    }))
}
