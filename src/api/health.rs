use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[utoipa::path(
    get,
    path = "/readyz",
    responses((status = 200, description = "Ready"), (status = 503, description = "Not ready"))
)]
pub async fn ready(State(s): State<AppState>) -> Result<(StatusCode, Json<Value>), AppError> {
    sqlx::query_scalar::<_, i32>("select 1")
        .fetch_one(&s.db)
        .await
        .map_err(|e| AppError::Other(e.into()))?;
    Ok((StatusCode::OK, Json(json!({ "ok": true }))))
}

pub async fn healthz() -> &'static str {
    "ok"
}

#[allow(dead_code)]
pub async fn _unused() -> AppResult<()> {
    Ok(())
}
