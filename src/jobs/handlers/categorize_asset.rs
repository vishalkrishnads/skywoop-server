//! Move an Inbox upload into `Context/<Category>/` using vision classification.

use uuid::Uuid;

use crate::ai::vision;
use crate::domain::assets::{self, AssetPatch};
use crate::domain::onboarding::context_dir_for_category;
use crate::jobs::JobError;
use crate::state::AppState;

const ALLOWED: &[&str] = &[
    "Brand",
    "Product/Screenshots",
    "Product/Screen recordings",
    "People",
    "Lifestyle",
    "Testimonials",
    "Press",
    "Other",
];

pub async fn run(
    state: &AppState,
    asset_id: Uuid,
    poster_asset_id: Option<Uuid>,
) -> Result<(), JobError> {
    let asset = assets::get_asset(&state.db, asset_id)
        .await
        .map_err(|e| JobError::permanent(anyhow::anyhow!("{e}")))?;

    let notes: String = sqlx::query_scalar("select context_notes from projects where id = $1")
        .bind(asset.project_id)
        .fetch_one(&state.db)
        .await
        .map_err(|e| JobError::Retry(e.into()))?;

    let (category, description, tags) = if asset.kind == "video" && poster_asset_id.is_none() {
        (
            "Videos".to_string(),
            format!("User-uploaded video: {}", asset.name),
            vec!["video".into(), "context".into()],
        )
    } else {
        let image_bytes = if let Some(poster_id) = poster_asset_id {
            let poster = assets::get_asset(&state.db, poster_id)
                .await
                .map_err(|e| JobError::permanent(anyhow::anyhow!("{e}")))?;
            state
                .blob
                .get(&poster.blob_path)
                .await
                .map_err(JobError::Retry)?
        } else {
            state
                .blob
                .get(&asset.blob_path)
                .await
                .map_err(JobError::Retry)?
        };

        let jpeg = vision::to_vision_jpeg(&image_bytes).map_err(JobError::permanent)?;
        match vision::categorize(
            state.ai.as_ref(),
            &jpeg,
            "onboarding context upload",
            &asset.name,
            &notes,
        )
        .await
        {
            Ok(c) => {
                let cat = if ALLOWED.contains(&c.category.as_str()) {
                    c.category
                } else {
                    "Other".into()
                };
                (cat, c.description, c.tags)
            }
            Err(e) => {
                tracing::warn!(error = %e, %asset_id, "categorize failed; defaulting to Other");
                (
                    "Other".into(),
                    format!("User-uploaded context: {}", asset.name),
                    vec!["context".into()],
                )
            }
        }
    };

    let dest_dir = context_dir_for_category(&category);
    ensure_context_path(state, asset.project_id, dest_dir).await?;

    assets::update_asset(
        &state.db,
        asset_id,
        AssetPatch {
            name: None,
            dir_path: Some(dest_dir.to_string()),
            description: Some(description),
            tags: Some(tags),
        },
    )
    .await
    .map_err(|e| JobError::Retry(anyhow::anyhow!("{e}")))?;

    Ok(())
}

async fn ensure_context_path(
    state: &AppState,
    project_id: Uuid,
    path: &str,
) -> Result<(), JobError> {
    let mut built = String::new();
    for segment in path.split('/') {
        built = if built.is_empty() {
            segment.to_string()
        } else {
            format!("{built}/{segment}")
        };
        if assets::find_dir_pool(&state.db, project_id, &built)
            .await
            .map_err(|e| JobError::Retry(e.into()))?
            .is_some()
        {
            continue;
        }
        // Parent must exist for nested paths; create with a short description.
        let _ = assets::create_dir(
            &state.db,
            project_id,
            &built,
            &format!("Onboarding context: {built}"),
        )
        .await;
    }
    Ok(())
}
