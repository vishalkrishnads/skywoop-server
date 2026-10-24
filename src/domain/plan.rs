//! Daily plan domain: markdown plans, status machine, videos, comments, chat, helpers.

use chrono::{DateTime, NaiveDate, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Transaction};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::domain::Platform;
use crate::domain::new_id;
use crate::plan_format::ParsedPlan;

/// Plan status machine (matches the `daily_plans.status` check constraint).
///
/// ```text
/// waiting_for_data → drafting → review ⇄ generating_assets ⇄ building ⇄ built → scheduled → posted
///                         ↘ failed ↗ (failed → review/drafting on retry)
/// any state before scheduled → no_op → review (resume, videos exist) | waiting_for_data (redraft)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    WaitingForData,
    Drafting,
    Review,
    GeneratingAssets,
    Building,
    Built,
    Scheduled,
    Posted,
    Failed,
    /// Nothing is posted today (recent posts are still gaining views, or the user skipped).
    NoOp,
}

impl PlanStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WaitingForData => "waiting_for_data",
            Self::Drafting => "drafting",
            Self::Review => "review",
            Self::GeneratingAssets => "generating_assets",
            Self::Building => "building",
            Self::Built => "built",
            Self::Scheduled => "scheduled",
            Self::Posted => "posted",
            Self::Failed => "failed",
            Self::NoOp => "no_op",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "waiting_for_data" => Some(Self::WaitingForData),
            "drafting" => Some(Self::Drafting),
            "review" => Some(Self::Review),
            "generating_assets" => Some(Self::GeneratingAssets),
            "building" => Some(Self::Building),
            "built" => Some(Self::Built),
            "scheduled" => Some(Self::Scheduled),
            "posted" => Some(Self::Posted),
            "failed" => Some(Self::Failed),
            "no_op" => Some(Self::NoOp),
            _ => None,
        }
    }

    /// The user may mark the day as a no-op from any state before it is scheduled.
    pub fn can_mark_no_op(self) -> bool {
        !matches!(self, Self::Scheduled | Self::Posted | Self::NoOp)
    }

    /// Markdown / comments / videos may still be edited by the user.
    pub fn is_editable(self) -> bool {
        matches!(
            self,
            Self::Review | Self::GeneratingAssets | Self::Building | Self::Built
        )
    }

    /// States a client may move between with `POST /plans/{id}/status`.
    pub fn is_client_status(self) -> bool {
        matches!(
            self,
            Self::Review | Self::GeneratingAssets | Self::Building | Self::Built
        )
    }

    /// Whether `self → to` is a legal client-driven transition.
    pub fn can_transition_to(self, to: PlanStatus) -> bool {
        if self == to {
            return true;
        }
        if to == Self::NoOp {
            return self.can_mark_no_op();
        }
        if self == Self::NoOp {
            // Resuming: back to review (when videos exist) or a fresh draft.
            return to == Self::Review;
        }
        if !to.is_client_status() {
            return false;
        }
        match self {
            // failed plans can be recovered back into review once they have markdown.
            Self::Failed => to == Self::Review,
            s if s.is_client_status() => true,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanTrack {
    Audience,
    Trend,
}

impl PlanTrack {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Audience => "audience",
            Self::Trend => "trend",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "audience" => Some(Self::Audience),
            "trend" => Some(Self::Trend),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Daypart {
    Prime,
    Morning,
    Afternoon,
    Evening,
    Night,
}

impl Daypart {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prime => "prime",
            Self::Morning => "morning",
            Self::Afternoon => "afternoon",
            Self::Evening => "evening",
            Self::Night => "night",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "prime" => Some(Self::Prime),
            "morning" => Some(Self::Morning),
            "afternoon" => Some(Self::Afternoon),
            "evening" => Some(Self::Evening),
            "night" => Some(Self::Night),
            _ => None,
        }
    }

    pub fn non_prime() -> [Daypart; 4] {
        [
            Daypart::Morning,
            Daypart::Afternoon,
            Daypart::Evening,
            Daypart::Night,
        ]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProjectAudience {
    pub id: Uuid,
    pub project_id: Uuid,
    pub position: i32,
    pub name: String,
    pub description: String,
    pub archived_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct DailyPlan {
    pub id: Uuid,
    pub project_id: Uuid,
    pub platform: String,
    pub for_date: NaiveDate,
    pub status: String,
    pub version: i32,
    pub markdown: String,
    pub greeting: String,
    pub findings: Value,
    pub prime_window_override: Option<Value>,
    pub job_id: Option<Uuid>,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Day title (the markdown `#` heading).
    pub title: String,
    /// Paragraph under the title (max 280 characters).
    pub summary: String,
    /// Why the day is a no-op, e.g. `recent_posts_growing` or `user`.
    pub no_op_reason: Option<String>,
    pub no_op_explanation: Option<String>,
}

pub const PLAN_COLUMNS: &str = "id, project_id, platform, for_date, status, version, markdown, \
     greeting, findings, prime_window_override, job_id, scheduled_at, created_at, updated_at, \
     title, summary, no_op_reason, no_op_explanation";

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct PlanVideo {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub anchor: String,
    pub title: String,
    pub track: String,
    pub audience_id: Option<Uuid>,
    pub trend: Option<Value>,
    pub caption: String,
    pub daypart: Option<String>,
    pub position: i32,
    pub scheduled_for: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

pub const VIDEO_COLUMNS: &str = "id, plan_id, anchor, title, track, audience_id, trend, caption, \
     daypart, position, scheduled_for, created_at";

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct PlanComment {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub anchor: String,
    pub body: String,
    pub author: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct PlanMessage {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub role: String,
    /// UI message parts (AI SDK style), stored verbatim.
    pub parts: Value,
    pub created_at: DateTime<Utc>,
}

// ── Agent output schema ──────────────────────────────────────────────

/// PlanDay structured output: a greeting plus the full plan markdown.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlanDayOut {
    /// 1–3 friendly sentences addressed to the operator (what changed, what to look at first).
    pub greeting: String,
    /// Full plan markdown following the plan format.
    pub markdown: String,
    /// Set only when today should post nothing because recent posts are still gaining views;
    /// otherwise null.
    #[serde(default)]
    pub no_op: Option<NoOpOut>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NoOpOut {
    /// Short snake_case reason, e.g. `recent_posts_growing`.
    pub reason: String,
    /// 1–3 sentences for the operator citing the numbers behind the decision.
    pub explanation: String,
}

/// Reason stored when the user marks a day as no-op.
pub const NO_OP_REASON_USER: &str = "user";

/// Signals behind a no-op day.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct NoOpMetrics {
    /// Views gained across this platform's posts in the last 48 hours.
    pub views_last_48h: i64,
    /// Installs per view across experiments published in the last 7 days.
    pub install_rate: Option<f64>,
    /// Posts published on this platform so far.
    pub prior_posts: i64,
}

impl NoOpMetrics {
    /// PlanDay may propose a no-op only when earlier posts exist and are still gaining views.
    pub fn allows_no_op(&self) -> bool {
        self.prior_posts > 0 && self.views_last_48h > 0
    }
}

pub async fn no_op_metrics(
    pool: &PgPool,
    project_id: Uuid,
    platform: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<NoOpMetrics> {
    let (views_last_48h, install_rate, prior_posts): (i64, Option<f64>, i64) = sqlx::query_as(
        r#"
        with snaps as (
            select s.experiment_id, s.views, s.fetched_at
              from post_metric_snapshots s
              join experiments e on e.id = s.experiment_id
             where e.project_id = $1 and e.platform = $2
        ),
        latest as (
            select distinct on (experiment_id) experiment_id, views
              from snaps order by experiment_id, fetched_at desc
        ),
        before as (
            select distinct on (experiment_id) experiment_id, views
              from snaps where fetched_at <= $3 - interval '48 hours'
             order by experiment_id, fetched_at desc
        )
        select
            (select coalesce(sum(greatest(coalesce(l.views, 0) - coalesce(b.views, 0), 0)), 0)::int8
               from latest l left join before b using (experiment_id)),
            (select sum(coalesce(e.installs, e.est_installs))::float8 / nullif(sum(e.views), 0)::float8
               from experiments e
              where e.project_id = $1 and e.platform = $2
                and e.published_at >= $3 - interval '7 days'
                and coalesce(e.installs, e.est_installs) is not null),
            (select count(*) from experiments e where e.project_id = $1 and e.platform = $2)
        "#,
    )
    .bind(project_id)
    .bind(platform)
    .bind(now)
    .fetch_one(pool)
    .await?;
    Ok(NoOpMetrics {
        views_last_48h,
        install_rate,
        prior_posts,
    })
}

/// `daily_plans.title` / `summary` values for a parsed plan.
pub fn title_and_summary(parsed: &ParsedPlan) -> (String, String) {
    (
        parsed.title.clone().unwrap_or_default(),
        parsed
            .summary
            .chars()
            .take(crate::plan_format::MAX_SUMMARY_CHARS)
            .collect(),
    )
}

// ── Audience / trend resolution ──────────────────────────────────────

/// Resolve an `audience:` value (uuid, 1-based position, or name) to an audience id.
pub fn resolve_audience(audiences: &[ProjectAudience], raw: &str) -> Option<Uuid> {
    let raw = raw.trim().trim_matches(['"', '`']);
    if let Ok(id) = Uuid::parse_str(raw) {
        return audiences.iter().find(|a| a.id == id).map(|a| a.id);
    }
    let first = raw.split_whitespace().next().unwrap_or("");
    let numeric = first.trim_matches(['#', '.', ':']);
    if let Ok(pos) = numeric.parse::<i32>()
        && let Some(a) = audiences.iter().find(|a| a.position == pos)
    {
        return Some(a.id);
    }
    let lower = raw.to_lowercase();
    audiences
        .iter()
        .find(|a| a.name.to_lowercase() == lower)
        .or_else(|| {
            audiences
                .iter()
                .find(|a| !lower.is_empty() && lower.contains(&a.name.to_lowercase()))
        })
        .map(|a| a.id)
}

/// Resolve a `trend:` value against a trend snapshot (`id` or `title`).
/// Falls back to a stub so hand-edited plans do not fail on a stale trend reference.
pub fn resolve_trend(items: &Value, raw: &str) -> Value {
    let raw = raw.trim().trim_matches(['"', '`']);
    if let Some(arr) = items.as_array() {
        if let Some(found) = arr
            .iter()
            .find(|t| t.get("id").and_then(Value::as_str) == Some(raw))
        {
            return found.clone();
        }
        if let Some(found) = arr.iter().find(|t| {
            t.get("title")
                .and_then(Value::as_str)
                .is_some_and(|title| title.eq_ignore_ascii_case(raw))
        }) {
            return found.clone();
        }
    }
    json!({
        "id": raw,
        "title": raw,
        "summary": "",
        "source_urls": [],
        "source": "plan",
    })
}

// ── DB helpers ───────────────────────────────────────────────────────

pub async fn list_active_audiences(
    pool: &PgPool,
    project_id: Uuid,
) -> anyhow::Result<Vec<ProjectAudience>> {
    let rows: Vec<(
        Uuid,
        Uuid,
        i32,
        String,
        String,
        Option<DateTime<Utc>>,
        DateTime<Utc>,
    )> = sqlx::query_as(
        r#"
            select id, project_id, position, name, description, archived_at, created_at
              from project_audiences
             where project_id = $1 and archived_at is null
             order by position
            "#,
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(id, project_id, position, name, description, archived_at, created_at)| {
                ProjectAudience {
                    id,
                    project_id,
                    position,
                    name,
                    description,
                    archived_at,
                    created_at,
                }
            },
        )
        .collect())
}

pub async fn replace_audiences(
    pool: &PgPool,
    project_id: Uuid,
    audiences: &[(String, String)],
) -> anyhow::Result<Vec<ProjectAudience>> {
    if audiences.len() != 5 {
        anyhow::bail!("exactly 5 audiences required, got {}", audiences.len());
    }
    let mut tx = pool.begin().await?;
    sqlx::query(
        r#"
        update project_audiences set archived_at = now()
         where project_id = $1 and archived_at is null
        "#,
    )
    .bind(project_id)
    .execute(&mut *tx)
    .await?;

    let mut out = Vec::new();
    for (i, (name, desc)) in audiences.iter().enumerate() {
        let id = new_id();
        let position = (i as i32) + 1;
        let row: (
            Uuid,
            Uuid,
            i32,
            String,
            String,
            Option<DateTime<Utc>>,
            DateTime<Utc>,
        ) = sqlx::query_as(
            r#"
                insert into project_audiences (id, project_id, position, name, description)
                values ($1,$2,$3,$4,$5)
                returning id, project_id, position, name, description, archived_at, created_at
                "#,
        )
        .bind(id)
        .bind(project_id)
        .bind(position)
        .bind(name)
        .bind(desc)
        .fetch_one(&mut *tx)
        .await?;
        out.push(ProjectAudience {
            id: row.0,
            project_id: row.1,
            position: row.2,
            name: row.3,
            description: row.4,
            archived_at: row.5,
            created_at: row.6,
        });
    }
    tx.commit().await?;
    Ok(out)
}

pub async fn get_plan(pool: &PgPool, plan_id: Uuid) -> anyhow::Result<Option<DailyPlan>> {
    Ok(sqlx::query_as::<_, DailyPlan>(sqlx::AssertSqlSafe(format!(
        "select {PLAN_COLUMNS} from daily_plans where id = $1"
    )))
    .bind(plan_id)
    .fetch_optional(pool)
    .await?)
}

pub async fn get_plan_for_date(
    pool: &PgPool,
    project_id: Uuid,
    platform: Platform,
    for_date: NaiveDate,
) -> anyhow::Result<Option<DailyPlan>> {
    Ok(sqlx::query_as::<_, DailyPlan>(sqlx::AssertSqlSafe(format!(
        "select {PLAN_COLUMNS} from daily_plans \
         where project_id = $1 and platform = $2 and for_date = $3"
    )))
    .bind(project_id)
    .bind(platform.as_str())
    .bind(for_date)
    .fetch_optional(pool)
    .await?)
}

pub async fn latest_plan(
    pool: &PgPool,
    project_id: Uuid,
    platform: Platform,
) -> anyhow::Result<Option<DailyPlan>> {
    Ok(sqlx::query_as::<_, DailyPlan>(sqlx::AssertSqlSafe(format!(
        "select {PLAN_COLUMNS} from daily_plans \
         where project_id = $1 and platform = $2 order by for_date desc limit 1"
    )))
    .bind(project_id)
    .bind(platform.as_str())
    .fetch_optional(pool)
    .await?)
}

pub async fn list_videos(pool: &PgPool, plan_id: Uuid) -> anyhow::Result<Vec<PlanVideo>> {
    Ok(sqlx::query_as::<_, PlanVideo>(sqlx::AssertSqlSafe(format!(
        "select {VIDEO_COLUMNS} from plan_videos where plan_id = $1 order by position"
    )))
    .bind(plan_id)
    .fetch_all(pool)
    .await?)
}

pub async fn list_comments(pool: &PgPool, plan_id: Uuid) -> anyhow::Result<Vec<PlanComment>> {
    Ok(sqlx::query_as::<_, PlanComment>(
        "select id, plan_id, anchor, body, author, created_at from plan_comments \
         where plan_id = $1 order by created_at",
    )
    .bind(plan_id)
    .fetch_all(pool)
    .await?)
}

/// Get or create the plan row for a day. Newly created rows start in `initial`.
pub async fn ensure_plan(
    pool: &PgPool,
    project_id: Uuid,
    platform: Platform,
    for_date: NaiveDate,
    initial: PlanStatus,
) -> anyhow::Result<DailyPlan> {
    sqlx::query(
        "insert into daily_plans (id, project_id, platform, for_date, status) \
         values ($1,$2,$3,$4,$5) on conflict (project_id, platform, for_date) do nothing",
    )
    .bind(new_id())
    .bind(project_id)
    .bind(platform.as_str())
    .bind(for_date)
    .bind(initial.as_str())
    .execute(pool)
    .await?;
    get_plan_for_date(pool, project_id, platform, for_date)
        .await?
        .ok_or_else(|| anyhow::anyhow!("plan row vanished"))
}

pub async fn set_plan_status(
    pool: &PgPool,
    plan_id: Uuid,
    status: PlanStatus,
) -> anyhow::Result<()> {
    sqlx::query("update daily_plans set status = $2, updated_at = now() where id = $1")
        .bind(plan_id)
        .bind(status.as_str())
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn insert_plan_version(
    tx: &mut Transaction<'_, Postgres>,
    plan_id: Uuid,
    version: i32,
    markdown: &str,
    greeting: &str,
    reason: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "insert into plan_versions (plan_id, version, markdown, greeting, reason) \
         values ($1,$2,$3,$4,$5) \
         on conflict (plan_id, version) do update set \
           markdown = excluded.markdown, greeting = excluded.greeting, reason = excluded.reason",
    )
    .bind(plan_id)
    .bind(version)
    .bind(markdown)
    .bind(greeting)
    .bind(reason)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// How [`sync_videos`] resolves references to audiences and trends.
pub struct SyncCtx<'a> {
    pub audiences: &'a [ProjectAudience],
    pub trends: &'a Value,
}

/// Blob paths of renders that belonged to videos removed by a sync (caller deletes best-effort).
#[derive(Debug, Default)]
pub struct SyncResult {
    pub orphaned_blobs: Vec<String>,
    /// References that could not be resolved (reported as warnings).
    pub warnings: Vec<String>,
}

/// Upsert `plan_videos` from a parsed plan; creates `video_projects` lazily elsewhere.
/// Videos missing from the markdown are deleted.
pub async fn sync_videos(
    tx: &mut Transaction<'_, Postgres>,
    plan_id: Uuid,
    parsed: &ParsedPlan,
    ctx: &SyncCtx<'_>,
) -> anyhow::Result<SyncResult> {
    let mut res = SyncResult::default();

    // Free the (plan_id, position) unique index so positions can be shuffled.
    sqlx::query("update plan_videos set position = -position - 1 where plan_id = $1")
        .bind(plan_id)
        .execute(&mut **tx)
        .await?;

    let anchors: Vec<String> = parsed.videos.iter().map(|v| v.anchor.clone()).collect();

    let orphan: Vec<(Option<String>,)> = sqlx::query_as(
        "select vp.render_blob_path from plan_videos pv \
           join video_projects vp on vp.plan_video_id = pv.id \
          where pv.plan_id = $1 and pv.anchor <> all($2)",
    )
    .bind(plan_id)
    .bind(&anchors)
    .fetch_all(&mut **tx)
    .await?;
    res.orphaned_blobs = orphan.into_iter().filter_map(|(p,)| p).collect();

    sqlx::query("delete from plan_videos where plan_id = $1 and anchor <> all($2)")
        .bind(plan_id)
        .bind(&anchors)
        .execute(&mut **tx)
        .await?;

    for v in &parsed.videos {
        let track = v.track.unwrap_or(PlanTrack::Audience);
        let audience_id = match (&track, &v.audience) {
            (PlanTrack::Audience, Some(raw)) => {
                let id = resolve_audience(ctx.audiences, raw);
                if id.is_none() {
                    res.warnings
                        .push(format!("{}: unknown audience \"{raw}\"", v.anchor));
                }
                id
            }
            _ => None,
        };
        let trend = match (&track, &v.trend) {
            (PlanTrack::Trend, Some(raw)) => Some(resolve_trend(ctx.trends, raw)),
            _ => None,
        };
        sqlx::query(
            r#"
            insert into plan_videos
                (id, plan_id, anchor, title, track, audience_id, trend, caption, daypart, position)
            values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
            on conflict (plan_id, anchor) do update set
                title = excluded.title,
                track = excluded.track,
                audience_id = excluded.audience_id,
                trend = excluded.trend,
                caption = excluded.caption,
                daypart = excluded.daypart,
                position = excluded.position
            "#,
        )
        .bind(new_id())
        .bind(plan_id)
        .bind(&v.anchor)
        .bind(&v.title)
        .bind(track.as_str())
        .bind(audience_id)
        .bind(&trend)
        .bind(&v.caption)
        .bind(v.daypart.map(Daypart::as_str))
        .bind(v.position)
        .execute(&mut **tx)
        .await?;
    }
    Ok(res)
}

/// Current trend items for a plan (empty array when no snapshot exists).
pub async fn trend_items_for(
    pool: &PgPool,
    project_id: Uuid,
    platform: &str,
    for_date: NaiveDate,
) -> anyhow::Result<Value> {
    let items: Option<Value> = sqlx::query_scalar(
        "select items from trend_snapshots \
          where project_id = $1 and platform = $2 and local_date = $3",
    )
    .bind(project_id)
    .bind(platform)
    .bind(for_date)
    .fetch_optional(pool)
    .await?;
    Ok(items.unwrap_or_else(|| json!([])))
}

/// Recent video titles (last `days`) to avoid repeating concepts.
pub async fn recent_video_titles(
    pool: &PgPool,
    project_id: Uuid,
    platform: &str,
    since: NaiveDate,
) -> anyhow::Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "select pv.title from plan_videos pv join daily_plans dp on dp.id = pv.plan_id \
          where dp.project_id = $1 and dp.platform = $2 and dp.for_date >= $3 \
          order by dp.for_date desc, pv.position",
    )
    .bind(project_id)
    .bind(platform)
    .bind(since)
    .fetch_all(pool)
    .await?)
}

/// Flip a scheduled plan to `posted` (or `failed`) once all its posts are terminal.
pub async fn maybe_finish_plan(pool: &PgPool, plan_id: Uuid) -> anyhow::Result<()> {
    let row: (i64, i64, i64) = sqlx::query_as(
        "select count(*) filter (where p.status not in ('published','failed')), \
                count(*) filter (where p.status = 'published'), \
                count(*) \
           from posts p join plan_videos pv on pv.id = p.plan_video_id \
          where pv.plan_id = $1",
    )
    .bind(plan_id)
    .fetch_one(pool)
    .await?;
    let (open, published, total) = row;
    if total == 0 || open > 0 {
        return Ok(());
    }
    let status = if published > 0 {
        PlanStatus::Posted
    } else {
        PlanStatus::Failed
    };
    sqlx::query(
        "update daily_plans set status = $2, updated_at = now() \
          where id = $1 and status = 'scheduled'",
    )
    .bind(plan_id)
    .bind(status.as_str())
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aud(pos: i32, name: &str) -> ProjectAudience {
        ProjectAudience {
            id: new_id(),
            project_id: new_id(),
            position: pos,
            name: name.into(),
            description: String::new(),
            archived_at: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn resolves_audience_by_position_name_and_id() {
        let a = vec![aud(1, "Students"), aud(2, "New parents")];
        assert_eq!(resolve_audience(&a, "2"), Some(a[1].id));
        assert_eq!(resolve_audience(&a, "2 (New parents)"), Some(a[1].id));
        assert_eq!(resolve_audience(&a, "students"), Some(a[0].id));
        assert_eq!(resolve_audience(&a, &a[0].id.to_string()), Some(a[0].id));
        assert_eq!(resolve_audience(&a, "9"), None);
    }

    #[test]
    fn status_transitions() {
        use PlanStatus::*;
        assert!(Review.can_transition_to(GeneratingAssets));
        assert!(Built.can_transition_to(Review));
        assert!(!Scheduled.can_transition_to(Review));
        assert!(!Review.can_transition_to(Scheduled));
        assert!(Failed.can_transition_to(Review));
        assert!(!Failed.can_transition_to(Built));
        assert!(!Drafting.can_transition_to(Review));
    }

    #[test]
    fn no_op_transitions() {
        use PlanStatus::*;
        for s in [
            WaitingForData,
            Drafting,
            Review,
            GeneratingAssets,
            Building,
            Built,
            Failed,
        ] {
            assert!(s.can_transition_to(NoOp), "{s:?}");
        }
        assert!(!Scheduled.can_transition_to(NoOp));
        assert!(!Posted.can_transition_to(NoOp));
        assert!(NoOp.can_transition_to(Review));
        assert!(!NoOp.can_transition_to(Built));
        assert!(!NoOp.is_editable());
        assert_eq!(PlanStatus::parse("no_op"), Some(NoOp));
        assert_eq!(NoOp.as_str(), "no_op");
    }

    #[test]
    fn no_op_needs_prior_growing_posts() {
        let m = |views_last_48h, prior_posts| NoOpMetrics {
            views_last_48h,
            install_rate: None,
            prior_posts,
        };
        assert!(m(120, 3).allows_no_op());
        assert!(!m(0, 3).allows_no_op());
        assert!(!m(120, 0).allows_no_op());
    }

    #[test]
    fn title_and_summary_come_from_markdown() {
        let p = crate::plan_format::parse("# Day title\nShort summary.\n");
        assert_eq!(
            title_and_summary(&p),
            ("Day title".to_owned(), "Short summary.".to_owned())
        );
    }

    #[test]
    fn trend_resolution_falls_back() {
        let items = json!([{"id":"a","title":"Foo","summary":"s","source_urls":[],"source":"web"}]);
        assert_eq!(resolve_trend(&items, "a")["title"], "Foo");
        assert_eq!(resolve_trend(&items, "foo")["id"], "a");
        assert_eq!(resolve_trend(&items, "zzz")["source"], "plan");
    }
}
