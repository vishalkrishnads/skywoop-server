use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::client::{AiClient, AiError, Effort, ImageInput, TextRole};
use super::prompt;

/// Searchable description of an uploaded or crawled image.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AssetDescription {
    /// One to three plain sentences: what the image shows, visible text, colors, mood.
    pub description: String,
    /// Up to 8 short lowercase tags (e.g. `app-icon`, `screenshot`, `onboarding`, `dark-mode`).
    pub tags: Vec<String>,
}

/// Onboarding context upload: category + searchable description.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AssetCategorization {
    /// One of the allowed category strings from `categorize_asset` prompt.
    pub category: String,
    pub description: String,
    pub tags: Vec<String>,
}

/// Describe a crawled/uploaded image so it can be found with asset search.
///
/// `context` is a short hint such as "App Store screenshot 3 for <app>".
pub async fn describe(
    ai: &AiClient,
    image_jpeg: &[u8],
    context: &str,
) -> Result<AssetDescription, AiError> {
    let prompt = prompt::render("describe_asset", &json!({ "context": context }))?;
    let images = [ImageInput {
        mime: "image/jpeg".into(),
        bytes: image_jpeg.to_vec(),
    }];
    ai.structured(
        TextRole::Vision,
        "describe_asset",
        &prompt,
        &images,
        Effort::Low,
    )
    .await
}

/// Downscale (max 1024 px side) and re-encode any decodable image as JPEG for the vision model.
pub fn to_vision_jpeg(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    let img = image::load_from_memory(bytes)?;
    let img = if img.width().max(img.height()) > 1024 {
        img.resize(1024, 1024, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgb = img.to_rgb8();
    let mut out = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85);
    enc.encode_image(&rgb)?;
    Ok(out)
}

/// Categorize an onboarding upload (image or video poster) into a Context/ directory.
pub async fn categorize(
    ai: &AiClient,
    image_jpeg: &[u8],
    context: &str,
    filename: &str,
    notes: &str,
) -> Result<AssetCategorization, AiError> {
    let prompt = prompt::render(
        "categorize_asset",
        &json!({
            "context": context,
            "filename": filename,
            "notes": notes,
        }),
    )?;
    let images = [ImageInput {
        mime: "image/jpeg".into(),
        bytes: image_jpeg.to_vec(),
    }];
    ai.structured(
        TextRole::Vision,
        "categorize_asset",
        &prompt,
        &images,
        Effort::Low,
    )
    .await
}

/// Best-effort description of raw image bytes; `None` when the image cannot be decoded or the
/// model call fails (callers treat descriptions as optional).
pub async fn describe_bytes(
    ai: &AiClient,
    bytes: &[u8],
    context: &str,
) -> Option<AssetDescription> {
    let jpeg = match to_vision_jpeg(bytes) {
        Ok(j) => j,
        Err(e) => {
            tracing::debug!(error = %e, "image not decodable for description");
            return None;
        }
    };
    match describe(ai, &jpeg, context).await {
        Ok(d) => Some(d),
        Err(e) => {
            tracing::warn!(error = %e, "asset description failed");
            None
        }
    }
}
