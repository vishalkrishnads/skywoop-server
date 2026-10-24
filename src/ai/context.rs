use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::analysis::rollups::ig_bio_campaign_token;
use crate::domain::{BrandProfile, Platform};

#[derive(Debug, Clone, Serialize)]
pub struct AudienceBrief {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub position: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectBrief {
    pub name: String,
    pub description: String,
    pub website: Option<String>,
    pub industry: Option<String>,
    pub brand: BrandProfile,
    pub audiences: Vec<AudienceBrief>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Medians {
    pub views: Option<f64>,
    pub cta_rate: Option<f64>,
    pub installs: Option<f64>,
    pub avg_watch_time_ms: Option<f64>,
    pub skip_rate: Option<f64>,
}

/// One planned video and, when published, how it did.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct VideoBrief {
    pub for_date: NaiveDate,
    pub title: String,
    pub track: String,
    pub audience: Option<String>,
    pub daypart: Option<String>,
    pub post_status: Option<String>,
    pub views: Option<i64>,
    pub cta_rate: Option<f64>,
    pub installs: Option<i64>,
    pub avg_watch_time_ms: Option<i64>,
    pub skip_rate: Option<f64>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct DiaryBrief {
    pub entry_date: NaiveDate,
    pub kind: String,
    pub video_title: Option<String>,
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
}

/// Compact live ledger of every experiment on the platform.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct PostLedgerRow {
    pub publish_local_date: NaiveDate,
    pub published_at: DateTime<Utc>,
    pub title: String,
    pub track: String,
    pub views: Option<i64>,
    pub reach: Option<i64>,
    pub profile_visits: Option<i64>,
    pub bio_link_taps: Option<i64>,
    pub follows: Option<i64>,
    pub saves: Option<i64>,
    pub shares: Option<i64>,
    pub avg_watch_time_ms: Option<i64>,
    pub skip_rate: Option<f64>,
    pub cta_rate: Option<f64>,
    pub cta_source: Option<String>,
    /// Actual ASC installs (FB) or estimated bio installs (IG).
    pub installs: Option<i64>,
    pub maturity: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstallHistoryDay {
    pub date: NaiveDate,
    pub total_installs: i64,
    pub bio_campaign_installs: i64,
    pub instagram_referrer_installs: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct KnowledgePack {
    pub project: ProjectBrief,
    pub platform: Platform,
    pub platform_medians: Medians,
    pub recent_videos: Vec<VideoBrief>,
    pub recent_diary: Vec<DiaryBrief>,
    pub focus_diary: Vec<DiaryBrief>,
    pub post_ledger: Vec<PostLedgerRow>,
    pub install_history: Vec<InstallHistoryDay>,
    pub funnel_notes: String,
}

/// Build the shared agent memory pack for planner calls.
pub async fn build(
    db: &PgPool,
    project_id: Uuid,
    platform: Platform,
    focus_plan_video_id: Option<Uuid>,
) -> anyhow::Result<KnowledgePack> {
    let project = load_project(db, project_id).await?;
    let platform_medians = load_medians(db, project_id, platform).await?;
    let recent_videos = load_recent_videos(db, project_id, platform).await?;
    let recent_diary = load_diary(db, project_id, platform, None, None).await?;
    let focus_diary = if let Some(vid) = focus_plan_video_id {
        load_diary(db, project_id, platform, Some(vid), Some(40)).await?
    } else {
        vec![]
    };
    let post_ledger = load_post_ledger(db, project_id, platform).await?;
    let install_history = load_install_history(db, project_id).await?;

    let funnel_notes = match platform {
        Platform::Instagram => "Instagram organic Reels cannot carry per-post links. \
Funnel entry = profile_visits (and bio_link_taps when available). \
cta_rate = profile_visits / views. \
Installs on the shared bio App Store campaign are estimated (est_installs) by apportioning \
daily ASC bio-campaign downloads across posts by that day's bio-link-tap (else profile-visit) share. \
Treat est_installs as estimates, not exact attribution. \
avg_watch_time_ms and skip_rate describe hook retention."
            .into(),
        Platform::Facebook => "Facebook organic Reels carry a tracked /r/{token} link. \
cta_rate = unique tracked clicks / views. \
Installs come from the per-post App Store Connect campaign token. \
avg_watch_time_ms describes hook retention; skip_rate is not available on Facebook."
            .into(),
    };

    Ok(KnowledgePack {
        project,
        platform,
        platform_medians,
        recent_videos,
        recent_diary,
        focus_diary,
        post_ledger,
        install_history,
        funnel_notes,
    })
}

async fn load_project(db: &PgPool, project_id: Uuid) -> anyhow::Result<ProjectBrief> {
    let row: (String, String, Option<String>, Option<String>, Value) = sqlx::query_as(
        r#"
        select name, description, website, industry, brand
        from projects
        where id = $1
        "#,
    )
    .bind(project_id)
    .fetch_one(db)
    .await?;

    let brand: BrandProfile = serde_json::from_value(row.4).unwrap_or_default();
    let audiences = load_audiences(db, project_id).await?;
    Ok(ProjectBrief {
        name: row.0,
        description: row.1,
        website: row.2,
        industry: row.3,
        brand,
        audiences,
    })
}

async fn load_audiences(db: &PgPool, project_id: Uuid) -> anyhow::Result<Vec<AudienceBrief>> {
    let rows: Vec<(Uuid, String, String, i32)> = sqlx::query_as(
        r#"
        select id, name, description, position
          from project_audiences
         where project_id = $1 and archived_at is null
         order by position
        "#,
    )
    .bind(project_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, name, description, position)| AudienceBrief {
            id,
            name,
            description,
            position,
        })
        .collect())
}

async fn load_medians(
    db: &PgPool,
    project_id: Uuid,
    platform: Platform,
) -> anyhow::Result<Medians> {
    let row: (
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
    ) = sqlx::query_as(
        r#"
        select avg(views::float8), avg(cta_rate),
               avg(coalesce(installs, est_installs)::float8),
               avg(avg_watch_time_ms::float8), avg(skip_rate)
        from experiments
        where project_id = $1 and platform = $2 and maturity = 'mature'
        "#,
    )
    .bind(project_id)
    .bind(platform.as_str())
    .fetch_one(db)
    .await?;

    Ok(Medians {
        views: row.0,
        cta_rate: row.1,
        installs: row.2,
        avg_watch_time_ms: row.3,
        skip_rate: row.4,
    })
}

async fn load_recent_videos(
    db: &PgPool,
    project_id: Uuid,
    platform: Platform,
) -> anyhow::Result<Vec<VideoBrief>> {
    Ok(sqlx::query_as::<_, VideoBrief>(
        r#"
        select dp.for_date, pv.title, pv.track, pa.name as audience, pv.daypart,
               po.status as post_status,
               e.views, e.cta_rate,
               coalesce(e.installs, e.est_installs) as installs,
               e.avg_watch_time_ms, e.skip_rate
          from plan_videos pv
          join daily_plans dp on dp.id = pv.plan_id
          left join project_audiences pa on pa.id = pv.audience_id
          left join posts po on po.plan_video_id = pv.id
          left join experiments e on e.plan_video_id = pv.id
         where dp.project_id = $1 and dp.platform = $2
           and dp.status not in ('waiting_for_data', 'drafting', 'failed')
         order by dp.for_date desc, pv.position
         limit 60
        "#,
    )
    .bind(project_id)
    .bind(platform.as_str())
    .fetch_all(db)
    .await?)
}

async fn load_diary(
    db: &PgPool,
    project_id: Uuid,
    platform: Platform,
    plan_video_id: Option<Uuid>,
    limit: Option<i64>,
) -> anyhow::Result<Vec<DiaryBrief>> {
    Ok(sqlx::query_as::<_, DiaryBrief>(
        r#"
        select d.entry_date, d.kind, pv.title as video_title,
               d.views, d.cta_clicks, d.profile_visits, d.bio_link_taps,
               d.installs, d.est_installs, d.growth,
               d.avg_watch_time_ms, d.skip_rate, d.observation
          from diary_entries d
          left join plan_videos pv on pv.id = d.plan_video_id
         where d.project_id = $1 and d.platform = $2
           and ($3::uuid is null or d.plan_video_id = $3)
         order by d.entry_date desc, d.created_at desc
         limit $4
        "#,
    )
    .bind(project_id)
    .bind(platform.as_str())
    .bind(plan_video_id)
    .bind(limit.unwrap_or(30))
    .fetch_all(db)
    .await?)
}

async fn load_post_ledger(
    db: &PgPool,
    project_id: Uuid,
    platform: Platform,
) -> anyhow::Result<Vec<PostLedgerRow>> {
    Ok(sqlx::query_as::<_, PostLedgerRow>(
        r#"
        select e.publish_local_date, e.published_at, pv.title, e.track,
               e.views, e.reach, e.profile_visits, e.bio_link_taps,
               e.follows, e.saves, e.shares, e.avg_watch_time_ms, e.skip_rate,
               e.cta_rate, e.cta_source,
               coalesce(e.installs, e.est_installs) as installs,
               e.maturity
          from experiments e
          join plan_videos pv on pv.id = e.plan_video_id
         where e.project_id = $1 and e.platform = $2
         order by e.published_at desc
        "#,
    )
    .bind(project_id)
    .bind(platform.as_str())
    .fetch_all(db)
    .await?)
}

async fn load_install_history(
    db: &PgPool,
    project_id: Uuid,
) -> anyhow::Result<Vec<InstallHistoryDay>> {
    let row: Option<(Option<String>, String)> =
        sqlx::query_as("select app_store_app_id, slug from projects where id = $1")
            .bind(project_id)
            .fetch_optional(db)
            .await?;

    let Some((Some(app_id), slug)) = row else {
        return Ok(vec![]);
    };
    let bio = ig_bio_campaign_token(&slug);
    let since = Utc::now().date_naive() - chrono::Duration::days(90);

    // Total installs per day across all campaigns.
    let totals: Vec<(NaiveDate, i64)> = sqlx::query_as(
        r#"
        select date, coalesce(sum(value), 0)::bigint
          from asc_campaign_daily
         where app_id = $1
           and date >= $2
           and metric in ('first_time_downloads', 'redownloads')
         group by date
         order by date
        "#,
    )
    .bind(&app_id)
    .bind(since)
    .fetch_all(db)
    .await?;

    let bio_days: Vec<(NaiveDate, i64)> = sqlx::query_as(
        r#"
        select date, coalesce(sum(value), 0)::bigint
          from asc_campaign_daily
         where app_id = $1
           and campaign = $2
           and date >= $3
           and metric in ('first_time_downloads', 'redownloads')
         group by date
        "#,
    )
    .bind(&app_id)
    .bind(&bio)
    .bind(since)
    .fetch_all(db)
    .await?;

    // Instagram app referrer from source_info (string match, case-insensitive).
    let ig_ref: Vec<(NaiveDate, i64)> = sqlx::query_as(
        r#"
        select date, coalesce(sum(value), 0)::bigint
          from asc_source_daily
         where app_id = $1
           and date >= $2
           and metric in ('first_time_downloads', 'redownloads')
           and lower(source_info) like '%instagram%'
         group by date
        "#,
    )
    .bind(&app_id)
    .bind(since)
    .fetch_all(db)
    .await?;

    use std::collections::BTreeMap;
    let mut map: BTreeMap<NaiveDate, InstallHistoryDay> = BTreeMap::new();
    for (d, v) in totals {
        map.entry(d)
            .or_insert(InstallHistoryDay {
                date: d,
                total_installs: 0,
                bio_campaign_installs: 0,
                instagram_referrer_installs: 0,
            })
            .total_installs = v;
    }
    for (d, v) in bio_days {
        map.entry(d)
            .or_insert(InstallHistoryDay {
                date: d,
                total_installs: 0,
                bio_campaign_installs: 0,
                instagram_referrer_installs: 0,
            })
            .bio_campaign_installs = v;
    }
    for (d, v) in ig_ref {
        map.entry(d)
            .or_insert(InstallHistoryDay {
                date: d,
                total_installs: 0,
                bio_campaign_installs: 0,
                instagram_referrer_installs: 0,
            })
            .instagram_referrer_installs = v;
    }
    Ok(map.into_values().collect())
}
