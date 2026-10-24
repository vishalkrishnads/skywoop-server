use serde_json::json;
use uuid::Uuid;

use crate::ai::client::TextRole;
use crate::ai::{build_knowledge_pack, planner};
use crate::domain::Platform;
use crate::domain::new_id;
use crate::domain::playbook;
use crate::jobs::JobError;
use crate::plan_format;
use crate::state::AppState;

#[derive(sqlx::FromRow)]
struct ExpRow {
    id: Uuid,
    project_id: Uuid,
    platform: String,
    plan_video_id: Uuid,
    publish_local_date: chrono::NaiveDate,
    hypothesis: String,
    maturity: String,
    views: Option<i64>,
    reach: Option<i64>,
    interactions: Option<i64>,
    cta_clicks: Option<i64>,
    cta_clicks_unique: Option<i64>,
    profile_visits: Option<i64>,
    bio_link_taps: Option<i64>,
    installs: Option<i64>,
    est_installs: Option<i64>,
    cta_rate: Option<f64>,
    install_rate: Option<f64>,
    growth: Option<i64>,
    avg_watch_time_ms: Option<i64>,
    total_watch_time_ms: Option<i64>,
    skip_rate: Option<f64>,
    reels_play_count: Option<i64>,
    observation_diary_id: Option<Uuid>,
    audience_id: Option<Uuid>,
    plan_id: Uuid,
    anchor: String,
    title: String,
    track: String,
    daypart: Option<String>,
    caption: String,
    markdown: String,
}

pub async fn run(state: &AppState, experiment_id: Uuid) -> Result<(), JobError> {
    let row: ExpRow = sqlx::query_as(
        r#"
        select e.id, e.project_id, e.platform, e.plan_video_id,
               e.publish_local_date, e.hypothesis, e.maturity,
               e.views, e.reach, e.interactions, e.cta_clicks, e.cta_clicks_unique,
               e.profile_visits, e.bio_link_taps,
               e.installs, e.est_installs, e.cta_rate, e.install_rate, e.growth,
               e.avg_watch_time_ms, e.total_watch_time_ms, e.skip_rate, e.reels_play_count,
               e.observation_diary_id, e.audience_id,
               pv.plan_id, pv.anchor, pv.title, pv.track, pv.daypart, pv.caption,
               dp.markdown
          from experiments e
          join plan_videos pv on pv.id = e.plan_video_id
          join daily_plans dp on dp.id = pv.plan_id
         where e.id = $1
        "#,
    )
    .bind(experiment_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| JobError::Retry(e.into()))?
    .ok_or_else(|| JobError::permanent(anyhow::anyhow!("experiment not found")))?;

    if row.observation_diary_id.is_some() {
        tracing::info!(%experiment_id, "analyze_experiment already done");
        return Ok(());
    }

    if row.maturity != "mature" && row.maturity != "inconclusive" {
        return Err(JobError::Retry(anyhow::anyhow!(
            "experiment {experiment_id} not ready for analysis (maturity={})",
            row.maturity
        )));
    }

    let platform = Platform::parse(&row.platform)
        .ok_or_else(|| JobError::permanent(anyhow::anyhow!("bad platform")))?;

    let pack = build_knowledge_pack(&state.db, row.project_id, platform, Some(row.plan_video_id))
        .await
        .map_err(JobError::permanent)?;

    // What the plan said about this video (concept, flow, assets) for narrative context.
    let parsed = plan_format::parse(&row.markdown);
    let planned = parsed.videos.iter().find(|v| v.anchor == row.anchor);
    let video_payload = json!({
        "plan_video_id": row.plan_video_id,
        "title": row.title,
        "track": row.track,
        "daypart": row.daypart,
        "caption": row.caption,
        "hypothesis": planned.map(|v| v.hypothesis.clone()).unwrap_or_else(|| row.hypothesis.clone()),
        "flow": planned.map(|v| v.flow.clone()).unwrap_or_default(),
        "assets": planned
            .map(|v| v.checklist.iter().map(|c| c.text.clone()).collect::<Vec<_>>())
            .unwrap_or_default(),
    });

    // Metrics in the LLM payload are for narrative context only; diary numbers are copied below.
    let experiment_payload = json!({
        "id": row.id,
        "publish_local_date": row.publish_local_date,
        "hypothesis": row.hypothesis,
        "maturity": row.maturity,
        "views": row.views,
        "reach": row.reach,
        "interactions": row.interactions,
        "reels_play_count": row.reels_play_count,
        "avg_watch_time_ms": row.avg_watch_time_ms,
        "total_watch_time_ms": row.total_watch_time_ms,
        "skip_rate": row.skip_rate,
        "cta_clicks": row.cta_clicks,
        "cta_clicks_unique": row.cta_clicks_unique,
        "profile_visits": row.profile_visits,
        "bio_link_taps": row.bio_link_taps,
        "installs": row.installs,
        "est_installs": row.est_installs,
        "cta_rate": row.cta_rate,
        "install_rate": row.install_rate,
        "growth": row.growth,
    });

    let analysis = planner::analyze_experiment(
        state.ai.as_ref(),
        &pack,
        &experiment_payload,
        &video_payload,
    )
    .await
    .map_err(JobError::from_ai)?;

    let model = state.ai.deployment(TextRole::Planner).to_owned();
    let diary_id = new_id();

    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| JobError::Retry(e.into()))?;

    // Re-check under lock for idempotency.
    let still_open: Option<(Option<Uuid>,)> =
        sqlx::query_as("select observation_diary_id from experiments where id = $1 for update")
            .bind(experiment_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| JobError::Retry(e.into()))?;

    if still_open.and_then(|r| r.0).is_some() {
        tx.rollback().await.map_err(|e| JobError::Retry(e.into()))?;
        return Ok(());
    }

    // Numbers MUST come from the experiment row, never from the LLM.
    sqlx::query(
        r#"
        insert into diary_entries (
            id, project_id, entry_date, platform, plan_video_id, experiment_id, plan_id,
            audience_id, kind,
            views, cta_clicks, profile_visits, bio_link_taps, installs, est_installs,
            growth, avg_watch_time_ms, skip_rate, observation, author, model
        ) values ($1,$2,$3,$4,$5,$6,$7,$8,'experiment',$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,'agent',$19)
        "#,
    )
    .bind(diary_id)
    .bind(row.project_id)
    .bind(row.publish_local_date)
    .bind(platform.as_str())
    .bind(row.plan_video_id)
    .bind(experiment_id)
    .bind(row.plan_id)
    .bind(row.audience_id)
    .bind(row.views)
    .bind(row.cta_clicks)
    .bind(row.profile_visits)
    .bind(row.bio_link_taps)
    .bind(row.installs)
    .bind(row.est_installs)
    .bind(row.growth)
    .bind(row.avg_watch_time_ms)
    .bind(row.skip_rate)
    .bind(&analysis.observation)
    .bind(&model)
    .execute(&mut *tx)
    .await
    .map_err(|e| JobError::Retry(e.into()))?;

    sqlx::query(
        "update experiments set observation_diary_id = $2, updated_at = now() where id = $1",
    )
    .bind(experiment_id)
    .bind(diary_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| JobError::Retry(e.into()))?;

    tx.commit().await.map_err(|e| JobError::Retry(e.into()))?;

    if !analysis.playbook_candidates.is_empty()
        && let Err(e) =
            playbook::propose_candidates(&state.db, row.project_id, &analysis.playbook_candidates)
                .await
    {
        tracing::warn!(error = %e, %experiment_id, "playbook candidate proposal failed");
    }

    tracing::info!(%experiment_id, %diary_id, "analyze_experiment wrote diary");
    Ok(())
}
