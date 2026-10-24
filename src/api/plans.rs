//! Markdown daily plans: read, edit, comment, chat, status, schedule.
//!
//! The plan is one markdown document (see [`crate::plan_format`]). Editing it re-parses the
//! video sections and upserts `plan_videos`. Past plans are read-only; `schedule` locks the plan.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::domain::Platform;
use crate::domain::access;
use crate::domain::assets;
use crate::domain::links;
use crate::domain::new_id;
use crate::domain::plan::{
    self, DailyPlan, Daypart, NO_OP_REASON_USER, NoOpMetrics, PlanComment, PlanMessage, PlanStatus,
    PlanTrack, PlanVideo, ProjectAudience, SyncCtx, insert_plan_version, list_active_audiences,
    replace_audiences,
};
use crate::error::{AppError, AppResult};
use crate::jobs::{DataJob, DataJobQueue, UserJob};
use crate::plan_format::{
    self, ChecklistEdit, ChecklistEditError, ChecklistItem, ChecklistKind, ChecklistState,
    CommentNote, FlowStep, KnownAsset, PlanIssue, ValidateOpts,
};
use crate::scheduling::industries::{Industry, prime_windows_for_industry};
use crate::scheduling::{
    DaypartWindows, PrimeWindow, SchedulableItem, assign_times, load_schedule_with_dayparts,
    online_followers_by_hour, prime_windows_from_slots, upsert_schedule_full,
};
use crate::state::AppState;
use crate::trends;

/// Reels API limit: 30 API-published Reels per rolling 24 hours per Page.
pub const FB_REELS_PER_24H: i64 = 30;

// ── Views ────────────────────────────────────────────────────────────

/// Lifetime of signed preview / render URLs in plan views.
const READ_URL_TTL_MINUTES: i64 = 60;

#[derive(Debug, Serialize, ToSchema)]
pub struct VideoProjectSummary {
    pub id: Uuid,
    pub rev: i32,
    pub render_rev: Option<i32>,
    /// draft | building | ready | failed
    pub status: String,
    /// A render exists for the current timeline revision.
    pub render_current: bool,
    /// Signed read URL of the latest render (absent before render or after cleanup).
    pub render_url: Option<String>,
    pub render_bytes: Option<i64>,
    /// Timeline duration / size / frame rate.
    pub duration_ms: Option<i64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub fps: Option<f64>,
    pub rendered_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PostSummary {
    pub id: Uuid,
    pub status: String,
    pub scheduled_for: Option<DateTime<Utc>>,
    pub published_at: Option<DateTime<Utc>>,
    pub permalink: Option<String>,
    pub media_deleted: bool,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PlanVideoView {
    pub video: PlanVideo,
    pub audience_name: Option<String>,
    pub hypothesis: String,
    /// Raw text of the plan's ```flow block.
    pub flow: String,
    /// Parsed flow steps (timings absent for legacy `A --> B` flows).
    pub flow_steps: Vec<FlowStep>,
    pub checklist: Vec<ChecklistItem>,
    pub video_project: Option<VideoProjectSummary>,
    pub post: Option<PostSummary>,
}

/// How a video published the day before this plan performed.
#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct PreviousDayVideo {
    pub plan_video_id: Uuid,
    pub title: String,
    pub views: Option<i64>,
    pub shares: Option<i64>,
    /// `1 - skip_rate`, when known.
    pub retention: Option<f64>,
    /// Latest daily diary observation for the video.
    pub note: Option<String>,
    pub permalink: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PlanView {
    pub plan: DailyPlan,
    /// Past plans and plans that are scheduled/posted cannot be edited.
    pub read_only: bool,
    pub videos: Vec<PlanVideoView>,
    pub comments: Vec<PlanComment>,
    /// Markdown with comments injected as `> User commented: …` blockquotes (for agents).
    pub annotated_markdown: String,
    /// Non-fatal findings (unresolved audiences, missing `@path` assets).
    pub warnings: Vec<String>,
    /// Signals behind no-op days (recent view growth, install rate).
    pub no_op_metrics: NoOpMetrics,
    /// Videos published on `for_date - 1` on this platform.
    pub previous_day: Vec<PreviousDayVideo>,
}

#[derive(sqlx::FromRow)]
struct VideoExtraRow {
    id: Uuid,
    audience_name: Option<String>,
    vp_id: Option<Uuid>,
    vp_rev: Option<i32>,
    vp_render_rev: Option<i32>,
    vp_status: Option<String>,
    vp_has_render: Option<bool>,
    vp_render_blob_path: Option<String>,
    vp_render_bytes: Option<i64>,
    vp_duration_ms: Option<f64>,
    vp_width: Option<f64>,
    vp_height: Option<f64>,
    vp_fps: Option<f64>,
    vp_rendered_at: Option<DateTime<Utc>>,
    post_id: Option<Uuid>,
    post_status: Option<String>,
    post_scheduled_for: Option<DateTime<Utc>>,
    post_published_at: Option<DateTime<Utc>>,
    post_permalink: Option<String>,
    post_media_deleted: Option<bool>,
    post_error: Option<String>,
}

async fn project_today(db: &PgPool, project_id: Uuid) -> AppResult<NaiveDate> {
    let tz: String = sqlx::query_scalar("select timezone from projects where id = $1")
        .bind(project_id)
        .fetch_one(db)
        .await?;
    Ok(trends::local_date_for_tz(&tz, Utc::now()))
}

fn is_read_only(plan: &DailyPlan, today: NaiveDate) -> bool {
    let status = PlanStatus::parse(&plan.status);
    plan.for_date < today || !status.is_some_and(PlanStatus::is_editable)
}

async fn known_assets_for(db: &PgPool, project_id: Uuid) -> AppResult<Vec<KnownAsset>> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("select path, kind from assets where project_id = $1")
            .bind(project_id)
            .fetch_all(db)
            .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(path, kind)| {
            ChecklistKind::parse(&kind).map(|kind| KnownAsset { path, kind })
        })
        .collect())
}

/// Signed read URL for each `@path` in `paths` that exists in the asset manager.
async fn asset_read_urls(
    s: &AppState,
    project_id: Uuid,
    paths: &[String],
) -> AppResult<HashMap<String, String>> {
    let normalized: Vec<String> = paths
        .iter()
        .filter_map(|p| assets::normalize_path(p).ok())
        .filter(|p| !p.is_empty())
        .collect();
    if normalized.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(String, String)> = sqlx::query_as(
        "select path, blob_path from assets where project_id = $1 and path = any($2)",
    )
    .bind(project_id)
    .bind(&normalized)
    .fetch_all(&s.db)
    .await?;
    let mut out = HashMap::new();
    for (path, blob_path) in rows {
        match s
            .blob
            .public_read_url(&blob_path, Duration::minutes(READ_URL_TTL_MINUTES))
            .await
        {
            Ok(u) => {
                out.insert(path, u.to_string());
            }
            Err(e) => tracing::warn!(error = %e, %path, "preview url signing failed"),
        }
    }
    Ok(out)
}

async fn previous_day_videos(db: &PgPool, plan: &DailyPlan) -> AppResult<Vec<PreviousDayVideo>> {
    Ok(sqlx::query_as::<_, PreviousDayVideo>(
        r#"
        select e.plan_video_id, pv.title, e.views, e.shares,
               case when e.skip_rate is null then null else 1 - e.skip_rate end as retention,
               (select d.observation from diary_entries d
                 where d.plan_video_id = e.plan_video_id and d.kind = 'video_daily'
                 order by d.entry_date desc, d.created_at desc limit 1) as note,
               po.permalink
          from experiments e
          join plan_videos pv on pv.id = e.plan_video_id
          left join posts po on po.id = e.post_id
         where e.project_id = $1 and e.platform = $2 and e.publish_local_date = $3
         order by e.published_at
        "#,
    )
    .bind(plan.project_id)
    .bind(&plan.platform)
    .bind(plan.for_date - Duration::days(1))
    .fetch_all(db)
    .await?)
}

async fn load_plan_view(
    s: &AppState,
    mut plan: DailyPlan,
    warnings: Vec<String>,
) -> AppResult<PlanView> {
    let db = &s.db;
    let today = project_today(db, plan.project_id).await?;
    let videos = plan::list_videos(db, plan.id)
        .await
        .map_err(AppError::Other)?;
    let comments = plan::list_comments(db, plan.id)
        .await
        .map_err(AppError::Other)?;
    let mut parsed = plan_format::parse(&plan.markdown);
    let known = known_assets_for(db, plan.project_id).await?;
    plan_format::annotate_generation_readiness(&mut parsed, &known);

    // Plans saved before titles were stored still show their markdown title / summary.
    if plan.title.is_empty() && plan.summary.is_empty() {
        (plan.title, plan.summary) = plan::title_and_summary(&parsed);
    }

    let first_refs: Vec<String> = parsed
        .videos
        .iter()
        .flat_map(|v| v.checklist.iter())
        .filter_map(|c| c.refs.first().cloned())
        .collect();
    let previews = asset_read_urls(s, plan.project_id, &first_refs).await?;
    for item in parsed
        .videos
        .iter_mut()
        .flat_map(|v| v.checklist.iter_mut())
    {
        item.preview_url = item
            .refs
            .first()
            .and_then(|r| assets::normalize_path(r).ok())
            .and_then(|p| previews.get(&p).cloned());
    }

    let extras: Vec<VideoExtraRow> = sqlx::query_as(
        r#"
        select pv.id,
               pa.name as audience_name,
               vp.id as vp_id, vp.rev as vp_rev, vp.render_rev as vp_render_rev,
               vp.status as vp_status,
               (vp.render_blob_path is not null) as vp_has_render,
               vp.render_blob_path as vp_render_blob_path,
               vp.render_bytes as vp_render_bytes,
               case when jsonb_typeof(vp.timeline->'duration_ms') = 'number'
                    then (vp.timeline->>'duration_ms')::float8 end as vp_duration_ms,
               case when jsonb_typeof(vp.timeline->'width') = 'number'
                    then (vp.timeline->>'width')::float8 end as vp_width,
               case when jsonb_typeof(vp.timeline->'height') = 'number'
                    then (vp.timeline->>'height')::float8 end as vp_height,
               case when jsonb_typeof(vp.timeline->'fps') = 'number'
                    then (vp.timeline->>'fps')::float8 end as vp_fps,
               case when vp.render_blob_path is not null then vp.rendered_at end as vp_rendered_at,
               po.id as post_id, po.status as post_status,
               po.scheduled_for as post_scheduled_for, po.published_at as post_published_at,
               po.permalink as post_permalink, po.media_deleted as post_media_deleted,
               po.error as post_error
          from plan_videos pv
          left join project_audiences pa on pa.id = pv.audience_id
          left join video_projects vp on vp.plan_video_id = pv.id
          left join posts po on po.plan_video_id = pv.id
         where pv.plan_id = $1
        "#,
    )
    .bind(plan.id)
    .fetch_all(db)
    .await?;

    let mut render_urls: HashMap<Uuid, String> = HashMap::new();
    for x in &extras {
        if let Some(b) = &x.vp_render_blob_path {
            match s
                .blob
                .public_read_url(b, Duration::minutes(READ_URL_TTL_MINUTES))
                .await
            {
                Ok(u) => {
                    render_urls.insert(x.id, u.to_string());
                }
                Err(e) => tracing::warn!(error = %e, blob = %b, "render url signing failed"),
            }
        }
    }

    let views = videos
        .into_iter()
        .map(|v| {
            let x = extras.iter().find(|e| e.id == v.id);
            let p = parsed.videos.iter().find(|p| p.anchor == v.anchor);
            PlanVideoView {
                audience_name: x.and_then(|x| x.audience_name.clone()),
                hypothesis: p.map(|p| p.hypothesis.clone()).unwrap_or_default(),
                flow: p.map(|p| p.flow.clone()).unwrap_or_default(),
                flow_steps: p.map(|p| p.flow_steps.clone()).unwrap_or_default(),
                checklist: p.map(|p| p.checklist.clone()).unwrap_or_default(),
                video_project: x.and_then(|x| {
                    Some(VideoProjectSummary {
                        id: x.vp_id?,
                        rev: x.vp_rev?,
                        render_rev: x.vp_render_rev,
                        status: x.vp_status.clone()?,
                        render_current: x.vp_has_render == Some(true)
                            && x.vp_render_rev.is_some()
                            && x.vp_render_rev == x.vp_rev,
                        render_url: render_urls.get(&x.id).cloned(),
                        render_bytes: x.vp_render_bytes,
                        duration_ms: x.vp_duration_ms.map(|d| d.round() as i64),
                        width: x.vp_width.map(|w| w.round() as i32),
                        height: x.vp_height.map(|h| h.round() as i32),
                        fps: x.vp_fps,
                        rendered_at: x.vp_rendered_at,
                    })
                }),
                post: x.and_then(|x| {
                    Some(PostSummary {
                        id: x.post_id?,
                        status: x.post_status.clone()?,
                        scheduled_for: x.post_scheduled_for,
                        published_at: x.post_published_at,
                        permalink: x.post_permalink.clone(),
                        media_deleted: x.post_media_deleted.unwrap_or(false),
                        error: x.post_error.clone(),
                    })
                }),
                video: v,
            }
        })
        .collect();

    let notes: Vec<CommentNote> = comments
        .iter()
        .map(|c| CommentNote {
            anchor: c.anchor.clone(),
            author: c.author.clone(),
            body: c.body.clone(),
        })
        .collect();
    let annotated_markdown = plan_format::annotate(&plan.markdown, &notes);
    let no_op_metrics = plan::no_op_metrics(db, plan.project_id, &plan.platform, s.clock.now())
        .await
        .map_err(AppError::Other)?;
    let previous_day = previous_day_videos(db, &plan).await?;

    Ok(PlanView {
        read_only: is_read_only(&plan, today),
        plan,
        videos: views,
        comments,
        annotated_markdown,
        warnings,
        no_op_metrics,
        previous_day,
    })
}

async fn ensure_workspace(db: &PgPool, pid: Uuid, platform: Platform) -> AppResult<()> {
    let exists: bool = sqlx::query_scalar(
        "select exists(select 1 from platform_workspaces where project_id = $1 and platform = $2)",
    )
    .bind(pid)
    .bind(platform.as_str())
    .fetch_one(db)
    .await?;
    if !exists {
        return Err(AppError::NotFound);
    }
    Ok(())
}

pub(crate) async fn require_plan(db: &PgPool, id: Uuid) -> AppResult<DailyPlan> {
    plan::get_plan(db, id)
        .await
        .map_err(AppError::Other)?
        .ok_or(AppError::NotFound)
}

/// Ensure the plan can be edited by the user (not past, not locked, not being drafted).
pub(crate) async fn require_editable(db: &PgPool, plan: &DailyPlan) -> AppResult<()> {
    let today = project_today(db, plan.project_id).await?;
    if plan.for_date < today {
        return Err(AppError::Conflict("Past plans are read-only.".into()));
    }
    let editable = PlanStatus::parse(&plan.status).is_some_and(PlanStatus::is_editable);
    if !editable {
        return Err(AppError::Conflict(format!(
            "Plan is {} and cannot be edited.",
            plan.status
        )));
    }
    Ok(())
}

fn issues_to_error(issues: &[PlanIssue]) -> AppError {
    let mut msg = String::from("Plan markdown is invalid:");
    for i in issues.iter().take(20) {
        msg.push_str("\n- ");
        msg.push_str(&i.to_string());
    }
    if issues.len() > 20 {
        msg.push_str(&format!("\n… and {} more", issues.len() - 20));
    }
    AppError::Invalid(msg)
}

// ── Read ─────────────────────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/projects/{pid}/platforms/{p}/plans/latest",
    params(
        ("pid" = Uuid, Path, description = "Project id"),
        ("p" = String, Path, description = "Platform (instagram|facebook)")
    ),
    responses((status = 200, description = "Latest plan by date", body = PlanView), (status = 404, description = "No plan yet"))
)]
pub async fn get_latest(
    State(s): State<AppState>,
    user: AuthUser,
    Path((pid, p)): Path<(Uuid, String)>,
) -> AppResult<Json<PlanView>> {
    let platform =
        Platform::parse(&p).ok_or_else(|| AppError::Invalid(format!("unknown platform: {p}")))?;
    access::ensure_project_ready(&s.db, &user.id, pid).await?;
    ensure_workspace(&s.db, pid, platform).await?;
    let plan = plan::latest_plan(&s.db, pid, platform)
        .await
        .map_err(AppError::Other)?
        .ok_or(AppError::NotFound)?;
    Ok(Json(load_plan_view(&s, plan, vec![]).await?))
}

#[utoipa::path(
    get,
    path = "/projects/{pid}/platforms/{p}/plans/{date}",
    params(
        ("pid" = Uuid, Path, description = "Project id"),
        ("p" = String, Path, description = "Platform"),
        ("date" = String, Path, description = "Plan date YYYY-MM-DD (project timezone)")
    ),
    responses((status = 200, description = "Plan for the date (past plans are read-only)", body = PlanView), (status = 404, description = "No plan"))
)]
pub async fn get_for_date(
    State(s): State<AppState>,
    user: AuthUser,
    Path((pid, p, date)): Path<(Uuid, String, String)>,
) -> AppResult<Json<PlanView>> {
    let platform =
        Platform::parse(&p).ok_or_else(|| AppError::Invalid(format!("unknown platform: {p}")))?;
    let date = NaiveDate::parse_from_str(&date, "%Y-%m-%d")
        .map_err(|_| AppError::Invalid("date must be YYYY-MM-DD".into()))?;
    access::ensure_project_ready(&s.db, &user.id, pid).await?;
    ensure_workspace(&s.db, pid, platform).await?;
    let plan = plan::get_plan_for_date(&s.db, pid, platform, date)
        .await
        .map_err(AppError::Other)?
        .ok_or(AppError::NotFound)?;
    Ok(Json(load_plan_view(&s, plan, vec![]).await?))
}

#[derive(Debug, Deserialize)]
pub struct PlanListQuery {
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct PlanListItem {
    pub id: Uuid,
    pub for_date: NaiveDate,
    pub status: String,
    pub title: String,
}

/// Longest date range `GET …/plans` returns.
const MAX_PLAN_LIST_DAYS: i64 = 366;

#[utoipa::path(
    get,
    path = "/projects/{pid}/platforms/{p}/plans",
    params(
        ("pid" = Uuid, Path, description = "Project id"),
        ("p" = String, Path, description = "Platform (instagram|facebook)"),
        ("from" = Option<String>, Query, description = "First date YYYY-MM-DD (default: today - 30 days)"),
        ("to" = Option<String>, Query, description = "Last date YYYY-MM-DD, inclusive (default: today + 7 days)")
    ),
    responses(
        (status = 200, description = "Plans in the range, oldest first", body = [PlanListItem]),
        (status = 422, description = "Bad range (from after to, or more than 366 days)")
    )
)]
pub async fn list_plans(
    State(s): State<AppState>,
    user: AuthUser,
    Path((pid, p)): Path<(Uuid, String)>,
    Query(q): Query<PlanListQuery>,
) -> AppResult<Json<Vec<PlanListItem>>> {
    let platform =
        Platform::parse(&p).ok_or_else(|| AppError::Invalid(format!("unknown platform: {p}")))?;
    access::ensure_project_ready(&s.db, &user.id, pid).await?;
    ensure_workspace(&s.db, pid, platform).await?;
    let today = project_today(&s.db, pid).await?;
    let from = q.from.unwrap_or(today - Duration::days(30));
    let to = q.to.unwrap_or(today + Duration::days(7));
    if from > to {
        return Err(AppError::Invalid("from must be on or before to".into()));
    }
    if (to - from).num_days() > MAX_PLAN_LIST_DAYS {
        return Err(AppError::Invalid(format!(
            "range must be at most {MAX_PLAN_LIST_DAYS} days"
        )));
    }
    let rows: Vec<(Uuid, NaiveDate, String, String, Option<String>)> = sqlx::query_as(
        "select id, for_date, status, title, case when title = '' then markdown end \
           from daily_plans \
          where project_id = $1 and platform = $2 and for_date between $3 and $4 \
          order by for_date",
    )
    .bind(pid)
    .bind(platform.as_str())
    .bind(from)
    .bind(to)
    .fetch_all(&s.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, for_date, status, title, markdown)| PlanListItem {
                id,
                for_date,
                status,
                title: match markdown {
                    Some(md) => plan_format::parse(&md).title.unwrap_or_default(),
                    None => title,
                },
            })
            .collect(),
    ))
}

#[utoipa::path(
    get,
    path = "/plans/{id}",
    params(("id" = Uuid, Path, description = "Plan id")),
    responses((status = 200, description = "Plan", body = PlanView), (status = 404, description = "Not found"))
)]
pub async fn get_plan(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<PlanView>> {
    access::ensure_plan_owner(&s.db, &user.id, id).await?;
    let plan = require_plan(&s.db, id).await?;
    Ok(Json(load_plan_view(&s, plan, vec![]).await?))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PlanVersionView {
    pub version: i32,
    pub reason: String,
    pub greeting: String,
    pub markdown: String,
    pub created_at: DateTime<Utc>,
}

#[utoipa::path(
    get,
    path = "/plans/{id}/versions",
    params(("id" = Uuid, Path, description = "Plan id")),
    responses((status = 200, description = "Markdown history, newest first", body = [PlanVersionView]))
)]
pub async fn list_versions(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Vec<PlanVersionView>>> {
    access::ensure_plan_owner(&s.db, &user.id, id).await?;
    require_plan(&s.db, id).await?;
    let rows: Vec<(i32, String, String, String, DateTime<Utc>)> = sqlx::query_as(
        "select version, reason, greeting, markdown, created_at from plan_versions \
          where plan_id = $1 order by version desc limit 100",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(
                |(version, reason, greeting, markdown, created_at)| PlanVersionView {
                    version,
                    reason,
                    greeting,
                    markdown,
                    created_at,
                },
            )
            .collect(),
    ))
}

// ── Draft on demand ──────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
pub struct DraftPlanRequest {
    /// Plan date (project timezone). Defaults to tomorrow.
    pub for_date: Option<NaiveDate>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DraftPlanResponse {
    pub for_date: NaiveDate,
    /// False when a job for this date was already queued.
    pub enqueued: bool,
}

#[utoipa::path(
    post,
    path = "/projects/{pid}/platforms/{p}/plans/draft",
    params(
        ("pid" = Uuid, Path, description = "Project id"),
        ("p" = String, Path, description = "Platform")
    ),
    request_body = DraftPlanRequest,
    responses((status = 202, description = "PlanDay job queued", body = DraftPlanResponse))
)]
pub async fn draft_plan(
    State(s): State<AppState>,
    user: AuthUser,
    Path((pid, p)): Path<(Uuid, String)>,
    Json(body): Json<DraftPlanRequest>,
) -> AppResult<(StatusCode, Json<DraftPlanResponse>)> {
    let platform =
        Platform::parse(&p).ok_or_else(|| AppError::Invalid(format!("unknown platform: {p}")))?;
    access::ensure_project_ready(&s.db, &user.id, pid).await?;
    ensure_workspace(&s.db, pid, platform).await?;
    let today = project_today(&s.db, pid).await?;
    let for_date = body.for_date.unwrap_or(today + Duration::days(1));
    if for_date < today {
        return Err(AppError::Invalid("for_date is in the past".into()));
    }
    if let Some(existing) = plan::get_plan_for_date(&s.db, pid, platform, for_date)
        .await
        .map_err(AppError::Other)?
        && !matches!(existing.status.as_str(), "waiting_for_data" | "failed")
    {
        let hint = if existing.status == PlanStatus::NoOp.as_str() {
            " Resume it with POST /plans/{id}/resume."
        } else {
            ""
        };
        return Err(AppError::Conflict(format!(
            "A plan for {for_date} already exists ({}).{hint}",
            existing.status
        )));
    }
    let queue = DataJobQueue::new(s.db.clone());
    let mut tx = s.db.begin().await?;
    let id = queue
        .enqueue(
            &mut tx,
            &DataJob::DraftPlan {
                project_id: pid,
                platform,
                for_date,
                allow_no_op: false,
            },
            Some(format!(
                "draft_plan:{pid}:{}:{for_date}:{}",
                platform.as_str(),
                Utc::now().format("%Y%m%d%H")
            )),
            Utc::now(),
        )
        .await?;
    tx.commit().await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(DraftPlanResponse {
            for_date,
            enqueued: id.is_some(),
        }),
    ))
}

// ── Edit markdown ────────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
pub struct PutPlanRequest {
    pub markdown: String,
    /// The `plan.version` this edit is based on (optimistic concurrency).
    pub version: i32,
}

#[derive(Debug, sqlx::FromRow)]
struct LockedPlan {
    id: Uuid,
    project_id: Uuid,
    platform: String,
    for_date: NaiveDate,
    status: String,
    version: i32,
    markdown: String,
}

async fn lock_plan(tx: &mut sqlx::PgConnection, id: Uuid) -> AppResult<LockedPlan> {
    Ok(sqlx::query_as::<_, LockedPlan>(
        "select id, project_id, platform, for_date, status, version, markdown \
           from daily_plans where id = $1 for update",
    )
    .bind(id)
    .fetch_one(tx)
    .await?)
}

/// Warnings for `@path` references that do not resolve to a file or directory.
async fn missing_ref_warnings(
    db: &PgPool,
    project_id: Uuid,
    parsed: &plan_format::ParsedPlan,
) -> Vec<String> {
    let refs = plan_format::referenced_paths(parsed);
    if refs.is_empty() {
        return vec![];
    }
    let paths: Vec<String> = refs.iter().map(|(_, p)| p.clone()).collect();
    match assets::missing_paths(db, project_id, &paths).await {
        Ok(missing) => refs
            .iter()
            .filter(|(_, p)| {
                assets::normalize_path(p)
                    .map(|n| missing.contains(&n))
                    .unwrap_or(true)
            })
            .map(|(k, p)| format!("{k}: @{p} does not exist in the asset manager"))
            .collect(),
        Err(_) => vec![],
    }
}

/// Re-parse the markdown, upsert videos, bump the version, and record the history row.
/// Runs inside the caller's locked transaction.
async fn apply_markdown(
    s: &AppState,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    locked: &LockedPlan,
    markdown: &str,
    reason: &str,
    opts: &ValidateOpts,
) -> AppResult<(plan_format::ParsedPlan, plan::SyncResult)> {
    let healed =
        plan_format::strip_typography_dashes(&plan_format::split_compound_video_lines(markdown));
    let markdown = healed.as_str();
    let parsed =
        plan_format::parse_and_validate(markdown, opts).map_err(|e| issues_to_error(&e))?;
    let audiences = list_active_audiences(&s.db, locked.project_id)
        .await
        .map_err(AppError::Other)?;
    let platform = Platform::parse(&locked.platform)
        .ok_or_else(|| AppError::Other(anyhow::anyhow!("bad platform")))?;
    let trends_items =
        plan::trend_items_for(&s.db, locked.project_id, platform.as_str(), locked.for_date)
            .await
            .map_err(AppError::Other)?;

    // Audience-track videos must reference a known audience.
    for v in &parsed.videos {
        if v.track == Some(PlanTrack::Audience)
            && let Some(raw) = &v.audience
            && plan::resolve_audience(&audiences, raw).is_none()
        {
            return Err(AppError::Invalid(format!(
                "[{}] unknown audience \"{raw}\" (use 1-{} or an audience name)",
                v.anchor,
                audiences.len()
            )));
        }
    }

    let sync = plan::sync_videos(
        tx,
        locked.id,
        &parsed,
        &SyncCtx {
            audiences: &audiences,
            trends: &trends_items,
        },
    )
    .await
    .map_err(AppError::Other)?;

    let new_version = locked.version + 1;
    let (title, summary) = plan::title_and_summary(&parsed);
    sqlx::query(
        "update daily_plans set markdown = $2, version = $3, title = $4, summary = $5, \
                updated_at = now() \
          where id = $1",
    )
    .bind(locked.id)
    .bind(markdown)
    .bind(new_version)
    .bind(&title)
    .bind(&summary)
    .execute(&mut **tx)
    .await?;
    insert_plan_version(tx, locked.id, new_version, markdown, "", reason)
        .await
        .map_err(AppError::Other)?;

    // A built plan with new/changed videos is no longer fully rendered.
    if locked.status == "built" {
        let unrendered: bool = sqlx::query_scalar(
            "select exists(select 1 from plan_videos pv \
               left join video_projects vp on vp.plan_video_id = pv.id \
              where pv.plan_id = $1 \
                and (vp.id is null or vp.render_rev is distinct from vp.rev \
                     or vp.render_blob_path is null))",
        )
        .bind(locked.id)
        .fetch_one(&mut **tx)
        .await?;
        if unrendered {
            sqlx::query("update daily_plans set status = 'review' where id = $1")
                .bind(locked.id)
                .execute(&mut **tx)
                .await?;
        }
    }
    Ok((parsed, sync))
}

#[utoipa::path(
    put,
    path = "/plans/{id}",
    params(("id" = Uuid, Path, description = "Plan id")),
    request_body = PutPlanRequest,
    responses(
        (status = 200, description = "Saved; videos re-parsed and upserted", body = PlanView),
        (status = 409, description = "Version mismatch, past plan, or locked plan"),
        (status = 422, description = "Markdown failed validation")
    )
)]
pub async fn put_plan(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<PutPlanRequest>,
) -> AppResult<Json<PlanView>> {
    access::ensure_plan_owner(&s.db, &user.id, id).await?;
    let mut tx = s.db.begin().await?;
    let locked = lock_plan(&mut tx, id).await?;
    let plan = require_plan(&s.db, id).await?;
    require_editable(&s.db, &plan).await?;
    if locked.version != body.version {
        return Err(AppError::Conflict(format!(
            "Plan changed (version {}); reload and re-apply your edit.",
            locked.version
        )));
    }
    if locked.markdown == body.markdown {
        tx.rollback().await?;
        return Ok(Json(load_plan_view(&s, plan, vec![]).await?));
    }
    let (parsed, sync) = apply_markdown(
        &s,
        &mut tx,
        &locked,
        &body.markdown,
        "user_edit",
        &ValidateOpts::default(),
    )
    .await?;
    tx.commit().await?;

    delete_blobs(&s, &sync.orphaned_blobs).await;
    let mut warnings = sync.warnings;
    warnings.extend(missing_ref_warnings(&s.db, locked.project_id, &parsed).await);
    let plan = require_plan(&s.db, id).await?;
    Ok(Json(load_plan_view(&s, plan, warnings).await?))
}

async fn delete_blobs(s: &AppState, paths: &[String]) {
    for b in paths {
        if let Err(e) = s.blob.delete(b).await {
            tracing::warn!(error = %e, blob = %b, "blob delete failed");
        }
    }
}

/// Distinguishes an absent field (`None`) from an explicit `null` (`Some(None)`).
fn double_option<'de, D, T>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(de).map(Some)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ChecklistRequest {
    /// Checklist key `{anchor}#{n}`, e.g. `video-1#2`.
    pub key: String,
    /// open | pending | done (also accepts queued | confirmed). Omit to keep the marker.
    pub state: Option<String>,
    /// Attach this asset path as an `@"path"` reference (usually together with `done`).
    pub asset_path: Option<String>,
    /// Generation model id (`[a-z0-9._/-]{1,80}`); `null` removes the `- model:` line.
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<String>, nullable)]
    pub model: Option<Option<String>>,
    /// Text for a `<!-- REGENERATE: … -->` mark; `null` removes it.
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<String>, nullable)]
    pub regenerate_note: Option<Option<String>>,
}

#[utoipa::path(
    put,
    path = "/plans/{id}/checklist",
    params(("id" = Uuid, Path, description = "Plan id")),
    request_body = ChecklistRequest,
    responses((status = 200, description = "Checklist item updated in the markdown", body = PlanView))
)]
pub async fn put_checklist(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<ChecklistRequest>,
) -> AppResult<Json<PlanView>> {
    access::ensure_plan_owner(&s.db, &user.id, id).await?;
    let state = body
        .state
        .as_deref()
        .map(|st| {
            ChecklistState::parse(st)
                .ok_or_else(|| AppError::Invalid("state must be open, pending or done".into()))
        })
        .transpose()?;
    let mut tx = s.db.begin().await?;
    let locked = lock_plan(&mut tx, id).await?;
    let plan = require_plan(&s.db, id).await?;
    require_editable(&s.db, &plan).await?;

    // Queuing a video for generation (open → pending, no attached asset yet) requires
    // every declared input to already be confirmed / present.
    if state == Some(ChecklistState::Pending) && body.asset_path.is_none() {
        let mut parsed = plan_format::parse(&locked.markdown);
        let known = known_assets_for(&s.db, locked.project_id).await?;
        plan_format::annotate_generation_readiness(&mut parsed, &known);
        if let Some(item) = parsed
            .videos
            .iter()
            .flat_map(|v| v.checklist.iter())
            .find(|c| c.key == body.key)
        {
            if item.kind == ChecklistKind::Video && !item.generation_ready {
                let detail = item
                    .generation_blockers
                    .iter()
                    .map(|b| format!("{}: {}", b.target, b.reason))
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(AppError::Conflict(format!(
                    "Cannot queue {}: required inputs are not ready. {detail}",
                    body.key
                )));
            }
        }
    }

    let attach = match body.asset_path.as_deref() {
        Some(p) => Some(assets::normalize_path(p).map_err(AppError::Invalid)?),
        None => None,
    };
    let edit = ChecklistEdit {
        state,
        attach_path: attach.as_deref(),
        model: body
            .model
            .as_ref()
            .map(|m| m.as_deref().map(str::trim).filter(|m| !m.is_empty())),
        regenerate_note: body.regenerate_note.as_ref().map(|n| n.as_deref()),
    };
    let md =
        plan_format::edit_checklist_item(&locked.markdown, &body.key, &edit).map_err(
            |e| match e {
                ChecklistEditError::UnknownKey => {
                    AppError::Invalid(format!("unknown checklist key {}", body.key))
                }
                ChecklistEditError::Invalid(msg) => AppError::Invalid(msg),
            },
        )?;
    if md != locked.markdown {
        apply_markdown(
            &s,
            &mut tx,
            &locked,
            &md,
            "checklist",
            &ValidateOpts::default(),
        )
        .await?;
    }
    tx.commit().await?;
    let plan = require_plan(&s.db, id).await?;
    Ok(Json(load_plan_view(&s, plan, vec![]).await?))
}

// ── Status ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
pub struct StatusRequest {
    /// review | generating_assets | building | built | no_op.
    /// `review` on a no-op plan resumes it (same as `POST /plans/{id}/resume`).
    pub status: String,
    /// For `no_op`: short reason (default `user`).
    pub reason: Option<String>,
    /// For `no_op`: why nothing is posted today.
    pub explanation: Option<String>,
}

const MAX_NO_OP_REASON_CHARS: usize = 64;
const MAX_NO_OP_EXPLANATION_CHARS: usize = 1000;

/// Mark a plan as a no-op day (allowed from any state before `scheduled`).
async fn mark_no_op(
    s: &AppState,
    id: Uuid,
    reason: Option<&str>,
    explanation: Option<&str>,
) -> AppResult<PlanView> {
    let reason = reason.map(str::trim).filter(|r| !r.is_empty());
    if reason.is_some_and(|r| r.chars().count() > MAX_NO_OP_REASON_CHARS) {
        return Err(AppError::Invalid(format!(
            "reason must be at most {MAX_NO_OP_REASON_CHARS} characters"
        )));
    }
    let explanation = explanation.map(str::trim).filter(|e| !e.is_empty());
    if explanation.is_some_and(|e| e.chars().count() > MAX_NO_OP_EXPLANATION_CHARS) {
        return Err(AppError::Invalid(format!(
            "explanation must be at most {MAX_NO_OP_EXPLANATION_CHARS} characters"
        )));
    }
    let mut tx = s.db.begin().await?;
    let locked = lock_plan(&mut tx, id).await?;
    let from = PlanStatus::parse(&locked.status)
        .ok_or_else(|| AppError::Other(anyhow::anyhow!("unknown plan status")))?;
    if from != PlanStatus::NoOp {
        if !from.can_mark_no_op() {
            return Err(AppError::Conflict(format!(
                "A {} plan cannot be skipped.",
                from.as_str()
            )));
        }
        if locked.for_date < project_today(&s.db, locked.project_id).await? {
            return Err(AppError::Conflict("Past plans are read-only.".into()));
        }
        sqlx::query(
            "update daily_plans set status = 'no_op', no_op_reason = $2, no_op_explanation = $3, \
                    updated_at = now() \
              where id = $1",
        )
        .bind(id)
        .bind(reason.unwrap_or(NO_OP_REASON_USER))
        .bind(explanation)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    let plan = require_plan(&s.db, id).await?;
    load_plan_view(s, plan, vec![]).await
}

/// Leave a no-op day: back to review when videos exist, otherwise draft a fresh plan
/// (PlanDay may not choose no-op again).
async fn resume_plan(s: &AppState, id: Uuid) -> AppResult<PlanView> {
    let mut tx = s.db.begin().await?;
    let locked = lock_plan(&mut tx, id).await?;
    if locked.status != PlanStatus::NoOp.as_str() {
        return Err(AppError::Conflict(format!(
            "Only no-op plans can be resumed (this one is {}).",
            locked.status
        )));
    }
    if locked.for_date < project_today(&s.db, locked.project_id).await? {
        return Err(AppError::Conflict("Past plans are read-only.".into()));
    }
    let videos: i64 = sqlx::query_scalar("select count(*) from plan_videos where plan_id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    let next = if videos > 0 {
        PlanStatus::Review
    } else {
        PlanStatus::WaitingForData
    };
    sqlx::query(
        "update daily_plans set status = $2, no_op_reason = null, no_op_explanation = null, \
                updated_at = now() \
          where id = $1",
    )
    .bind(id)
    .bind(next.as_str())
    .execute(&mut *tx)
    .await?;
    if next == PlanStatus::WaitingForData {
        let platform = Platform::parse(&locked.platform)
            .ok_or_else(|| AppError::Other(anyhow::anyhow!("bad platform")))?;
        let now = Utc::now();
        DataJobQueue::new(s.db.clone())
            .enqueue(
                &mut tx,
                &DataJob::DraftPlan {
                    project_id: locked.project_id,
                    platform,
                    for_date: locked.for_date,
                    allow_no_op: false,
                },
                Some(format!(
                    "draft_plan:{}:{}:{}:resume:{}",
                    locked.project_id,
                    platform.as_str(),
                    locked.for_date,
                    now.timestamp()
                )),
                now,
            )
            .await?;
    }
    tx.commit().await?;
    let plan = require_plan(&s.db, id).await?;
    load_plan_view(s, plan, vec![]).await
}

#[utoipa::path(
    post,
    path = "/plans/{id}/resume",
    params(("id" = Uuid, Path, description = "Plan id")),
    responses(
        (status = 200, description = "Back in review (videos exist) or waiting_for_data with a fresh PlanDay draft queued", body = PlanView),
        (status = 409, description = "Plan is not a no-op, or is in the past")
    )
)]
pub async fn resume(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<PlanView>> {
    access::ensure_plan_owner(&s.db, &user.id, id).await?;
    Ok(Json(resume_plan(&s, id).await?))
}

/// Videos whose render is missing or stale (anchors).
async fn unrendered_videos(db: &PgPool, plan_id: Uuid) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "select pv.anchor from plan_videos pv \
           left join video_projects vp on vp.plan_video_id = pv.id \
          where pv.plan_id = $1 \
            and (vp.id is null or vp.render_rev is distinct from vp.rev \
                 or vp.render_blob_path is null) \
          order by pv.position",
    )
    .bind(plan_id)
    .fetch_all(db)
    .await?)
}

#[utoipa::path(
    post,
    path = "/plans/{id}/status",
    params(("id" = Uuid, Path, description = "Plan id")),
    request_body = StatusRequest,
    responses(
        (status = 200, description = "Status changed", body = PlanView),
        (status = 409, description = "Illegal transition or videos not rendered")
    )
)]
pub async fn set_status(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<StatusRequest>,
) -> AppResult<Json<PlanView>> {
    access::ensure_plan_owner(&s.db, &user.id, id).await?;
    let to = PlanStatus::parse(&body.status)
        .ok_or_else(|| AppError::Invalid(format!("unknown status {}", body.status)))?;
    if to == PlanStatus::NoOp {
        return Ok(Json(
            mark_no_op(&s, id, body.reason.as_deref(), body.explanation.as_deref()).await?,
        ));
    }
    let mut tx = s.db.begin().await?;
    let locked = lock_plan(&mut tx, id).await?;
    if locked.status == PlanStatus::NoOp.as_str() && to == PlanStatus::Review {
        tx.rollback().await?;
        return Ok(Json(resume_plan(&s, id).await?));
    }
    let plan = require_plan(&s.db, id).await?;
    let from = PlanStatus::parse(&locked.status)
        .ok_or_else(|| AppError::Other(anyhow::anyhow!("unknown plan status")))?;
    if from != to {
        if !from.can_transition_to(to) {
            return Err(AppError::Conflict(format!(
                "Cannot move a plan from {} to {}.",
                from.as_str(),
                to.as_str()
            )));
        }
        if from != PlanStatus::Failed {
            require_editable(&s.db, &plan).await?;
        }
        if matches!(
            to,
            PlanStatus::GeneratingAssets | PlanStatus::Building | PlanStatus::Built
        ) {
            let n: i64 = sqlx::query_scalar("select count(*) from plan_videos where plan_id = $1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
            if n == 0 {
                return Err(AppError::Conflict("The plan has no videos yet.".into()));
            }
        }
        if to == PlanStatus::Built {
            let missing = unrendered_videos(&s.db, id).await?;
            if !missing.is_empty() {
                return Err(AppError::Conflict(format!(
                    "Videos not rendered for the current timeline: {}",
                    missing.join(", ")
                )));
            }
        }
        sqlx::query("update daily_plans set status = $2, updated_at = now() where id = $1")
            .bind(id)
            .bind(to.as_str())
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    let plan = require_plan(&s.db, id).await?;
    Ok(Json(load_plan_view(&s, plan, vec![]).await?))
}

// ── Comments ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
pub struct CommentRequest {
    /// Section anchor, e.g. `video-1` or `strategy`.
    pub anchor: String,
    pub body: String,
    /// user (default) | agent
    pub author: Option<String>,
}

#[utoipa::path(
    get,
    path = "/plans/{id}/comments",
    params(("id" = Uuid, Path, description = "Plan id")),
    responses((status = 200, description = "Comments by creation time", body = [PlanComment]))
)]
pub async fn list_comments(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Vec<PlanComment>>> {
    access::ensure_plan_owner(&s.db, &user.id, id).await?;
    require_plan(&s.db, id).await?;
    Ok(Json(
        plan::list_comments(&s.db, id)
            .await
            .map_err(AppError::Other)?,
    ))
}

#[utoipa::path(
    post,
    path = "/plans/{id}/comments",
    params(("id" = Uuid, Path, description = "Plan id")),
    request_body = CommentRequest,
    responses((status = 201, description = "Created", body = PlanComment))
)]
pub async fn post_comment(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<CommentRequest>,
) -> AppResult<(StatusCode, Json<PlanComment>)> {
    access::ensure_plan_owner(&s.db, &user.id, id).await?;
    let plan = require_plan(&s.db, id).await?;
    require_editable(&s.db, &plan).await?;
    let text = body.body.trim();
    if text.is_empty() || text.chars().count() > 4000 {
        return Err(AppError::Invalid("body must be 1–4000 characters".into()));
    }
    let author = match body.author.as_deref().unwrap_or("user") {
        a @ ("user" | "agent") => a.to_owned(),
        other => {
            return Err(AppError::Invalid(format!(
                "author must be user or agent, got {other}"
            )));
        }
    };
    let anchor = body.anchor.trim().trim_start_matches('#').to_owned();
    let parsed = plan_format::parse(&plan.markdown);
    if !parsed.sections.iter().any(|sec| {
        sec.anchor
            .as_deref()
            .is_some_and(|a| a.eq_ignore_ascii_case(&anchor))
    }) {
        return Err(AppError::Invalid(format!(
            "no section with anchor \"{anchor}\" in this plan"
        )));
    }
    let c = sqlx::query_as::<_, PlanComment>(
        "insert into plan_comments (id, plan_id, anchor, body, author) values ($1,$2,$3,$4,$5) \
         returning id, plan_id, anchor, body, author, created_at",
    )
    .bind(new_id())
    .bind(id)
    .bind(&anchor)
    .bind(text)
    .bind(&author)
    .fetch_one(&s.db)
    .await?;
    Ok((StatusCode::CREATED, Json(c)))
}

#[utoipa::path(
    delete,
    path = "/plan-comments/{id}",
    params(("id" = Uuid, Path, description = "Comment id")),
    responses((status = 204, description = "Deleted"))
)]
pub async fn delete_comment(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    let plan_id = access::ensure_comment_owner(&s.db, &user.id, id).await?;
    let plan = require_plan(&s.db, plan_id).await?;
    require_editable(&s.db, &plan).await?;
    sqlx::query("delete from plan_comments where id = $1")
        .bind(id)
        .execute(&s.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ── Chat persistence ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct MessagesQuery {
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PostMessageRequest {
    /// user | agent | system (default user)
    pub role: Option<String>,
    /// UI message parts (JSON array, AI-SDK style), stored verbatim.
    #[schema(value_type = Vec<Object>)]
    pub parts: Value,
}

#[utoipa::path(
    get,
    path = "/plans/{id}/messages",
    params(
        ("id" = Uuid, Path, description = "Plan id"),
        ("limit" = Option<i64>, Query, description = "Max messages (default 500)")
    ),
    responses((status = 200, description = "Chat messages, oldest first", body = [PlanMessage]))
)]
pub async fn list_messages(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Query(q): Query<MessagesQuery>,
) -> AppResult<Json<Vec<PlanMessage>>> {
    access::ensure_plan_owner(&s.db, &user.id, id).await?;
    require_plan(&s.db, id).await?;
    let rows = sqlx::query_as::<_, PlanMessage>(
        "select * from (select id, plan_id, role, parts, created_at from plan_messages \
          where plan_id = $1 order by created_at desc limit $2) t order by created_at",
    )
    .bind(id)
    .bind(q.limit.unwrap_or(500).clamp(1, 2000))
    .fetch_all(&s.db)
    .await?;
    Ok(Json(rows))
}

#[utoipa::path(
    post,
    path = "/plans/{id}/messages",
    params(("id" = Uuid, Path, description = "Plan id")),
    request_body = PostMessageRequest,
    responses((status = 201, description = "Stored", body = PlanMessage))
)]
pub async fn post_message(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<PostMessageRequest>,
) -> AppResult<(StatusCode, Json<PlanMessage>)> {
    access::ensure_plan_owner(&s.db, &user.id, id).await?;
    let plan = require_plan(&s.db, id).await?;
    require_editable(&s.db, &plan).await?;
    let role = match body.role.as_deref().unwrap_or("user") {
        r @ ("user" | "agent" | "system") => r.to_owned(),
        other => {
            return Err(AppError::Invalid(format!(
                "role must be user, agent or system, got {other}"
            )));
        }
    };
    if !body.parts.is_array() {
        return Err(AppError::Invalid("parts must be a JSON array".into()));
    }
    if serde_json::to_vec(&body.parts)
        .map(|v| v.len())
        .unwrap_or(0)
        > 256 * 1024
    {
        return Err(AppError::Invalid("parts exceed 256 KB".into()));
    }
    let m = sqlx::query_as::<_, PlanMessage>(
        "insert into plan_messages (id, plan_id, role, parts) values ($1,$2,$3,$4) \
         returning id, plan_id, role, parts, created_at",
    )
    .bind(new_id())
    .bind(id)
    .bind(&role)
    .bind(&body.parts)
    .fetch_one(&s.db)
    .await?;
    Ok((StatusCode::CREATED, Json(m)))
}

// ── Schedule ─────────────────────────────────────────────────────────

/// Pick prime windows + dayparts for scheduling, filling empty prime windows from the
/// industry prior (or legacy slots) and persisting them.
async fn scheduling_windows(
    db: &PgPool,
    plan: &DailyPlan,
) -> AppResult<(Vec<PrimeWindow>, DaypartWindows)> {
    let mut schedule = load_schedule_with_dayparts(db, plan.project_id, &plan.platform)
        .await
        .map_err(AppError::Other)?
        .ok_or_else(|| {
            AppError::Conflict(
                "No posting schedule yet — wait for industry classification or configure schedule."
                    .into(),
            )
        })?;
    let mut prime_windows: Vec<PrimeWindow> = if let Some(ov) = &plan.prime_window_override {
        serde_json::from_value(ov.clone()).map_err(|e| AppError::Invalid(e.to_string()))?
    } else {
        schedule.prime_windows.clone()
    };
    if prime_windows.is_empty() && plan.prime_window_override.is_none() {
        let industry: Option<String> =
            sqlx::query_scalar("select industry from projects where id = $1")
                .bind(plan.project_id)
                .fetch_one(db)
                .await?;
        prime_windows = industry
            .as_deref()
            .and_then(Industry::parse)
            .map(prime_windows_for_industry)
            .unwrap_or_default();
        if prime_windows.is_empty() {
            prime_windows = prime_windows_from_slots(&schedule.slots);
        }
        if prime_windows.is_empty() {
            return Err(AppError::Conflict(
                "Posting schedule has no prime windows configured.".into(),
            ));
        }
        let filled_from = if industry.as_deref().and_then(Industry::parse).is_some() {
            "industry prior"
        } else {
            "legacy slots"
        };
        schedule = upsert_schedule_full(
            db,
            plan.project_id,
            &plan.platform,
            &schedule.slots,
            &prime_windows,
            &schedule.dayparts,
            schedule.source,
            &format!(
                "{}; filled empty prime_windows from {filled_from}",
                schedule.rationale
            ),
        )
        .await
        .map_err(AppError::Other)?;
    }
    Ok((prime_windows, schedule.dayparts))
}

/// Order videos into per-track ranks and dayparts for [`assign_times`].
///
/// Explicit dayparts are honoured (non-prime videos never get rank 1 so they are not pulled
/// into the prime window). Videos without a daypart go: first of each track → prime, then
/// morning/afternoon/evening/night round-robin.
fn schedulable_items(videos: &[PlanVideo]) -> Vec<SchedulableItem> {
    let mut out = Vec::new();
    for track in [PlanTrack::Audience, PlanTrack::Trend] {
        let mut in_track: Vec<&PlanVideo> = videos
            .iter()
            .filter(|v| v.track == track.as_str())
            .collect();
        in_track.sort_by_key(|v| v.position);
        let mut auto_idx = 0usize;
        for (i, v) in in_track.iter().enumerate() {
            let explicit = v.daypart.as_deref().and_then(Daypart::parse);
            let (rank, daypart) = match explicit {
                Some(Daypart::Prime) => (1, Daypart::Prime),
                Some(dp) => (2 + i as i32, dp),
                None => {
                    let r = auto_idx;
                    auto_idx += 1;
                    if r == 0 {
                        (1, Daypart::Prime)
                    } else {
                        (
                            2 + r as i32,
                            Daypart::non_prime()[(r - 1) % Daypart::non_prime().len()],
                        )
                    }
                }
            };
            out.push(SchedulableItem {
                id: v.id,
                track: track.as_str().to_owned(),
                rank,
                daypart,
            });
        }
    }
    out
}

#[utoipa::path(
    post,
    path = "/plans/{id}/schedule",
    params(("id" = Uuid, Path, description = "Plan id")),
    responses(
        (status = 200, description = "Videos scheduled; plan locked", body = PlanView),
        (status = 409, description = "Not built, renders stale, platform not connected, or no time window")
    )
)]
pub async fn schedule_plan(
    State(s): State<AppState>,
    user: AuthUser,
    Path(plan_id): Path<Uuid>,
) -> AppResult<Json<PlanView>> {
    access::ensure_plan_owner(&s.db, &user.id, plan_id).await?;
    let plan = require_plan(&s.db, plan_id).await?;
    let status = PlanStatus::parse(&plan.status)
        .ok_or_else(|| AppError::Other(anyhow::anyhow!("unknown plan status")))?;
    if status != PlanStatus::Built {
        return Err(AppError::Conflict(format!(
            "Plan must be built to schedule (it is {}).",
            plan.status
        )));
    }
    require_editable(&s.db, &plan).await?;
    let platform = Platform::parse(&plan.platform)
        .ok_or_else(|| AppError::Other(anyhow::anyhow!("bad platform")))?;

    let connected: bool = sqlx::query_scalar(
        "select exists(select 1 from platform_connections \
          where project_id = $1 and platform = $2 and status = 'connected')",
    )
    .bind(plan.project_id)
    .bind(&plan.platform)
    .fetch_one(&s.db)
    .await?;
    if !connected {
        let label = if platform == Platform::Instagram {
            "Connect Instagram before scheduling the plan."
        } else {
            "Connect Facebook before scheduling the plan."
        };
        return Err(AppError::Conflict(label.into()));
    }

    let videos = plan::list_videos(&s.db, plan_id)
        .await
        .map_err(AppError::Other)?;
    if videos.is_empty() {
        return Err(AppError::Conflict("The plan has no videos.".into()));
    }

    // Every video must have a render that matches its current timeline revision.
    let renders: Vec<(Uuid, String, i32, Option<i32>, Option<String>)> = sqlx::query_as(
        "select pv.id, pv.anchor, coalesce(vp.rev, 0), vp.render_rev, vp.render_blob_path \
           from plan_videos pv left join video_projects vp on vp.plan_video_id = pv.id \
          where pv.plan_id = $1 order by pv.position",
    )
    .bind(plan_id)
    .fetch_all(&s.db)
    .await?;
    let mut stale: Vec<String> = Vec::new();
    for (_, anchor, rev, render_rev, blob) in &renders {
        let ok = *rev > 0
            && *render_rev == Some(*rev)
            && match blob {
                Some(b) => s
                    .blob
                    .size(b)
                    .await
                    .map_err(AppError::Other)?
                    .is_some_and(|n| n > 0),
                None => false,
            };
        if !ok {
            stale.push(anchor.clone());
        }
    }
    if !stale.is_empty() {
        return Err(AppError::Conflict(format!(
            "Render each video at its latest timeline revision before scheduling: {}",
            stale.join(", ")
        )));
    }

    let (prime_windows, dayparts) = scheduling_windows(&s.db, &plan).await?;
    let timezone: String = sqlx::query_scalar("select timezone from projects where id = $1")
        .bind(plan.project_id)
        .fetch_one(&s.db)
        .await?;
    let since = plan.for_date - Duration::days(30);
    let online = online_followers_by_hour(&s.db, plan.project_id, since, &timezone)
        .await
        .map_err(AppError::Other)?;

    let occupied: Vec<DateTime<Utc>> = sqlx::query_scalar(
        "select scheduled_for from posts \
          where project_id = $1 and platform = $2 and scheduled_for is not null \
            and status in ('scheduled', 'pending') and plan_video_id <> all($3)",
    )
    .bind(plan.project_id)
    .bind(&plan.platform)
    .bind(videos.iter().map(|v| v.id).collect::<Vec<_>>())
    .fetch_all(&s.db)
    .await?;

    let items = schedulable_items(&videos);
    let now = s.clock.now();
    let assign = assign_times(
        &items,
        plan.for_date,
        &prime_windows,
        &dayparts,
        &online,
        &occupied,
        &timezone,
        now,
        Duration::minutes(10),
    )
    .map_err(AppError::Invalid)?;
    if !assign.missed.is_empty() {
        return Err(AppError::Conflict(format!(
            "No posting window left on {} for {} video(s); move the plan to a later date.",
            plan.for_date,
            assign.missed.len()
        )));
    }

    if platform == Platform::Facebook {
        enforce_fb_reels_limit(
            &s.db,
            &plan,
            &assign
                .assigned
                .iter()
                .map(|a| a.scheduled_for)
                .collect::<Vec<_>>(),
            &videos,
        )
        .await?;
    }

    let mut tx = s.db.begin().await?;
    for at in &assign.assigned {
        let video = videos
            .iter()
            .find(|v| v.id == at.item_id)
            .ok_or(AppError::NotFound)?;
        sqlx::query("update plan_videos set scheduled_for = $2 where id = $1")
            .bind(video.id)
            .bind(at.scheduled_for)
            .execute(&mut *tx)
            .await?;
        links::ensure_tracked_link(&mut tx, video.id, plan.project_id, platform).await?;

        let row: Option<(Uuid, i32)> = sqlx::query_as(
            r#"
            insert into posts (id, plan_video_id, project_id, platform, status, scheduled_for)
            values ($1, $2, $3, $4, 'scheduled', $5)
            on conflict (plan_video_id) do update
               set status = excluded.status,
                   scheduled_for = excluded.scheduled_for,
                   attempt = posts.attempt + 1,
                   error = null,
                   container_id = null,
                   container_polls = 0
             where posts.status = 'failed'
            returning id, attempt
            "#,
        )
        .bind(new_id())
        .bind(video.id)
        .bind(plan.project_id)
        .bind(&plan.platform)
        .bind(at.scheduled_for)
        .fetch_optional(&mut *tx)
        .await?;
        let (post_id, attempt) = match row {
            Some(r) => r,
            None => {
                sqlx::query_as("select id, attempt from posts where plan_video_id = $1")
                    .bind(video.id)
                    .fetch_one(&mut *tx)
                    .await?
            }
        };
        s.user_jobs
            .enqueue_at_tx(
                &mut tx,
                &UserJob::PublishVideo { post_id, attempt },
                Some(format!("publish:{post_id}:{attempt}")),
                at.scheduled_for,
            )
            .await?;
    }
    sqlx::query(
        "update daily_plans set status = 'scheduled', scheduled_at = now(), updated_at = now() \
          where id = $1",
    )
    .bind(plan_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let plan = require_plan(&s.db, plan_id).await?;
    Ok(Json(load_plan_view(&s, plan, vec![]).await?))
}

/// Reject schedules that would exceed the Facebook Reels API cap (30 per rolling 24 h per Page).
async fn enforce_fb_reels_limit(
    db: &PgPool,
    plan: &DailyPlan,
    new_times: &[DateTime<Utc>],
    videos: &[PlanVideo],
) -> AppResult<()> {
    let Some(min) = new_times.iter().min() else {
        return Ok(());
    };
    let Some(max) = new_times.iter().max() else {
        return Ok(());
    };
    let existing: Vec<DateTime<Utc>> = sqlx::query_scalar(
        "select coalesce(published_at, scheduled_for) from posts \
          where project_id = $1 and platform = 'facebook' \
            and status in ('scheduled','pending','container_created','publishing','published') \
            and plan_video_id <> all($2) \
            and coalesce(published_at, scheduled_for) between $3 and $4",
    )
    .bind(plan.project_id)
    .bind(videos.iter().map(|v| v.id).collect::<Vec<_>>())
    .bind(*min - Duration::hours(24))
    .bind(*max + Duration::hours(24))
    .fetch_all(db)
    .await?;
    if let Some(t) = fb_limit_violation(&existing, new_times) {
        return Err(AppError::Conflict(format!(
            "Facebook allows {FB_REELS_PER_24H} API-published Reels per 24 hours; scheduling at {t} would exceed it."
        )));
    }
    Ok(())
}

/// First proposed time at which the rolling-24h count would exceed the limit.
pub fn fb_limit_violation(
    existing: &[DateTime<Utc>],
    new_times: &[DateTime<Utc>],
) -> Option<DateTime<Utc>> {
    let mut all: Vec<DateTime<Utc>> = existing.to_vec();
    all.extend_from_slice(new_times);
    all.sort();
    for t in new_times {
        let n = all
            .iter()
            .filter(|x| **x <= *t && **x > *t - Duration::hours(24))
            .count() as i64;
        // A new post is also counted by every later post inside its own window.
        let n_forward = all
            .iter()
            .filter(|x| **x >= *t && **x < *t + Duration::hours(24))
            .count() as i64;
        if n > FB_REELS_PER_24H || n_forward > FB_REELS_PER_24H {
            return Some(*t);
        }
    }
    None
}

// ── Audiences (unchanged semantics) ──────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
pub struct PutAudiencesRequest {
    pub audiences: Vec<AudienceInput>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AudienceInput {
    pub name: String,
    pub description: String,
}

#[utoipa::path(
    get,
    path = "/projects/{pid}/audiences",
    params(("pid" = Uuid, Path, description = "Project id")),
    responses((status = 200, description = "Active audiences", body = [ProjectAudience]))
)]
pub async fn list_audiences(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
) -> AppResult<Json<Vec<ProjectAudience>>> {
    access::ensure_project_ready(&s.db, &user.id, pid).await?;
    Ok(Json(
        list_active_audiences(&s.db, pid)
            .await
            .map_err(AppError::Other)?,
    ))
}

#[utoipa::path(
    put,
    path = "/projects/{pid}/audiences",
    params(("pid" = Uuid, Path, description = "Project id")),
    request_body = PutAudiencesRequest,
    responses((status = 200, description = "Updated audiences", body = [ProjectAudience]))
)]
pub async fn put_audiences(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
    Json(body): Json<PutAudiencesRequest>,
) -> AppResult<Json<Vec<ProjectAudience>>> {
    if body.audiences.len() != 5 {
        return Err(AppError::Invalid("exactly 5 audiences required".into()));
    }
    access::ensure_project_ready(&s.db, &user.id, pid).await?;

    let pairs: Vec<(String, String)> = body
        .audiences
        .iter()
        .map(|a| (a.name.trim().to_string(), a.description.trim().to_string()))
        .collect();
    for (name, _) in &pairs {
        if name.is_empty() {
            return Err(AppError::Invalid("each audience needs a name".into()));
        }
    }

    Ok(Json(
        replace_audiences(&s.db, pid, &pairs)
            .await
            .map_err(AppError::Other)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(position: i32, track: &str, daypart: Option<&str>) -> PlanVideo {
        PlanVideo {
            id: Uuid::from_u128(position as u128 + if track == "trend" { 100 } else { 0 }),
            plan_id: Uuid::nil(),
            anchor: format!("video-{position}"),
            title: String::new(),
            track: track.into(),
            audience_id: None,
            trend: None,
            caption: String::new(),
            daypart: daypart.map(str::to_owned),
            position,
            scheduled_for: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn auto_dayparts_follow_track_order() {
        let vs: Vec<PlanVideo> = (0..5).map(|i| video(i, "audience", None)).collect();
        let items = schedulable_items(&vs);
        let dp: Vec<Daypart> = items.iter().map(|i| i.daypart).collect();
        assert_eq!(
            dp,
            vec![
                Daypart::Prime,
                Daypart::Morning,
                Daypart::Afternoon,
                Daypart::Evening,
                Daypart::Night
            ]
        );
        assert_eq!(items[0].rank, 1);
        assert!(items[1..].iter().all(|i| i.rank > 1));
    }

    #[test]
    fn explicit_non_prime_never_rank_one() {
        let vs = vec![
            video(0, "audience", Some("evening")),
            video(1, "audience", None),
        ];
        let items = schedulable_items(&vs);
        assert_eq!(items[0].daypart, Daypart::Evening);
        assert!(items[0].rank > 1);
        assert_eq!(items[1].daypart, Daypart::Prime);
        assert_eq!(items[1].rank, 1);
    }

    #[test]
    fn many_videos_wrap_dayparts() {
        let vs: Vec<PlanVideo> = (0..7).map(|i| video(i, "trend", None)).collect();
        let items = schedulable_items(&vs);
        assert_eq!(items[5].daypart, Daypart::Morning);
        assert_eq!(items[6].daypart, Daypart::Afternoon);
    }

    #[test]
    fn fb_limit_counts_rolling_window() {
        let base = Utc::now();
        let existing: Vec<DateTime<Utc>> =
            (0..29).map(|i| base + Duration::minutes(i * 10)).collect();
        assert!(fb_limit_violation(&existing, &[base + Duration::hours(6)]).is_none());
        assert!(
            fb_limit_violation(
                &existing,
                &[base + Duration::hours(6), base + Duration::hours(7)]
            )
            .is_some()
        );
        // Outside the 24h window the limit does not apply.
        assert!(fb_limit_violation(&existing, &[base + Duration::hours(30)]).is_none());
    }
}
