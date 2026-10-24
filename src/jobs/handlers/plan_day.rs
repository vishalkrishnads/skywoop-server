//! `DraftPlan`: PlanDay agent writes the day's markdown plan (greeting + markdown).
//!
//! The markdown is validated with [`crate::plan_format`] (one repair pass), videos are synced
//! into `plan_videos`, and the plan lands in `review`. No assets are generated here.
//!
//! When earlier posts are still gaining views, the agent may instead return `no_op` and the
//! plan lands in `no_op` with the agent's reason and explanation.

use chrono::{NaiveDate, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::agent::{AgentCtx, AgentTool, ToolLoopLimits, run_tool_loop};
use crate::ai::Effort;
use crate::ai::client::TextRole;
use crate::ai::prompt;
use crate::api::budget::{self, VideoCounts};
use crate::domain::Platform;
use crate::domain::assets;
use crate::domain::plan::{
    self, PlanDayOut, PlanStatus, PlanTrack, ProjectAudience, SyncCtx, insert_plan_version,
};
use crate::jobs::JobError;
use crate::plan_format::{self, ParsedPlan, ValidateOpts};
use crate::state::AppState;
use crate::trends;

/// Statuses from which PlanDay may (re)draft.
fn can_draft(status: &str) -> bool {
    matches!(status, "waiting_for_data" | "drafting" | "failed")
}

/// Split joined video shots and strip em/en dashes before validation, so the
/// stored plan is what the operator sees.
fn normalize_plan_text(out: &mut PlanDayOut) {
    out.markdown = plan_format::strip_typography_dashes(&plan_format::split_compound_video_lines(
        &out.markdown,
    ));
    out.greeting = plan_format::strip_typography_dashes(&out.greeting);
}

pub async fn run(
    state: &AppState,
    project_id: Uuid,
    platform: Platform,
    for_date: NaiveDate,
    allow_no_op: bool,
) -> Result<(), JobError> {
    let retry = |e: anyhow::Error| JobError::Retry(e);
    let plan = plan::ensure_plan(
        &state.db,
        project_id,
        platform,
        for_date,
        PlanStatus::WaitingForData,
    )
    .await
    .map_err(retry)?;

    if !can_draft(&plan.status) {
        tracing::info!(plan_id = %plan.id, status = %plan.status, "plan_day skip; plan already past drafting");
        return Ok(());
    }

    let audiences = plan::list_active_audiences(&state.db, project_id)
        .await
        .map_err(retry)?;
    if audiences.is_empty() {
        tracing::info!(plan_id = %plan.id, "plan_day: no audiences yet; staying in waiting_for_data");
        return Ok(());
    }

    plan::set_plan_status(&state.db, plan.id, PlanStatus::Drafting)
        .await
        .map_err(retry)?;

    match draft(
        state,
        &plan,
        &audiences,
        project_id,
        platform,
        for_date,
        allow_no_op,
    )
    .await
    {
        Ok(()) => Ok(()),
        Err(e) => {
            // Leave the plan retryable (`failed`) instead of stuck in `drafting`.
            let _ = plan::set_plan_status(&state.db, plan.id, PlanStatus::Failed).await;
            Err(e)
        }
    }
}

async fn draft(
    state: &AppState,
    plan: &plan::DailyPlan,
    audiences: &[ProjectAudience],
    project_id: Uuid,
    platform: Platform,
    for_date: NaiveDate,
    allow_no_op: bool,
) -> Result<(), JobError> {
    let (project_name, project_description, market_country, timezone): (
        String,
        String,
        String,
        String,
    ) = sqlx::query_as(
        "select name, description, market_country, timezone from projects where id = $1",
    )
    .bind(project_id)
    .fetch_one(&state.db)
    .await
    .map_err(|e| JobError::Retry(e.into()))?;

    // Warm today's trend cache so get_trends has something to return.
    let trend_items = trends::ensure_trend_snapshot(
        &state.db,
        state.ai.as_ref(),
        project_id,
        platform.as_str(),
        for_date,
        &market_country,
    )
    .await
    .unwrap_or_else(|e| {
        tracing::warn!(error = %e, "trend snapshot failed; continuing with partial trends");
        json!([])
    });

    let counts = budget::video_counts(&state.db, project_id)
        .await
        .map_err(|e| JobError::Retry(anyhow::anyhow!(e)))?;

    let analysis_date = trends::local_date_for_tz(&timezone, Utc::now());

    let no_op_metrics = plan::no_op_metrics(&state.db, project_id, platform.as_str(), Utc::now())
        .await
        .map_err(JobError::Retry)?;
    let no_op_allowed = allow_no_op && no_op_metrics.allows_no_op();

    let mut tool_box = crate::agent::tools::analytics_tools();
    tool_box.extend(crate::agent::tools::plan_tools());
    let tools: Vec<&dyn AgentTool> = tool_box.iter().map(|t| t.as_ref()).collect();
    let ctx = AgentCtx {
        project_id,
        platform,
        plan_id: Some(plan.id),
        db: state.db.clone(),
        state: Some(state.clone()),
    };

    let inputs = PromptInputs {
        project_name,
        project_description,
        audiences: pretty(audiences)?,
        for_date,
        analysis_date,
        counts,
        no_op_allowed,
        no_op_metrics: pretty(&no_op_metrics)?,
    };

    let mut out = run_plan_agent(state, &tools, &ctx, &inputs, None).await?;
    normalize_plan_text(&mut out);
    let parsed = match check_plan(
        &state.db,
        project_id,
        &out,
        audiences,
        counts,
        no_op_allowed,
    )
    .await
    {
        Ok(p) => p,
        Err(errs) => {
            let feedback = format_errors(&errs);
            tracing::info!(plan_id = %plan.id, errors = errs.len(), "plan_day repair pass");
            out = run_plan_agent(
                state,
                &tools,
                &ctx,
                &inputs,
                Some((&out.markdown, &feedback)),
            )
            .await?;
            normalize_plan_text(&mut out);
            match check_plan(
                &state.db,
                project_id,
                &out,
                audiences,
                counts,
                no_op_allowed,
            )
            .await
            {
                Ok(p) => p,
                Err(errs2) => {
                    return Err(JobError::permanent(anyhow::anyhow!(
                        "plan validation failed: {}",
                        format_errors(&errs2)
                    )));
                }
            }
        }
    };

    persist(state, plan, &out, &parsed, audiences, &trend_items).await
}

struct PromptInputs {
    project_name: String,
    project_description: String,
    audiences: String,
    for_date: NaiveDate,
    analysis_date: NaiveDate,
    counts: VideoCounts,
    no_op_allowed: bool,
    no_op_metrics: String,
}

fn pretty<T: serde::Serialize + ?Sized>(v: &T) -> Result<String, JobError> {
    serde_json::to_string_pretty(v).map_err(|e| JobError::permanent(anyhow::Error::from(e)))
}

async fn run_plan_agent(
    state: &AppState,
    tools: &[&dyn AgentTool],
    ctx: &AgentCtx,
    inputs: &PromptInputs,
    repair: Option<(&str, &str)>,
) -> Result<PlanDayOut, JobError> {
    let (previous_markdown, validation_errors) = repair.unwrap_or(("", ""));
    let prompt = prompt::render(
        "plan_day",
        &json!({
            "project_name": inputs.project_name,
            "project_description": inputs.project_description,
            "audiences": inputs.audiences,
            "for_date": inputs.for_date.to_string(),
            "analysis_date": inputs.analysis_date.to_string(),
            "audience_videos": inputs.counts.audience,
            "trend_videos": inputs.counts.trend,
            "previous_markdown": previous_markdown,
            "validation_errors": validation_errors,
            "allow_no_op": inputs.no_op_allowed,
            "no_op_metrics": inputs.no_op_metrics,
        }),
    )
    .map_err(JobError::from_ai)?;

    let (out, _run_id) = run_tool_loop::<PlanDayOut>(
        state.ai.as_ref(),
        TextRole::Planner,
        "plan_day",
        &prompt,
        tools,
        ctx,
        Effort::High,
        ToolLoopLimits::default(),
    )
    .await
    .map_err(JobError::from_ai)?;
    Ok(out)
}

/// Structural validation + semantic checks (audiences resolve, referenced assets exist).
async fn check_plan(
    db: &sqlx::PgPool,
    project_id: Uuid,
    out: &PlanDayOut,
    audiences: &[ProjectAudience],
    counts: VideoCounts,
    no_op_allowed: bool,
) -> Result<ParsedPlan, Vec<String>> {
    let mut errs = no_op_errors(out, no_op_allowed);
    let opts = plan_opts(out.no_op.is_some(), counts);
    let parsed = match plan_format::parse_and_validate(&out.markdown, &opts) {
        Ok(p) => p,
        Err(issues) => {
            errs.extend(issues.iter().map(ToString::to_string));
            return Err(errs);
        }
    };

    for v in &parsed.videos {
        if v.track == Some(PlanTrack::Audience)
            && let Some(raw) = &v.audience
            && plan::resolve_audience(audiences, raw).is_none()
        {
            errs.push(format!(
                "[{}] unknown audience \"{raw}\"; use a position 1-{} or an exact audience name",
                v.anchor,
                audiences.len()
            ));
        }
    }

    let refs = plan_format::referenced_paths(&parsed);
    if !refs.is_empty() {
        let paths: Vec<String> = refs.iter().map(|(_, p)| p.clone()).collect();
        match assets::missing_paths(db, project_id, &paths).await {
            Ok(missing) => {
                for (key, p) in &refs {
                    let norm = assets::normalize_path(p).unwrap_or_else(|_| p.clone());
                    if missing.contains(&norm) {
                        errs.push(format!(
                            "[{key}] @{p} does not exist; only reference paths returned by search_assets/list_asset_dir, otherwise leave the checklist item unchecked without a path"
                        ));
                    }
                }
            }
            Err(e) => tracing::warn!(error = ?e, "asset reference check failed; skipping"),
        }
    }

    if errs.is_empty() {
        Ok(parsed)
    } else {
        Err(errs)
    }
}

/// PlanDay validation. A no-op day skips the video-count checks.
fn plan_opts(no_op: bool, counts: VideoCounts) -> ValidateOpts {
    ValidateOpts {
        audience_videos: (!no_op).then_some(counts.audience.max(0) as usize),
        max_trend_videos: (!no_op).then_some(counts.trend.max(0) as usize),
        require_caption: true,
        require_title: true,
        require_names: true,
        ..Default::default()
    }
}

fn no_op_errors(out: &PlanDayOut, no_op_allowed: bool) -> Vec<String> {
    let Some(no_op) = &out.no_op else {
        return vec![];
    };
    if !no_op_allowed {
        return vec![
            "no_op is not allowed today (no earlier posts are still gaining views); set no_op to null and write the full plan".into(),
        ];
    }
    let mut errs = Vec::new();
    if no_op.reason.trim().is_empty() {
        errs.push("no_op.reason must be a short snake_case reason".into());
    }
    if no_op.explanation.trim().is_empty() {
        errs.push("no_op.explanation must explain the decision with numbers".into());
    }
    errs
}

fn format_errors(errs: &[String]) -> String {
    errs.iter()
        .take(25)
        .map(|e| format!("- {e}"))
        .collect::<Vec<_>>()
        .join("\n")
}

async fn persist(
    state: &AppState,
    plan: &plan::DailyPlan,
    out: &PlanDayOut,
    parsed: &ParsedPlan,
    audiences: &[ProjectAudience],
    trend_items: &Value,
) -> Result<(), JobError> {
    let retry = |e: sqlx::Error| JobError::Retry(e.into());
    let mut tx = state.db.begin().await.map_err(retry)?;

    let (status, version): (String, i32) =
        sqlx::query_as("select status, version from daily_plans where id = $1 for update")
            .bind(plan.id)
            .fetch_one(&mut *tx)
            .await
            .map_err(retry)?;
    if !can_draft(&status) {
        // Someone else moved the plan on (e.g. user edited a previous draft) — keep their work.
        tracing::info!(plan_id = %plan.id, %status, "plan_day: plan changed during drafting; discarding");
        return Ok(());
    }
    let new_version = if plan.markdown.is_empty() {
        1
    } else {
        version + 1
    };

    let sync = plan::sync_videos(
        &mut tx,
        plan.id,
        parsed,
        &SyncCtx {
            audiences,
            trends: trend_items,
        },
    )
    .await
    .map_err(JobError::Retry)?;

    let (title, summary) = plan::title_and_summary(parsed);
    let status = if out.no_op.is_some() {
        PlanStatus::NoOp
    } else {
        PlanStatus::Review
    };
    sqlx::query(
        r#"
        update daily_plans
           set markdown = $2, greeting = $3, version = $4, status = $5,
               findings = $6, title = $7, summary = $8,
               no_op_reason = $9, no_op_explanation = $10, updated_at = now()
         where id = $1
        "#,
    )
    .bind(plan.id)
    .bind(&out.markdown)
    .bind(out.greeting.trim())
    .bind(new_version)
    .bind(status.as_str())
    .bind(json!({ "warnings": sync.warnings }))
    .bind(&title)
    .bind(&summary)
    .bind(out.no_op.as_ref().map(|n| n.reason.trim().to_owned()))
    .bind(out.no_op.as_ref().map(|n| n.explanation.trim().to_owned()))
    .execute(&mut *tx)
    .await
    .map_err(retry)?;

    insert_plan_version(
        &mut tx,
        plan.id,
        new_version,
        &out.markdown,
        out.greeting.trim(),
        "plan_day",
    )
    .await
    .map_err(JobError::Retry)?;

    tx.commit().await.map_err(retry)?;

    for b in &sync.orphaned_blobs {
        let _ = state.blob.delete(b).await;
    }
    tracing::info!(
        plan_id = %plan.id,
        videos = parsed.videos.len(),
        version = new_version,
        status = status.as_str(),
        "plan_day drafted"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_statuses() {
        assert!(can_draft("waiting_for_data"));
        assert!(can_draft("failed"));
        assert!(!can_draft("review"));
        assert!(!can_draft("scheduled"));
    }

    fn out(no_op: Option<(&str, &str)>) -> PlanDayOut {
        PlanDayOut {
            greeting: "Hi".into(),
            markdown: "# Resting day\nYesterday's reel is still climbing.\n".into(),
            no_op: no_op.map(|(reason, explanation)| plan::NoOpOut {
                reason: reason.into(),
                explanation: explanation.into(),
            }),
        }
    }

    #[test]
    fn no_op_gating_and_counts() {
        let counts = VideoCounts {
            audience: 1,
            trend: 0,
        };
        assert!(no_op_errors(&out(None), false).is_empty());
        assert!(no_op_errors(&out(Some(("recent_posts_growing", "Up 4k views"))), true).is_empty());
        assert_eq!(
            no_op_errors(&out(Some(("recent_posts_growing", "Up 4k views"))), false).len(),
            1
        );
        assert_eq!(no_op_errors(&out(Some((" ", ""))), true).len(), 2);

        // A no-op plan with no videos passes; a normal plan without videos does not.
        let no_op = out(Some(("recent_posts_growing", "Up 4k views")));
        assert!(plan_format::parse_and_validate(&no_op.markdown, &plan_opts(true, counts)).is_ok());
        assert!(
            plan_format::parse_and_validate(&no_op.markdown, &plan_opts(false, counts)).is_err()
        );
    }

    #[test]
    fn error_feedback_is_bulleted_and_capped() {
        let errs: Vec<String> = (0..40).map(|i| format!("e{i}")).collect();
        let s = format_errors(&errs);
        assert_eq!(s.lines().count(), 25);
        assert!(s.starts_with("- e0"));
    }
}
