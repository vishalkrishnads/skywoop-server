//! Video prompting guide: agent reads of the handwritten guide markdown.

use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::auth::AuthUser;
use crate::error::{AppError, AppResult};
use crate::video_guide_doc::{self, VideoGuidePart};

#[derive(Debug, Deserialize, ToSchema)]
pub struct ReadVideoGuideRequest {
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
pub struct ReadVideoGuideResponse {
    /// The requested slice only. The full guide file is not returned elsewhere.
    pub text: String,
}

#[utoipa::path(
    post,
    path = "/video-guide/read",
    request_body = ReadVideoGuideRequest,
    responses(
        (status = 200, description = "Video guide slice as plain text", body = ReadVideoGuideResponse),
        (status = 422, description = "Unknown part or missing heading/query")
    )
)]
pub async fn read_doc(
    _user: AuthUser,
    Json(body): Json<ReadVideoGuideRequest>,
) -> AppResult<Json<ReadVideoGuideResponse>> {
    let part = match body.part.as_deref() {
        None | Some("") => VideoGuidePart::Outline,
        Some(s) => VideoGuidePart::parse(s).ok_or_else(|| {
            AppError::Invalid(format!(
                "part must be outline, section, search, or all; got {s}"
            ))
        })?,
    };
    let text = video_guide_doc::read(part, body.heading.as_deref(), body.query.as_deref())
        .map_err(AppError::Invalid)?;
    Ok(Json(ReadVideoGuideResponse { text }))
}
