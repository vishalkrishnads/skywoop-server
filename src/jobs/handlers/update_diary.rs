use std::collections::HashMap;

use chrono::{Duration, NaiveDate};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::ai::client::TextRole;
use crate::ai::planner::PlaybookCandidateOut;
use crate::ai::prompt;
use crate::domain::Platform;
use crate::domain::new_id;
use crate::domain::plan;
use crate::domain::playbook;
use crate::jobs::JobError;
use crate::plan_format;
use crate::state::AppState;

#[derive(Debug, Deserialize, JsonSchema)]
struct VideoObservationOut {
    plan_video_id: String,
    observation: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DiaryUpdateOut {
    video_observations: Vec<VideoObservationOut>,
    day_summary: String,
    #[serde(default)]
    plan_retro: Option<String>,
    /// Creative patterns that clearly generalize beyond this app (usually empty).
    #[serde(default)]
    playbook_candidates: Vec<PlaybookCandidateOut>,
}

#[derive(sqlx::FromRow)]
struct ExpRow {
    plan_video_id: Uuid,
    experiment_id: Uuid,
    plan_id: Uuid,
    audience_id: Option<Uuid>,
    publish_local_date: NaiveDate,
    views: Option<i64>,
    cta_clicks: Option<i64>,
    profile_visits: Option<i64>,
    bio_link_taps: Option<i64>,
    installs: Option<i64>,
    est_installs: Option<i64>,
    growth: Option<i64>,
    cta_rate: Option<f64>,
    install_rate: Option<f64>,
    avg_watch_time_ms: Option<i64>,
    skip_rate: Option<f64>,
    reels_play_count: Option<i64>,
    maturity: String,
    title: String,
    track: String,
    daypart: Option<String>,
}

pub async fn run(
    state: &AppState,
    project_id: Uuid,
    platform: Platform,
    local_date: NaiveDate,
) -> Result<(), JobError> {
    let diary_date = local_date - Duration::days(1);
    let window_start = local_date - Duration::days(3);

    let rows: Vec<ExpRow> = sqlx::query_as(
        r#"
        select e.plan_video_id, e.id as experiment_id, pv.plan_id, e.audience_id,
               e.publish_local_date,
               e.views, e.cta_clicks, e.profile_visits, e.bio_link_taps,
               e.installs, e.est_installs, e.growth, e.cta_rate, e.install_rate,
               e.avg_watch_time_ms, e.skip_rate, e.reels_play_count,
               e.maturity, pv.title, pv.track, pv.daypart
          from experiments e
          join plan_videos pv on pv.id = e.plan_video_id
         where e.project_id = $1 and e.platform = $2
           and e.publish_local_date between $3 and $4
         order by e.publish_local_date desc, e.published_at desc
        "#,
    )
    .bind(project_id)
    .bind(platform.as_str())
    .bind(window_start)
    .bind(diary_date)
    .fetch_all(&state.db)
    .await
    .map_err(|e| JobError::Retry(e.into()))?;

    let diary_rows: Vec<&ExpRow> = rows
        .iter()
        .filter(|r| r.publish_local_date == diary_date)
        .collect();

    let entries_json: Value = json!(
        diary_rows
            .iter()
            .map(|r| {
                json!({
                    "plan_video_id": r.plan_video_id,
                    "experiment_id": r.experiment_id,
                    "title": r.title,
                    "track": r.track,
                    "daypart": r.daypart,
                    "publish_local_date": r.publish_local_date,
                    "maturity": r.maturity,
                    "views": r.views,
                    "reels_play_count": r.reels_play_count,
                    "avg_watch_time_ms": r.avg_watch_time_ms,
                    "skip_rate": r.skip_rate,
                    "cta_clicks": r.cta_clicks,
                    "profile_visits": r.profile_visits,
                    "bio_link_taps": r.bio_link_taps,
                    "installs": r.installs,
                    "est_installs": r.est_installs,
                    "growth": r.growth,
                    "cta_rate": r.cta_rate,
                    "install_rate": r.install_rate,
                })
            })
            .collect::<Vec<_>>()
    );

    let prior_plan = load_prior_plan(&state.db, project_id, platform, diary_date).await?;

    let recent_diary: Vec<(NaiveDate, String, String)> = sqlx::query_as(
        r#"
        select entry_date, kind, observation
          from diary_entries
         where project_id = $1 and platform = $2
           and entry_date >= ($3::date - 7)
           and kind in ('day_summary', 'video_daily', 'plan_retro')
         order by entry_date desc, created_at desc
         limit 40
        "#,
    )
    .bind(project_id)
    .bind(platform.as_str())
    .bind(diary_date)
    .fetch_all(&state.db)
    .await
    .map_err(|e| JobError::Retry(e.into()))?;

    let recent_diary_json = json!(
        recent_diary
            .iter()
            .map(|(d, k, o)| json!({ "entry_date": d, "kind": k, "observation": o }))
            .collect::<Vec<_>>()
    );

    let (observations, day_summary, plan_retro, candidates, model) = if diary_rows.is_empty() {
        (
            HashMap::<Uuid, String>::new(),
            "No videos were published this day.".to_owned(),
            None,
            Vec::new(),
            None,
        )
    } else {
        let rendered = prompt::render(
            "update_diary",
            &json!({
                "entries": serde_json::to_string_pretty(&entries_json)
                    .map_err(|e| JobError::permanent(anyhow::Error::from(e)))?,
                "prior_plan": prior_plan
                    .as_ref()
                    .map(|p| serde_json::to_string_pretty(p).unwrap_or_default())
                    .unwrap_or_default(),
                "recent_diary": serde_json::to_string_pretty(&recent_diary_json)
                    .map_err(|e| JobError::permanent(anyhow::Error::from(e)))?,
            }),
        )
        .map_err(JobError::from_ai)?;

        let out: DiaryUpdateOut = state
            .ai
            .structured(
                TextRole::Planner,
                "update_diary",
                &rendered,
                &[],
                crate::ai::Effort::Medium,
            )
            .await
            .map_err(JobError::from_ai)?;

        let model = Some(state.ai.deployment(TextRole::Planner).to_owned());
        let mut obs_map = HashMap::new();
        for o in out.video_observations {
            if let Ok(id) = Uuid::parse_str(&o.plan_video_id) {
                obs_map.insert(id, o.observation);
            }
        }
        (
            obs_map,
            out.day_summary,
            out.plan_retro,
            out.playbook_candidates,
            model,
        )
    };

    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| JobError::Retry(e.into()))?;

    for r in &rows {
        let observation = observations
            .get(&r.plan_video_id)
            .cloned()
            .unwrap_or_else(|| "Published; metrics recorded.".into());
        sqlx::query(
            r#"
            insert into diary_entries (
                id, project_id, entry_date, platform, plan_video_id, experiment_id,
                plan_id, audience_id, kind,
                views, cta_clicks, profile_visits, bio_link_taps,
                installs, est_installs, growth, avg_watch_time_ms, skip_rate,
                observation, author, model
            ) values (
                $1,$2,$3,$4,$5,$6,$7,$8,'video_daily',
                $9,$10,$11,$12,$13,$14,$15,$16,$17,$18,'agent',$19
            )
            on conflict (plan_video_id, entry_date, kind)
              where plan_video_id is not null and kind = 'video_daily'
            do update set
                views = excluded.views,
                cta_clicks = excluded.cta_clicks,
                profile_visits = excluded.profile_visits,
                bio_link_taps = excluded.bio_link_taps,
                installs = excluded.installs,
                est_installs = excluded.est_installs,
                growth = excluded.growth,
                avg_watch_time_ms = excluded.avg_watch_time_ms,
                skip_rate = excluded.skip_rate,
                observation = excluded.observation,
                model = excluded.model
            "#,
        )
        .bind(new_id())
        .bind(project_id)
        .bind(r.publish_local_date)
        .bind(platform.as_str())
        .bind(r.plan_video_id)
        .bind(r.experiment_id)
        .bind(r.plan_id)
        .bind(r.audience_id)
        .bind(r.views)
        .bind(r.cta_clicks)
        .bind(r.profile_visits)
        .bind(r.bio_link_taps)
        .bind(r.installs)
        .bind(r.est_installs)
        .bind(r.growth)
        .bind(r.avg_watch_time_ms)
        .bind(r.skip_rate)
        .bind(&observation)
        .bind(model.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(|e| JobError::Retry(e.into()))?;
    }

    sqlx::query(
        r#"
        insert into diary_entries (
            id, project_id, entry_date, platform, kind, observation, author, model
        ) values ($1,$2,$3,$4,'day_summary',$5,'agent',$6)
        on conflict (project_id, platform, entry_date, kind)
          where kind = 'day_summary'
        do update set observation = excluded.observation, model = excluded.model
        "#,
    )
    .bind(new_id())
    .bind(project_id)
    .bind(diary_date)
    .bind(platform.as_str())
    .bind(&day_summary)
    .bind(model.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(|e| JobError::Retry(e.into()))?;

    if let (Some(retro), Some(plan_id)) = (
        plan_retro.as_ref(),
        prior_plan
            .as_ref()
            .and_then(|p| p.get("id"))
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok()),
    ) {
        sqlx::query(
            r#"
            insert into diary_entries (
                id, project_id, entry_date, platform, plan_id, kind, observation, author, model
            ) values ($1,$2,$3,$4,$5,'plan_retro',$6,'agent',$7)
            on conflict (plan_id, kind)
              where plan_id is not null and kind = 'plan_retro'
            do update set observation = excluded.observation, model = excluded.model
            "#,
        )
        .bind(new_id())
        .bind(project_id)
        .bind(diary_date)
        .bind(platform.as_str())
        .bind(plan_id)
        .bind(retro)
        .bind(model.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(|e| JobError::Retry(e.into()))?;
    }

    tx.commit().await.map_err(|e| JobError::Retry(e.into()))?;

    // Candidate playbook patterns wait for operator review; failures must not fail the diary.
    if !candidates.is_empty()
        && let Err(e) = playbook::propose_candidates(&state.db, project_id, &candidates).await
    {
        tracing::warn!(error = %e, %project_id, "playbook candidate proposal failed");
    }

    tracing::info!(
        %project_id,
        %platform,
        %diary_date,
        videos = diary_rows.len(),
        "update_diary complete"
    );
    Ok(())
}

async fn load_prior_plan(
    pool: &sqlx::PgPool,
    project_id: Uuid,
    platform: Platform,
    for_date: NaiveDate,
) -> Result<Option<Value>, JobError> {
    let Some(p) = plan::get_plan_for_date(pool, project_id, platform, for_date)
        .await
        .map_err(|e| JobError::Retry(e.into()))?
    else {
        return Ok(None);
    };
    let videos = plan::list_videos(pool, p.id)
        .await
        .map_err(|e| JobError::Retry(e.into()))?;
    let parsed = plan_format::parse(&p.markdown);
    Ok(Some(json!({
        "id": p.id,
        "for_date": p.for_date,
        "status": p.status,
        "greeting": p.greeting,
        "videos": videos.iter().map(|v| json!({
            "plan_video_id": v.id,
            "title": v.title,
            "track": v.track,
            "daypart": v.daypart,
            "hypothesis": parsed
                .videos
                .iter()
                .find(|pv| pv.anchor == v.anchor)
                .map(|pv| pv.hypothesis.clone())
                .unwrap_or_default(),
        })).collect::<Vec<_>>(),
    })))
}
