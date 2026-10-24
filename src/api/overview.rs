//! Home page summary: headline stats, upcoming plans, recent videos, and an activity feed.

use axum::Json;
use axum::extract::{Path, State};
use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::domain::access;
use crate::error::AppResult;
use crate::state::AppState;
use crate::trends;

const RECENT_VIDEOS: i64 = 8;
const ACTIVITY_ITEMS: i64 = 20;

#[derive(Debug, Serialize, ToSchema)]
pub struct ProjectOverview {
    pub project_id: Uuid,
    pub name: String,
    /// Today in the project's timezone.
    pub today: NaiveDate,
    pub stats: OverviewStats,
    /// The next plan (today or later) per platform.
    pub plans: Vec<OverviewPlan>,
    /// Latest published videos across platforms, newest first.
    pub recent_videos: Vec<OverviewVideo>,
    /// Latest events (published videos, drafted / scheduled plans, diary notes), newest first.
    pub activity: Vec<ActivityItem>,
}

/// Totals over videos published in the last 7 days (and the 7 days before, for trends).
#[derive(Debug, Serialize, ToSchema, FromRow)]
pub struct OverviewStats {
    pub posts_7d: i64,
    pub views_7d: i64,
    pub views_prev_7d: i64,
    pub shares_7d: i64,
    /// Measured installs, else estimated installs.
    pub installs_7d: i64,
    /// `installs_7d / views_7d`, when there are views.
    pub install_rate_7d: Option<f64>,
}

#[derive(Debug, Serialize, ToSchema, FromRow)]
pub struct OverviewPlan {
    pub platform: String,
    pub plan_id: Uuid,
    pub for_date: NaiveDate,
    pub status: String,
    pub title: String,
    pub videos: i64,
}

#[derive(Debug, Serialize, ToSchema, FromRow)]
pub struct OverviewVideo {
    pub plan_video_id: Uuid,
    pub plan_id: Uuid,
    pub platform: String,
    pub title: String,
    pub published_at: DateTime<Utc>,
    pub views: Option<i64>,
    pub shares: Option<i64>,
    /// `1 - skip_rate`, when known.
    pub retention: Option<f64>,
    pub permalink: Option<String>,
}

#[derive(Debug, Serialize, ToSchema, FromRow)]
pub struct ActivityItem {
    pub at: DateTime<Utc>,
    /// video_published | plan_drafted | plan_scheduled | diary
    pub kind: String,
    pub platform: Option<String>,
    pub title: String,
    /// Permalink for published videos, observation text for diary notes.
    pub detail: Option<String>,
    pub plan_id: Option<Uuid>,
    pub plan_video_id: Option<Uuid>,
}

#[utoipa::path(
    get,
    path = "/projects/{pid}/overview",
    params(("pid" = Uuid, Path, description = "Project id")),
    responses(
        (status = 200, description = "Home page summary", body = ProjectOverview),
        (status = 409, description = "Onboarding not complete")
    )
)]
pub async fn get(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
) -> AppResult<Json<ProjectOverview>> {
    access::ensure_project_ready(&s.db, &user.id, pid).await?;
    let (name, timezone): (String, String) =
        sqlx::query_as("select name, timezone from projects where id = $1")
            .bind(pid)
            .fetch_one(&s.db)
            .await?;
    let now = s.clock.now();
    let today = trends::local_date_for_tz(&timezone, now);

    let stats = sqlx::query_as::<_, OverviewStats>(
        r#"
        select count(*) filter (where published_at >= $2 - interval '7 days') as posts_7d,
               coalesce(sum(views) filter (where published_at >= $2 - interval '7 days'), 0)::int8
                 as views_7d,
               coalesce(sum(views) filter (where published_at < $2 - interval '7 days'), 0)::int8
                 as views_prev_7d,
               coalesce(sum(shares) filter (where published_at >= $2 - interval '7 days'), 0)::int8
                 as shares_7d,
               coalesce(sum(coalesce(installs, est_installs))
                 filter (where published_at >= $2 - interval '7 days'), 0)::int8 as installs_7d,
               (sum(coalesce(installs, est_installs))
                  filter (where published_at >= $2 - interval '7 days'))::float8
                 / nullif(sum(views) filter (where published_at >= $2 - interval '7 days'), 0)::float8
                 as install_rate_7d
          from experiments
         where project_id = $1 and published_at >= $2 - interval '14 days'
        "#,
    )
    .bind(pid)
    .bind(now)
    .fetch_one(&s.db)
    .await?;

    let plans = sqlx::query_as::<_, OverviewPlan>(
        r#"
        select distinct on (dp.platform)
               dp.platform, dp.id as plan_id, dp.for_date, dp.status, dp.title,
               (select count(*) from plan_videos pv where pv.plan_id = dp.id) as videos
          from daily_plans dp
         where dp.project_id = $1 and dp.for_date >= $2
         order by dp.platform, dp.for_date
        "#,
    )
    .bind(pid)
    .bind(today)
    .fetch_all(&s.db)
    .await?;

    let recent_videos = sqlx::query_as::<_, OverviewVideo>(
        r#"
        select e.plan_video_id, pv.plan_id, e.platform, pv.title, e.published_at,
               e.views, e.shares,
               case when e.skip_rate is null then null else 1 - e.skip_rate end as retention,
               po.permalink
          from experiments e
          join plan_videos pv on pv.id = e.plan_video_id
          left join posts po on po.id = e.post_id
         where e.project_id = $1
         order by e.published_at desc
         limit $2
        "#,
    )
    .bind(pid)
    .bind(RECENT_VIDEOS)
    .fetch_all(&s.db)
    .await?;

    let activity = sqlx::query_as::<_, ActivityItem>(
        r#"
        select * from (
            select po.published_at as at, 'video_published'::text as kind,
                   po.platform as platform, pv.title as title, po.permalink as detail,
                   pv.plan_id as plan_id, pv.id as plan_video_id
              from posts po
              join plan_videos pv on pv.id = po.plan_video_id
             where po.project_id = $1 and po.status = 'published' and po.published_at is not null
            union all
            select v.created_at, 'plan_drafted'::text, dp.platform,
                   coalesce(nullif(dp.title, ''), 'Plan for ' || dp.for_date::text),
                   null::text, dp.id, null::uuid
              from plan_versions v
              join daily_plans dp on dp.id = v.plan_id
             where dp.project_id = $1 and v.reason = 'plan_day'
            union all
            select dp.scheduled_at, 'plan_scheduled'::text, dp.platform,
                   coalesce(nullif(dp.title, ''), 'Plan for ' || dp.for_date::text),
                   null::text, dp.id, null::uuid
              from daily_plans dp
             where dp.project_id = $1 and dp.scheduled_at is not null
            union all
            select d.created_at, 'diary'::text, d.platform,
                   case d.kind
                     when 'video_daily' then 'Video check-in'
                     when 'day_summary' then 'Day summary'
                     when 'experiment' then 'Experiment result'
                     when 'platform_trend' then 'Platform trend'
                     when 'plan_retro' then 'Plan retro'
                     else 'Note'
                   end,
                   d.observation, d.plan_id, d.plan_video_id
              from diary_entries d
             where d.project_id = $1
        ) a
        order by at desc
        limit $2
        "#,
    )
    .bind(pid)
    .bind(ACTIVITY_ITEMS)
    .fetch_all(&s.db)
    .await?;

    let mut plans = plans;
    for p in plans.iter_mut().filter(|p| p.title.is_empty()) {
        p.title = format!("Plan for {}", p.for_date);
    }

    Ok(Json(ProjectOverview {
        project_id: pid,
        name,
        today,
        stats,
        plans,
        recent_videos,
        activity,
    }))
}
