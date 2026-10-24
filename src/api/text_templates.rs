//! Shared text-template catalog used by PlanDay and the editor.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;

use crate::auth::AuthUser;
use crate::domain::text_template::{self, NewTextTemplate, TextTemplate};
use crate::error::AppResult;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    /// Optional match against name, description, id, and element names.
    pub q: Option<String>,
}

#[utoipa::path(
    get,
    path = "/text-templates",
    params(("q" = Option<String>, Query, description = "Optional search. Omit to list the catalog.")),
    responses((status = 200, description = "Text templates, seeds first", body = [TextTemplate]))
)]
pub async fn list(
    State(s): State<AppState>,
    _user: AuthUser,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Vec<TextTemplate>>> {
    Ok(Json(text_template::search(&s.db, q.q.as_deref()).await?))
}

#[utoipa::path(
    post,
    path = "/text-templates",
    request_body = NewTextTemplate,
    responses(
        (status = 200, description = "Created or replaced custom template", body = TextTemplate),
        (status = 409, description = "Seeded template id")
    )
)]
pub async fn upsert(
    State(s): State<AppState>,
    _user: AuthUser,
    Json(body): Json<NewTextTemplate>,
) -> AppResult<Json<TextTemplate>> {
    Ok(Json(text_template::upsert_custom(&s.db, body).await?))
}

#[utoipa::path(
    delete,
    path = "/text-templates/{id}",
    params(("id" = String, Path, description = "Template id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "Not found"),
        (status = 409, description = "Seeded template")
    )
)]
pub async fn delete(
    State(s): State<AppState>,
    _user: AuthUser,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    text_template::delete_custom(&s.db, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}
