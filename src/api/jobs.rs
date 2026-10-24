use axum::Json;
use axum::extract::{Path, State};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::domain::access;
use crate::error::AppResult;
use crate::state::AppState;

#[derive(Debug, Serialize, FromRow, ToSchema)]
pub struct JobView {
    pub id: Uuid,
    pub kind: String,
    pub payload: Value,
    pub status: String,
    pub attempts: i32,
    pub max_attempts: i32,
    pub last_error: Option<String>,
    pub result: Option<Value>,
    pub run_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[utoipa::path(
    get,
    path = "/jobs/{id}",
    params(("id" = Uuid, Path, description = "Job id")),
    responses((status = 200, description = "Job", body = JobView), (status = 404, description = "Not found"))
)]
pub async fn get(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<JobView>> {
    access::ensure_job_owner(&s.db, &user.id, id).await?;
    let job = sqlx::query_as::<_, JobView>(
        r#"
        select id, kind, payload, status, attempts, max_attempts, last_error, result,
               run_at, created_at, finished_at
          from jobs
         where id = $1
        "#,
    )
    .bind(id)
    .fetch_one(&s.db)
    .await?;
    Ok(Json(job))
}
