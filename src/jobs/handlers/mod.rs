mod analyze_experiment;
mod categorize_asset;
mod classify_industry;
mod daily_cycle;
mod discover_product;
mod evaluate_maturity;
mod plan_day;
mod review_posting_schedule;
mod update_diary;

use super::{DataJob, JobError, UserJob};
use crate::state::AppState;

pub async fn handle_user(state: &AppState, job: UserJob) -> Result<(), JobError> {
    match job {
        UserJob::PublishVideo { post_id, attempt } => {
            tracing::info!(%post_id, attempt, "PublishVideo");
            with_post_failure(state, post_id, publish_video(state, post_id, attempt)).await
        }
        UserJob::PollIgContainer { post_id, poll_no } => {
            tracing::info!(%post_id, poll_no, "PollIgContainer");
            with_post_failure(
                state,
                post_id,
                crate::meta::instagram::poll_ig_container(state, post_id, poll_no),
            )
            .await
        }
        UserJob::PollFbVideo { post_id, poll_no } => {
            tracing::info!(%post_id, poll_no, "PollFbVideo");
            with_post_failure(
                state,
                post_id,
                crate::meta::facebook::poll_fb_video(state, post_id, poll_no),
            )
            .await
        }
        UserJob::CleanupPostMedia { post_id } => {
            crate::meta::publish::cleanup_post_media(state, post_id)
                .await
                .map_err(JobError::Retry)
        }
        UserJob::DiscoverProduct { discovery_id } => {
            discover_product::run(state, discovery_id).await
        }
        UserJob::ClassifyIndustry { project_id } => classify_industry::run(state, project_id).await,
        UserJob::CategorizeAsset {
            asset_id,
            poster_asset_id,
        } => categorize_asset::run(state, asset_id, poster_asset_id).await,
    }
}

/// Run a publish step; a permanent failure marks the post (and plan) as failed.
async fn with_post_failure<F>(state: &AppState, post_id: uuid::Uuid, fut: F) -> Result<(), JobError>
where
    F: std::future::Future<Output = Result<(), JobError>>,
{
    let res = fut.await;
    if let Err(e) = &res
        && e.is_permanent()
    {
        crate::meta::publish::mark_post_failed(state, post_id, &format!("{e:#}")).await;
    }
    res
}

pub async fn handle_data(state: &AppState, job: DataJob) -> Result<(), JobError> {
    match job {
        DataJob::FetchPostInsights {
            experiment_id,
            snapshot_no,
        } => crate::meta::insights::fetch_post_insights(state, experiment_id, snapshot_no)
            .await
            .map(|_| ()),
        DataJob::FetchAccountInsights {
            project_id,
            platform,
            date,
        } => crate::meta::account_insights::fetch_account_insights(
            state,
            project_id,
            platform.as_str(),
            date,
        )
        .await
        .map(|_| ()),
        DataJob::AscDailyIngest { app_id, date: _ } => {
            crate::appstore::ingest::asc_daily_ingest(state, &app_id).await
        }
        DataJob::EvaluateMaturity { hour } => {
            evaluate_maturity::evaluate_maturity(state, hour).await
        }
        DataJob::AnalyzeExperiment { experiment_id } => {
            analyze_experiment::run(state, experiment_id).await
        }
        DataJob::DailyCycle {
            project_id,
            platform,
            local_date,
        } => daily_cycle::run(state, project_id, platform, local_date).await,
        DataJob::RefreshTrends {
            project_id,
            platform,
            local_date,
            country,
        } => {
            let _ = crate::trends::ensure_trend_snapshot(
                &state.db,
                state.ai.as_ref(),
                project_id,
                platform.as_str(),
                local_date,
                &country,
            )
            .await
            .map_err(JobError::Retry)?;
            Ok(())
        }
        DataJob::DraftPlan {
            project_id,
            platform,
            for_date,
            allow_no_op,
        } => plan_day::run(state, project_id, platform, for_date, allow_no_op).await,
        DataJob::ReviewPostingSchedule {
            project_id,
            platform,
        } => review_posting_schedule::run(state, project_id, platform).await,
    }
}

async fn publish_video(
    state: &AppState,
    post_id: uuid::Uuid,
    attempt: i32,
) -> Result<(), JobError> {
    let row: Option<(String, String, i32)> =
        sqlx::query_as("select platform, status, attempt from posts where id = $1")
            .bind(post_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| JobError::Retry(e.into()))?;

    let Some((platform, status, current_attempt)) = row else {
        return Err(JobError::permanent(anyhow::anyhow!("post not found")));
    };

    // A re-scheduled post carries a newer attempt; stale delayed jobs must not publish.
    if attempt != current_attempt {
        tracing::info!(%post_id, attempt, current_attempt, "skip publish; stale attempt");
        return Ok(());
    }
    // Cancelled / finished posts must not publish when their delayed job fires.
    if status == "failed" || status == "published" {
        tracing::info!(%post_id, %status, "skip publish; post not actionable");
        return Ok(());
    }
    // Transition scheduled → pending before platform publish.
    if status == "scheduled" {
        sqlx::query("update posts set status = 'pending' where id = $1 and status = 'scheduled'")
            .bind(post_id)
            .execute(&state.db)
            .await
            .map_err(|e| JobError::Retry(e.into()))?;
    }

    match platform.as_str() {
        "instagram" => crate::meta::instagram::publish_instagram(state, post_id).await,
        "facebook" => crate::meta::facebook::publish_facebook(state, post_id).await,
        other => Err(JobError::permanent(anyhow::anyhow!(
            "unknown post platform: {other}"
        ))),
    }
}
