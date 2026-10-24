use axum::Json;
use axum::extract::{Path, State};
use chrono::NaiveDate;
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::domain::Platform;
use crate::domain::access;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::trends;

#[derive(Debug, Serialize, ToSchema)]
pub struct TrendsTodayView {
    pub project_id: Uuid,
    pub platform: String,
    pub country: String,
    #[schema(value_type = String, format = Date)]
    pub local_date: NaiveDate,
    /// Trend items: `{ id, title, summary, source_urls, source }`.
    pub items: Value,
}

#[utoipa::path(
    get,
    path = "/projects/{id}/{platform}/trends/today",
    params(
        ("id" = Uuid, Path, description = "Project id"),
        ("platform" = String, Path, description = "Platform (instagram|facebook)")
    ),
    responses(
        (status = 200, description = "Today's trend snapshot (fetched and cached on first call)", body = TrendsTodayView),
        (status = 404, description = "Project not found")
    )
)]
pub async fn today(
    State(s): State<AppState>,
    user: AuthUser,
    Path((id, platform)): Path<(Uuid, String)>,
) -> AppResult<Json<TrendsTodayView>> {
    access::ensure_project_ready(&s.db, &user.id, id).await?;
    let platform = Platform::parse(&platform)
        .ok_or_else(|| AppError::Invalid(format!("unknown platform: {platform}")))?;
    let (timezone, country): (String, String) =
        sqlx::query_as("select timezone, market_country from projects where id = $1")
            .bind(id)
            .fetch_one(&s.db)
            .await?;
    let local_date = trends::local_date_for_tz(&timezone, s.clock.now());
    let items = trends::ensure_trend_snapshot(
        &s.db,
        s.ai.as_ref(),
        id,
        platform.as_str(),
        local_date,
        &country,
    )
    .await
    .map_err(AppError::Other)?;
    Ok(Json(TrendsTodayView {
        project_id: id,
        platform: platform.as_str().to_string(),
        country,
        local_date,
        items,
    }))
}
