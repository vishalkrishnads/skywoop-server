//! Diary and per-day history over published videos.

use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use sqlx::{FromRow, PgPool};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::domain::Platform;
use crate::domain::types::new_id;
use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Serialize, FromRow, ToSchema)]
pub struct DiaryEntryView {
    pub id: Uuid,
    pub project_id: Uuid,
    pub entry_date: NaiveDate,
    pub platform: String,
    pub plan_video_id: Option<Uuid>,
    pub plan_id: Option<Uuid>,
    pub experiment_id: Option<Uuid>,
    pub audience_id: Option<Uuid>,
    /// experiment | platform_trend | manual | video_daily | day_summary | plan_retro
    pub kind: String,
    pub views: Option<i64>,
    pub cta_clicks: Option<i64>,
    pub profile_visits: Option<i64>,
    pub bio_link_taps: Option<i64>,
    pub installs: Option<i64>,
    pub est_installs: Option<i64>,
    pub growth: Option<i64>,
    pub avg_watch_time_ms: Option<i64>,
    pub skip_rate: Option<f64>,
    pub observation: String,
    pub author: String,
    pub model: Option<String>,
    pub created_at: DateTime<Utc>,
    pub video_title: Option<String>,
}

const DIARY_SELECT: &str = r#"
    select d.id, d.project_id, d.entry_date, d.platform, d.plan_video_id, d.plan_id,
           d.experiment_id, d.audience_id, d.kind, d.views, d.cta_clicks, d.profile_visits,
           d.bio_link_taps, d.installs, d.est_installs, d.growth, d.avg_watch_time_ms,
           d.skip_rate, d.observation, d.author, d.model, d.created_at,
           pv.title as video_title
      from diary_entries d
      left join plan_videos pv on pv.id = d.plan_video_id
"#;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HistoryDay {
    pub date: NaiveDate,
    pub entries: Vec<HistoryEntry>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HistoryEntry {
    pub kind: String,
    pub experiment_id: Option<Uuid>,
    pub plan_video_id: Option<Uuid>,
    pub plan_id: Option<Uuid>,
    pub video_title: Option<String>,
    pub permalink: Option<String>,
    pub views: Option<i64>,
    pub cta_clicks: Option<i64>,
    pub installs: Option<i64>,
    pub growth: Option<i64>,
    pub avg_watch_time_ms: Option<i64>,
    pub skip_rate: Option<f64>,
    pub observation: Option<String>,
    pub diary_id: Option<Uuid>,
}

pub async fn list_diary(
    db: &PgPool,
    project_id: Uuid,
    platform: Option<Platform>,
    plan_video_id: Option<Uuid>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> AppResult<Vec<DiaryEntryView>> {
    let rows = sqlx::query_as::<_, DiaryEntryView>(sqlx::AssertSqlSafe(format!(
        r#"{DIARY_SELECT}
         where d.project_id = $1
           and ($2::text is null or d.platform = $2)
           and ($3::uuid is null or d.plan_video_id = $3)
           and ($4::date is null or d.entry_date >= $4)
           and ($5::date is null or d.entry_date <= $5)
         order by d.entry_date desc, d.created_at desc
        "#
    )))
    .bind(project_id)
    .bind(platform.map(|p| p.as_str()))
    .bind(plan_video_id)
    .bind(from)
    .bind(to)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

#[allow(clippy::too_many_arguments)]
pub async fn create_manual_diary(
    db: &PgPool,
    project_id: Uuid,
    platform: Platform,
    entry_date: NaiveDate,
    plan_video_id: Option<Uuid>,
    observation: &str,
    views: Option<i64>,
    cta_clicks: Option<i64>,
    installs: Option<i64>,
    growth: Option<i64>,
) -> AppResult<DiaryEntryView> {
    if observation.trim().is_empty() {
        return Err(AppError::Invalid("observation is required".into()));
    }
    if let Some(vid) = plan_video_id {
        let ok: (bool,) = sqlx::query_as(
            r#"
            select exists(
                select 1 from plan_videos pv join daily_plans dp on dp.id = pv.plan_id
                 where pv.id = $1 and dp.project_id = $2 and dp.platform = $3
            )
            "#,
        )
        .bind(vid)
        .bind(project_id)
        .bind(platform.as_str())
        .fetch_one(db)
        .await?;
        if !ok.0 {
            return Err(AppError::Invalid(
                "video not found on this project/platform".into(),
            ));
        }
    }

    let id = new_id();
    sqlx::query(
        r#"
        insert into diary_entries (
            id, project_id, entry_date, platform, plan_video_id, experiment_id, kind,
            views, cta_clicks, installs, growth, observation, author, model
        ) values ($1,$2,$3,$4,$5,null,'manual',$6,$7,$8,$9,$10,'user',null)
        "#,
    )
    .bind(id)
    .bind(project_id)
    .bind(entry_date)
    .bind(platform.as_str())
    .bind(plan_video_id)
    .bind(views)
    .bind(cta_clicks)
    .bind(installs)
    .bind(growth)
    .bind(observation.trim())
    .execute(db)
    .await?;

    get_diary_entry(db, id).await
}

pub async fn get_diary_entry(db: &PgPool, id: Uuid) -> AppResult<DiaryEntryView> {
    sqlx::query_as::<_, DiaryEntryView>(sqlx::AssertSqlSafe(format!(
        "{DIARY_SELECT} where d.id = $1"
    )))
    .bind(id)
    .fetch_one(db)
    .await
    .map_err(AppError::from)
}

pub async fn patch_diary_entry(
    db: &PgPool,
    id: Uuid,
    observation: Option<&str>,
    views: Option<Option<i64>>,
    cta_clicks: Option<Option<i64>>,
    installs: Option<Option<i64>>,
    growth: Option<Option<i64>>,
) -> AppResult<DiaryEntryView> {
    let current = get_diary_entry(db, id).await?;
    if observation.is_none()
        && views.is_none()
        && cta_clicks.is_none()
        && installs.is_none()
        && growth.is_none()
    {
        return Ok(current);
    }
    if let Some(obs) = observation
        && obs.trim().is_empty()
    {
        return Err(AppError::Invalid("observation cannot be empty".into()));
    }

    sqlx::query(
        r#"
        update diary_entries
           set observation = coalesce($2, observation),
               views = case when $3::bool then $4 else views end,
               cta_clicks = case when $5::bool then $6 else cta_clicks end,
               installs = case when $7::bool then $8 else installs end,
               growth = case when $9::bool then $10 else growth end
         where id = $1
        "#,
    )
    .bind(id)
    .bind(observation.map(str::trim))
    .bind(views.is_some())
    .bind(views.flatten())
    .bind(cta_clicks.is_some())
    .bind(cta_clicks.flatten())
    .bind(installs.is_some())
    .bind(installs.flatten())
    .bind(growth.is_some())
    .bind(growth.flatten())
    .execute(db)
    .await?;

    get_diary_entry(db, id).await
}

pub async fn history_by_day(
    db: &PgPool,
    project_id: Uuid,
    platform: Platform,
    from: NaiveDate,
    to: NaiveDate,
) -> AppResult<Vec<HistoryDay>> {
    if to < from {
        return Err(AppError::Invalid("`to` must be on or after `from`".into()));
    }

    #[derive(FromRow)]
    struct Row {
        date: NaiveDate,
        kind: String,
        experiment_id: Option<Uuid>,
        plan_video_id: Option<Uuid>,
        plan_id: Option<Uuid>,
        video_title: Option<String>,
        permalink: Option<String>,
        views: Option<i64>,
        cta_clicks: Option<i64>,
        installs: Option<i64>,
        growth: Option<i64>,
        avg_watch_time_ms: Option<i64>,
        skip_rate: Option<f64>,
        observation: Option<String>,
        diary_id: Option<Uuid>,
        #[allow(dead_code)]
        sort_ts: DateTime<Utc>,
    }

    let rows = sqlx::query_as::<_, Row>(
        r#"
        select e.publish_local_date as date,
               'experiment'::text as kind,
               e.id as experiment_id,
               e.plan_video_id,
               pv.plan_id,
               pv.title as video_title,
               po.permalink,
               e.views,
               e.cta_clicks,
               e.installs,
               e.growth,
               e.avg_watch_time_ms,
               e.skip_rate,
               d.observation,
               d.id as diary_id,
               e.published_at as sort_ts
          from experiments e
          join plan_videos pv on pv.id = e.plan_video_id
          join posts po on po.id = e.post_id
          left join diary_entries d on d.id = e.observation_diary_id
         where e.project_id = $1 and e.platform = $2
           and e.publish_local_date between $3 and $4

        union all

        select d.entry_date as date,
               d.kind,
               d.experiment_id,
               d.plan_video_id,
               d.plan_id,
               pv.title as video_title,
               null::text as permalink,
               d.views,
               d.cta_clicks,
               d.installs,
               d.growth,
               d.avg_watch_time_ms,
               d.skip_rate,
               d.observation,
               d.id as diary_id,
               d.created_at as sort_ts
          from diary_entries d
          left join plan_videos pv on pv.id = d.plan_video_id
         where d.project_id = $1 and d.platform = $2
           and d.entry_date between $3 and $4
           and d.kind in ('platform_trend', 'manual', 'day_summary', 'plan_retro')
           and d.experiment_id is null

         order by 1 desc, 16 desc
        "#,
    )
    .bind(project_id)
    .bind(platform.as_str())
    .bind(from)
    .bind(to)
    .fetch_all(db)
    .await?;

    let mut days: Vec<HistoryDay> = Vec::new();
    for row in rows {
        let entry = HistoryEntry {
            kind: row.kind,
            experiment_id: row.experiment_id,
            plan_video_id: row.plan_video_id,
            plan_id: row.plan_id,
            video_title: row.video_title,
            permalink: row.permalink,
            views: row.views,
            cta_clicks: row.cta_clicks,
            installs: row.installs,
            growth: row.growth,
            avg_watch_time_ms: row.avg_watch_time_ms,
            skip_rate: row.skip_rate,
            observation: row.observation,
            diary_id: row.diary_id,
        };
        if let Some(day) = days.last_mut()
            && day.date == row.date
        {
            day.entries.push(entry);
        } else {
            days.push(HistoryDay {
                date: row.date,
                entries: vec![entry],
            });
        }
    }
    Ok(days)
}
