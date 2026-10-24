//! Cross-project creative playbook: submit, list, review, and agent reads of the
//! handwritten playbook markdown (never the full file on other endpoints).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::domain::access;
use crate::domain::playbook::{self, NewEntry, PlaybookEntry, ReviewEdit};
use crate::error::{AppError, AppResult};
use crate::playbook_doc::{self, PlaybookPart};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    /// proposed | approved | rejected
    pub status: Option<String>,
    /// App category token, e.g. `productivity`.
    pub category: Option<String>,
    /// Substring match on title, body and tags.
    pub q: Option<String>,
    pub limit: Option<i64>,
}

#[utoipa::path(
    get,
    path = "/playbook",
    params(
        ("status" = Option<String>, Query, description = "proposed | approved | rejected"),
        ("category" = Option<String>, Query, description = "Only entries tagged with this app category"),
        ("q" = Option<String>, Query, description = "Search title, body, tags"),
        ("limit" = Option<i64>, Query, description = "Max entries (default 100)")
    ),
    responses((status = 200, description = "Playbook entries, newest first", body = [PlaybookEntry]))
)]
pub async fn list(
    State(s): State<AppState>,
    _user: AuthUser,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Vec<PlaybookEntry>>> {
    if let Some(st) = q.status.as_deref()
        && !playbook::STATUSES.contains(&st)
    {
        return Err(AppError::Invalid(format!("unknown status {st}")));
    }
    Ok(Json(
        playbook::list(
            &s.db,
            q.status.as_deref(),
            q.category.as_deref(),
            q.q.as_deref(),
            q.limit.unwrap_or(100),
        )
        .await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SubmitRequest {
    pub title: String,
    /// Markdown: when to use the pattern, structure, example hooks.
    pub body_md: String,
    #[serde(default)]
    pub app_categories: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Why we believe it works (metrics, experiment ids).
    #[serde(default)]
    pub evidence: String,
    pub source_project_id: Option<Uuid>,
}

#[utoipa::path(
    post,
    path = "/playbook",
    request_body = SubmitRequest,
    responses((status = 201, description = "Stored as `proposed`", body = PlaybookEntry))
)]
pub async fn submit(
    State(s): State<AppState>,
    user: AuthUser,
    Json(body): Json<SubmitRequest>,
) -> AppResult<(StatusCode, Json<PlaybookEntry>)> {
    if let Some(pid) = body.source_project_id {
        access::ensure_project_ready(&s.db, &user.id, pid).await?;
    }
    let entry = playbook::submit(
        &s.db,
        NewEntry {
            title: body.title,
            body_md: body.body_md,
            app_categories: body.app_categories,
            tags: body.tags,
            evidence: body.evidence,
            source_project_id: body.source_project_id,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(entry)))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ReadPlaybookRequest {
    /// outline (default) | section | search | all
    #[serde(default)]
    pub part: Option<String>,
    /// Heading title for part=section.
    #[serde(default)]
    pub heading: Option<String>,
    /// Free-text query for part=search.
    #[serde(default)]
    pub query: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ReadPlaybookResponse {
    /// The requested slice only. The full playbook file is not returned elsewhere.
    pub text: String,
}

#[utoipa::path(
    post,
    path = "/playbook/read",
    request_body = ReadPlaybookRequest,
    responses(
        (status = 200, description = "Playbook slice as plain text", body = ReadPlaybookResponse),
        (status = 422, description = "Unknown part or missing heading/query")
    )
)]
pub async fn read_doc(
    _user: AuthUser,
    Json(body): Json<ReadPlaybookRequest>,
) -> AppResult<Json<ReadPlaybookResponse>> {
    let part = match body.part.as_deref() {
        None | Some("") => PlaybookPart::Outline,
        Some(s) => PlaybookPart::parse(s).ok_or_else(|| {
            AppError::Invalid(format!(
                "part must be outline, section, search, or all; got {s}"
            ))
        })?,
    };
    let text = playbook_doc::read(part, body.heading.as_deref(), body.query.as_deref())
        .map_err(AppError::Invalid)?;
    Ok(Json(ReadPlaybookResponse { text }))
}

#[utoipa::path(
    get,
    path = "/playbook/{id}",
    params(("id" = Uuid, Path, description = "Entry id")),
    responses((status = 200, description = "Entry", body = PlaybookEntry), (status = 404, description = "Not found"))
)]
pub async fn get(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<PlaybookEntry>> {
    access::ensure_playbook_owner(&s.db, &user.id, id).await?;
    Ok(Json(playbook::get(&s.db, id).await?))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ReviewRequest {
    /// approved | rejected | proposed
    pub status: String,
    /// Optional edits applied together with the decision.
    pub title: Option<String>,
    pub body_md: Option<String>,
    pub app_categories: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
}

#[utoipa::path(
    post,
    path = "/playbook/{id}/review",
    params(("id" = Uuid, Path, description = "Entry id")),
    request_body = ReviewRequest,
    responses((status = 200, description = "Reviewed entry", body = PlaybookEntry))
)]
pub async fn review(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<ReviewRequest>,
) -> AppResult<Json<PlaybookEntry>> {
    access::ensure_playbook_owner(&s.db, &user.id, id).await?;
    Ok(Json(
        playbook::review(
            &s.db,
            id,
            &body.status,
            ReviewEdit {
                title: body.title,
                body_md: body.body_md,
                app_categories: body.app_categories,
                tags: body.tags,
            },
        )
        .await?,
    ))
}
