use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::domain::Platform;
use crate::domain::access;
use crate::domain::knowledge::{self, HistoryDay};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(Debug, Deserialize, ToSchema)]
pub struct HistoryQuery {
    pub from: NaiveDate,
    pub to: NaiveDate,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct HistoryResponse {
    pub days: Vec<HistoryDay>,
}

#[utoipa::path(
    get,
    path = "/projects/{pid}/platforms/{p}/history",
    params(
        ("pid" = Uuid, Path, description = "Project id"),
        ("p" = String, Path, description = "Platform"),
        ("from" = NaiveDate, Query, description = "From date"),
        ("to" = NaiveDate, Query, description = "To date")
    ),
    responses((status = 200, description = "History by day", body = HistoryResponse))
)]
pub async fn get(
    State(s): State<AppState>,
    user: AuthUser,
    Path((pid, p)): Path<(Uuid, String)>,
    Query(q): Query<HistoryQuery>,
) -> AppResult<Json<HistoryResponse>> {
    access::ensure_project_ready(&s.db, &user.id, pid).await?;
    let platform =
        Platform::parse(&p).ok_or_else(|| AppError::Invalid(format!("unknown platform: {p}")))?;

    let exists: (bool,) = sqlx::query_as(
        r#"
        select exists(
            select 1 from platform_workspaces
             where project_id = $1 and platform = $2
        )
        "#,
    )
    .bind(pid)
    .bind(platform.as_str())
    .fetch_one(&s.db)
    .await?;
    if !exists.0 {
        return Err(AppError::NotFound);
    }

    let days = knowledge::history_by_day(&s.db, pid, platform, q.from, q.to).await?;
    Ok(Json(HistoryResponse { days }))
}
