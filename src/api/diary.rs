use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::NaiveDate;
use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::domain::Platform;
use crate::domain::access;
use crate::domain::knowledge::{self, DiaryEntryView};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(Debug, Deserialize, ToSchema)]
pub struct DiaryQuery {
    pub platform: Option<String>,
    pub plan_video: Option<Uuid>,
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateDiaryRequest {
    pub platform: String,
    pub entry_date: NaiveDate,
    pub plan_video_id: Option<Uuid>,
    pub observation: String,
    pub views: Option<i64>,
    pub cta_clicks: Option<i64>,
    pub installs: Option<i64>,
    pub growth: Option<i64>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PatchDiaryRequest {
    pub observation: Option<String>,
    pub views: Option<Option<i64>>,
    pub cta_clicks: Option<Option<i64>>,
    pub installs: Option<Option<i64>>,
    pub growth: Option<Option<i64>>,
}

#[utoipa::path(
    get,
    path = "/projects/{pid}/diary",
    params(
        ("pid" = Uuid, Path, description = "Project id"),
        ("platform" = Option<String>, Query, description = "Filter platform"),
        ("plan_video" = Option<Uuid>, Query, description = "Filter by plan video"),
        ("from" = Option<NaiveDate>, Query, description = "From date"),
        ("to" = Option<NaiveDate>, Query, description = "To date")
    ),
    responses((status = 200, description = "Diary entries", body = Vec<DiaryEntryView>))
)]
pub async fn list(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
    Query(q): Query<DiaryQuery>,
) -> AppResult<Json<Vec<DiaryEntryView>>> {
    access::ensure_project_ready(&s.db, &user.id, pid).await?;
    let platform = match q.platform.as_deref() {
        Some(p) => Some(
            Platform::parse(p)
                .ok_or_else(|| AppError::Invalid(format!("unknown platform: {p}")))?,
        ),
        None => None,
    };
    Ok(Json(
        knowledge::list_diary(&s.db, pid, platform, q.plan_video, q.from, q.to).await?,
    ))
}

#[utoipa::path(
    post,
    path = "/projects/{pid}/diary",
    params(("pid" = Uuid, Path, description = "Project id")),
    request_body = CreateDiaryRequest,
    responses((status = 201, description = "Created", body = DiaryEntryView))
)]
pub async fn create(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
    Json(body): Json<CreateDiaryRequest>,
) -> Result<(axum::http::StatusCode, Json<DiaryEntryView>), AppError> {
    access::ensure_project_ready(&s.db, &user.id, pid).await?;
    let platform = Platform::parse(&body.platform)
        .ok_or_else(|| AppError::Invalid(format!("unknown platform: {}", body.platform)))?;
    let entry = knowledge::create_manual_diary(
        &s.db,
        pid,
        platform,
        body.entry_date,
        body.plan_video_id,
        &body.observation,
        body.views,
        body.cta_clicks,
        body.installs,
        body.growth,
    )
    .await?;
    Ok((axum::http::StatusCode::CREATED, Json(entry)))
}

#[utoipa::path(
    patch,
    path = "/diary/{id}",
    params(("id" = Uuid, Path, description = "Diary entry id")),
    request_body = PatchDiaryRequest,
    responses((status = 200, description = "Updated", body = DiaryEntryView))
)]
pub async fn patch(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<PatchDiaryRequest>,
) -> AppResult<Json<DiaryEntryView>> {
    access::ensure_diary_owner(&s.db, &user.id, id).await?;
    Ok(Json(
        knowledge::patch_diary_entry(
            &s.db,
            id,
            body.observation.as_deref(),
            body.views,
            body.cta_clicks,
            body.installs,
            body.growth,
        )
        .await?,
    ))
}
