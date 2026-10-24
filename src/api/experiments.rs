//! Experiment list/detail/refresh APIs (B16 / M7).

use axum::Json;
use axum::extract::{Path, Query, State};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::analysis::rollups;
use crate::auth::AuthUser;
use crate::domain::Platform;
use crate::domain::access;
use crate::error::{AppError, AppResult};
use crate::jobs::{DataJob, DataJobQueue};
use crate::state::AppState;

#[derive(Debug, Deserialize, IntoParams)]
pub struct ListQuery {
    pub maturity: Option<String>,
}

#[derive(Debug, Serialize, FromRow, ToSchema)]
pub struct ExperimentSummary {
    pub id: Uuid,
    pub plan_video_id: Uuid,
    pub post_id: Uuid,
    pub project_id: Uuid,
    pub platform: String,
    pub track: String,
    pub audience_id: Option<Uuid>,
    pub published_at: DateTime<Utc>,
    pub publish_local_date: NaiveDate,
    pub hypothesis: String,
    pub maturity: String,
    pub meta_complete: bool,
    pub asc_complete: bool,
    pub asc_no_data: bool,
    pub matured_at: Option<DateTime<Utc>>,
    pub views: Option<i64>,
    pub reach: Option<i64>,
    pub interactions: Option<i64>,
    pub platform_link_clicks: Option<i64>,
    pub cta_clicks: Option<i64>,
    pub cta_clicks_unique: Option<i64>,
    pub asc_impressions: Option<i64>,
    pub asc_page_views: Option<i64>,
    pub installs: Option<i64>,
    pub profile_visits: Option<i64>,
    pub bio_link_taps: Option<i64>,
    pub avg_watch_time_ms: Option<i64>,
    pub total_watch_time_ms: Option<i64>,
    pub skip_rate: Option<f64>,
    pub reels_play_count: Option<i64>,
    pub est_installs: Option<i64>,
    pub cta_rate: Option<f64>,
    pub install_rate: Option<f64>,
    pub growth: Option<i64>,
    pub growth_baseline: Option<String>,
    pub campaign_token: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, FromRow, ToSchema)]
pub struct SnapshotView {
    pub id: Uuid,
    pub snapshot_no: i32,
    pub fetched_at: DateTime<Utc>,
    pub views: Option<i64>,
    pub reach: Option<i64>,
    pub interactions: Option<i64>,
    pub platform_link_clicks: Option<i64>,
}

#[derive(Debug, Serialize, FromRow, ToSchema)]
pub struct AscCampaignRow {
    pub date: NaiveDate,
    pub campaign: String,
    pub metric: String,
    pub value: i64,
    pub processing_date: NaiveDate,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ExperimentDetail {
    #[serde(flatten)]
    pub experiment: ExperimentSummary,
    pub snapshots: Vec<SnapshotView>,
    pub asc_campaign_rows: Vec<AscCampaignRow>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RefreshResponse {
    pub job_id: Option<Uuid>,
    pub snapshot_no: i32,
}

#[utoipa::path(
    get,
    path = "/projects/{pid}/platforms/{p}/experiments",
    params(
        ("pid" = Uuid, Path, description = "Project id"),
        ("p" = String, Path, description = "Platform"),
        ListQuery
    ),
    responses((status = 200, description = "Experiments", body = [ExperimentSummary]))
)]
pub async fn list(
    State(s): State<AppState>,
    user: AuthUser,
    Path((pid, p)): Path<(Uuid, String)>,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Vec<ExperimentSummary>>> {
    access::ensure_project_ready(&s.db, &user.id, pid).await?;
    let platform =
        Platform::parse(&p).ok_or_else(|| AppError::Invalid(format!("unknown platform: {p}")))?;

    let rows = if let Some(ref m) = q.maturity {
        if !matches!(m.as_str(), "collecting" | "mature" | "inconclusive") {
            return Err(AppError::Invalid(format!("unknown maturity: {m}")));
        }
        sqlx::query_as::<_, ExperimentSummary>(
            r#"
            select e.id, e.plan_video_id, e.post_id, e.project_id, e.platform, e.track, e.audience_id,
                   e.published_at, e.publish_local_date, e.hypothesis, e.maturity,
                   e.meta_complete, e.asc_complete, e.asc_no_data, e.matured_at,
                   e.views, e.reach, e.interactions, e.platform_link_clicks,
                   e.cta_clicks, e.cta_clicks_unique, e.asc_impressions, e.asc_page_views,
                   e.installs, e.cta_rate, e.install_rate, e.growth, e.growth_baseline,
                   e.profile_visits, e.bio_link_taps, e.avg_watch_time_ms, e.total_watch_time_ms,
                   e.skip_rate, e.reels_play_count, e.est_installs,
                   tl.token as campaign_token, e.updated_at
              from experiments e
              left join tracked_links tl on tl.plan_video_id = e.plan_video_id
             where e.project_id = $1 and e.platform = $2 and e.maturity = $3
             order by e.published_at desc
            "#,
        )
        .bind(pid)
        .bind(platform.as_str())
        .bind(m)
        .fetch_all(&s.db)
        .await?
    } else {
        sqlx::query_as::<_, ExperimentSummary>(
            r#"
            select e.id, e.plan_video_id, e.post_id, e.project_id, e.platform, e.track, e.audience_id,
                   e.published_at, e.publish_local_date, e.hypothesis, e.maturity,
                   e.meta_complete, e.asc_complete, e.asc_no_data, e.matured_at,
                   e.views, e.reach, e.interactions, e.platform_link_clicks,
                   e.cta_clicks, e.cta_clicks_unique, e.asc_impressions, e.asc_page_views,
                   e.installs, e.cta_rate, e.install_rate, e.growth, e.growth_baseline,
                   e.profile_visits, e.bio_link_taps, e.avg_watch_time_ms, e.total_watch_time_ms,
                   e.skip_rate, e.reels_play_count, e.est_installs,
                   tl.token as campaign_token, e.updated_at
              from experiments e
              left join tracked_links tl on tl.plan_video_id = e.plan_video_id
             where e.project_id = $1 and e.platform = $2
             order by e.published_at desc
            "#,
        )
        .bind(pid)
        .bind(platform.as_str())
        .fetch_all(&s.db)
        .await?
    };

    Ok(Json(rows))
}

#[utoipa::path(
    get,
    path = "/experiments/{id}",
    params(("id" = Uuid, Path, description = "Experiment id")),
    responses((status = 200, description = "Experiment detail", body = ExperimentDetail))
)]
pub async fn get(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<ExperimentDetail>> {
    access::ensure_experiment_owner(&s.db, &user.id, id).await?;
    let experiment = sqlx::query_as::<_, ExperimentSummary>(
        r#"
        select e.id, e.plan_video_id, e.post_id, e.project_id, e.platform, e.track, e.audience_id,
               e.published_at, e.publish_local_date, e.hypothesis, e.maturity,
               e.meta_complete, e.asc_complete, e.asc_no_data, e.matured_at,
               e.views, e.reach, e.interactions, e.platform_link_clicks,
               e.cta_clicks, e.cta_clicks_unique, e.asc_impressions, e.asc_page_views,
               e.installs, e.cta_rate, e.install_rate, e.growth, e.growth_baseline,
               e.profile_visits, e.bio_link_taps, e.avg_watch_time_ms, e.total_watch_time_ms,
                   e.skip_rate, e.reels_play_count, e.est_installs,
                   tl.token as campaign_token, e.updated_at
          from experiments e
          left join tracked_links tl on tl.plan_video_id = e.plan_video_id
         where e.id = $1
        "#,
    )
    .bind(id)
    .fetch_one(&s.db)
    .await?;

    let snapshots = sqlx::query_as::<_, SnapshotView>(
        r#"
        select id, snapshot_no, fetched_at, views, reach, interactions, platform_link_clicks
          from post_metric_snapshots
         where experiment_id = $1
         order by snapshot_no
        "#,
    )
    .bind(id)
    .fetch_all(&s.db)
    .await?;

    let asc_campaign_rows = if let Some(ref token) = experiment.campaign_token {
        sqlx::query_as::<_, AscCampaignRow>(
            r#"
            select date, campaign, metric, value, processing_date
              from asc_campaign_daily
             where campaign = $1
             order by date, metric
            "#,
        )
        .bind(token)
        .fetch_all(&s.db)
        .await?
    } else {
        Vec::new()
    };

    Ok(Json(ExperimentDetail {
        experiment,
        snapshots,
        asc_campaign_rows,
    }))
}

#[utoipa::path(
    post,
    path = "/experiments/{id}/refresh",
    params(("id" = Uuid, Path, description = "Experiment id")),
    responses((status = 200, description = "Insights refresh enqueued", body = RefreshResponse))
)]
pub async fn refresh(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<RefreshResponse>> {
    access::ensure_experiment_owner(&s.db, &user.id, id).await?;

    let next_no: i32 = sqlx::query_scalar(
        r#"
        select coalesce(max(snapshot_no), -1) + 1
          from post_metric_snapshots
         where experiment_id = $1
        "#,
    )
    .bind(id)
    .fetch_one(&s.db)
    .await?;

    let data_jobs = DataJobQueue::new(s.db.clone());
    let now = s.clock.now();
    let mut tx = s.db.begin().await?;
    let job_id = data_jobs
        .enqueue(
            &mut tx,
            &DataJob::FetchPostInsights {
                experiment_id: id,
                snapshot_no: next_no,
            },
            Some(format!("insights:{id}:{next_no}")),
            now,
        )
        .await?;
    tx.commit().await?;

    // Also refresh rollups from existing data immediately.
    let _ = rollups::recompute(&s.db, id).await;

    Ok(Json(RefreshResponse {
        job_id,
        snapshot_no: next_no,
    }))
}

// --- App Store status / report-request (project-scoped) ---

#[derive(Debug, Serialize, ToSchema)]
pub struct AppStoreStatus {
    pub app_store_app_id: Option<String>,
    pub request_id: Option<String>,
    pub last_seen_ok_at: Option<DateTime<Utc>>,
    pub admin_role_needed: bool,
    pub message: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ReportRequestResponse {
    pub request_id: String,
}

#[utoipa::path(
    get,
    path = "/projects/{pid}/app-store/status",
    params(("pid" = Uuid, Path, description = "Project id")),
    responses((status = 200, description = "App Store Connect status", body = AppStoreStatus))
)]
pub async fn app_store_status(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
) -> AppResult<Json<AppStoreStatus>> {
    access::ensure_project_ready(&s.db, &user.id, pid).await?;
    let app_id: Option<String> =
        sqlx::query_scalar("select app_store_app_id from projects where id = $1")
            .bind(pid)
            .fetch_optional(&s.db)
            .await?
            .flatten();

    let Some(ref app_store_app_id) = app_id else {
        return Ok(Json(AppStoreStatus {
            app_store_app_id: None,
            request_id: None,
            last_seen_ok_at: None,
            admin_role_needed: false,
            message: Some("App Store app id not configured".into()),
        }));
    };

    let row: Option<(String, Option<DateTime<Utc>>)> = sqlx::query_as(
        "select request_id, last_seen_ok_at from asc_report_requests where app_id = $1",
    )
    .bind(app_store_app_id)
    .fetch_optional(&s.db)
    .await?;

    let (request_id, last_seen_ok_at, admin_role_needed, message) = match row {
        Some((rid, ok)) if rid == crate::appstore::ADMIN_NEEDED_SENTINEL => (
            None,
            ok,
            true,
            Some("App Store key needs Admin role to start reporting".into()),
        ),
        Some((rid, ok)) => (Some(rid), ok, false, None),
        None => (
            None,
            None,
            false,
            Some("No analytics report request yet".into()),
        ),
    };

    Ok(Json(AppStoreStatus {
        app_store_app_id: Some(app_store_app_id.clone()),
        request_id,
        last_seen_ok_at,
        admin_role_needed,
        message,
    }))
}

#[utoipa::path(
    post,
    path = "/projects/{pid}/app-store/report-request",
    params(("pid" = Uuid, Path, description = "Project id")),
    responses((status = 200, description = "Report request ensured", body = ReportRequestResponse))
)]
pub async fn app_store_report_request(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
) -> AppResult<Json<ReportRequestResponse>> {
    access::ensure_project_ready(&s.db, &user.id, pid).await?;
    match crate::appstore::ingest::ensure_report_request_for_project(&s, pid).await {
        Ok(request_id) => Ok(Json(ReportRequestResponse { request_id })),
        Err(e) if e.is_permanent() => Err(AppError::Upstream(e.to_string())),
        Err(e) => Err(AppError::Upstream(e.to_string())),
    }
}
