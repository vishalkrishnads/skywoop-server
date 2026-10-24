//! Per-project plan budgets (video counts, daily spend cap) and the browser AI cost ledger.
//!
//! The browser editor calls generation providers (e.g. fal) directly; it reports each call here
//! so one ledger (`ai_calls`, `source = 'browser'`) covers server and browser spend.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::domain::access;
use crate::domain::new_id;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub const DEFAULT_DAILY_USD_CAP: f64 = 25.0;
pub const DEFAULT_AUDIENCE_VIDEOS: i32 = 1;
pub const DEFAULT_TREND_VIDEOS: i32 = 0;
pub const MIN_AUDIENCE_VIDEOS: i32 = 1;
pub const MAX_VIDEOS_PER_TRACK: i32 = 5;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Budget {
    pub project_id: Uuid,
    pub daily_usd_cap: f64,
    pub audience_videos_per_platform: i32,
    pub trend_videos_per_platform: i32,
}

#[derive(Debug, Clone, Copy)]
pub struct VideoCounts {
    pub audience: i32,
    pub trend: i32,
}

/// Budget for a project, falling back to the defaults when none is stored.
pub async fn load_budget(db: &PgPool, project_id: Uuid) -> AppResult<Budget> {
    let row: Option<(f64, i32, i32)> = sqlx::query_as(
        "select daily_usd_cap::float8, audience_videos_per_platform, trend_videos_per_platform \
           from project_budgets where project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await?;
    Ok(match row {
        Some((cap, a, t)) => Budget {
            project_id,
            daily_usd_cap: cap,
            audience_videos_per_platform: a,
            trend_videos_per_platform: t,
        },
        None => Budget {
            project_id,
            daily_usd_cap: DEFAULT_DAILY_USD_CAP,
            audience_videos_per_platform: DEFAULT_AUDIENCE_VIDEOS,
            trend_videos_per_platform: DEFAULT_TREND_VIDEOS,
        },
    })
}

/// Videos per platform per day (used by PlanDay).
pub async fn video_counts(db: &PgPool, project_id: Uuid) -> AppResult<VideoCounts> {
    let b = load_budget(db, project_id).await?;
    Ok(VideoCounts {
        audience: b.audience_videos_per_platform,
        trend: b.trend_videos_per_platform,
    })
}

#[utoipa::path(
    get,
    path = "/projects/{pid}/budget",
    params(("pid" = Uuid, Path, description = "Project id")),
    responses((status = 200, description = "Budget (defaults when never set)", body = Budget))
)]
pub async fn get_budget(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
) -> AppResult<Json<Budget>> {
    access::ensure_project_ready(&s.db, &user.id, pid).await?;
    Ok(Json(load_budget(&s.db, pid).await?))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PutBudgetRequest {
    pub daily_usd_cap: f64,
    /// 1–5 audience-track videos per platform per day.
    pub audience_videos_per_platform: i32,
    /// 0–5 trend-track videos per platform per day.
    pub trend_videos_per_platform: i32,
}

#[utoipa::path(
    put,
    path = "/projects/{pid}/budget",
    params(("pid" = Uuid, Path, description = "Project id")),
    request_body = PutBudgetRequest,
    responses((status = 200, description = "Saved", body = Budget))
)]
pub async fn put_budget(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
    Json(body): Json<PutBudgetRequest>,
) -> AppResult<Json<Budget>> {
    access::ensure_project_ready(&s.db, &user.id, pid).await?;
    validate_budget(&body)?;
    sqlx::query(
        r#"
        insert into project_budgets
            (project_id, daily_usd_cap, audience_videos_per_platform, trend_videos_per_platform)
        values ($1, $2::numeric, $3, $4)
        on conflict (project_id) do update
           set daily_usd_cap = excluded.daily_usd_cap,
               audience_videos_per_platform = excluded.audience_videos_per_platform,
               trend_videos_per_platform = excluded.trend_videos_per_platform,
               updated_at = now()
        "#,
    )
    .bind(pid)
    .bind(body.daily_usd_cap)
    .bind(body.audience_videos_per_platform)
    .bind(body.trend_videos_per_platform)
    .execute(&s.db)
    .await?;
    Ok(Json(load_budget(&s.db, pid).await?))
}

fn validate_budget(b: &PutBudgetRequest) -> AppResult<()> {
    if !b.daily_usd_cap.is_finite() || !(0.0..=10_000.0).contains(&b.daily_usd_cap) {
        return Err(AppError::Invalid(
            "daily_usd_cap must be between 0 and 10000".into(),
        ));
    }
    for (name, min, n) in [
        (
            "audience_videos_per_platform",
            MIN_AUDIENCE_VIDEOS,
            b.audience_videos_per_platform,
        ),
        ("trend_videos_per_platform", 0, b.trend_videos_per_platform),
    ] {
        if !(min..=MAX_VIDEOS_PER_TRACK).contains(&n) {
            return Err(AppError::Invalid(format!(
                "{name} must be between {min} and {MAX_VIDEOS_PER_TRACK}"
            )));
        }
    }
    Ok(())
}

// ── Usage ledger ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
pub struct UsageReport {
    /// What the call was for, e.g. `video_generate`, `image_generate`, `voiceover`.
    pub purpose: String,
    /// azure | fal | other (default fal)
    pub provider: Option<String>,
    /// Model / endpoint id, e.g. `fal-ai/veo3`.
    #[serde(default)]
    pub model: String,
    pub cost_usd: f64,
    pub input_tokens: Option<i32>,
    pub output_tokens: Option<i32>,
    /// Number of images/videos generated.
    pub images: Option<i32>,
    pub latency_ms: Option<i32>,
    /// ok (default) | error
    pub status: Option<String>,
    pub error: Option<String>,
    /// Free-form JSON (request id, prompt hash, plan_video_id…).
    #[schema(value_type = Option<Object>)]
    pub meta: Option<Value>,
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct UsageRow {
    pub id: Uuid,
    pub purpose: String,
    pub deployment: String,
    pub source: String,
    pub provider: String,
    pub cost_usd: Option<f64>,
    pub status: String,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct UsageSummary {
    pub daily_usd_cap: f64,
    /// Spend since local midnight (project timezone), all sources.
    pub spent_today_usd: f64,
    pub remaining_usd: f64,
    pub over_budget: bool,
    pub recent: Vec<UsageRow>,
}

#[derive(Debug, Deserialize)]
pub struct UsageQuery {
    pub limit: Option<i64>,
}

async fn usage_summary(db: &PgPool, pid: Uuid, limit: i64) -> AppResult<UsageSummary> {
    let budget = load_budget(db, pid).await?;
    let spent: f64 = sqlx::query_scalar(
        r#"
        select coalesce(sum(c.cost_usd), 0)::float8
          from ai_calls c
          join projects p on p.id = c.project_id
         where c.project_id = $1
           and c.created_at >= (date_trunc('day', now() at time zone p.timezone) at time zone p.timezone)
        "#,
    )
    .bind(pid)
    .fetch_one(db)
    .await?;
    let recent = sqlx::query_as::<_, UsageRow>(
        "select id, purpose, deployment, source, provider, cost_usd::float8 as cost_usd, status, \
                error, created_at \
           from ai_calls where project_id = $1 order by created_at desc limit $2",
    )
    .bind(pid)
    .bind(limit.clamp(1, 200))
    .fetch_all(db)
    .await?;
    Ok(UsageSummary {
        daily_usd_cap: budget.daily_usd_cap,
        spent_today_usd: spent,
        remaining_usd: (budget.daily_usd_cap - spent).max(0.0),
        over_budget: spent >= budget.daily_usd_cap,
        recent,
    })
}

#[utoipa::path(
    get,
    path = "/projects/{pid}/usage",
    params(
        ("pid" = Uuid, Path, description = "Project id"),
        ("limit" = Option<i64>, Query, description = "Recent rows (default 50)")
    ),
    responses((status = 200, description = "Today's spend vs. cap and recent calls", body = UsageSummary))
)]
pub async fn get_usage(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
    Query(q): Query<UsageQuery>,
) -> AppResult<Json<UsageSummary>> {
    access::ensure_project_ready(&s.db, &user.id, pid).await?;
    Ok(Json(
        usage_summary(&s.db, pid, q.limit.unwrap_or(50)).await?,
    ))
}

#[utoipa::path(
    post,
    path = "/projects/{pid}/usage",
    params(("pid" = Uuid, Path, description = "Project id")),
    request_body = UsageReport,
    responses((status = 201, description = "Recorded with source=browser; returns updated summary", body = UsageSummary))
)]
pub async fn post_usage(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
    Json(body): Json<UsageReport>,
) -> AppResult<(StatusCode, Json<UsageSummary>)> {
    access::ensure_project_ready(&s.db, &user.id, pid).await?;
    let purpose = body.purpose.trim();
    if purpose.is_empty() || purpose.len() > 80 {
        return Err(AppError::Invalid("purpose must be 1–80 characters".into()));
    }
    if !body.cost_usd.is_finite() || !(0.0..=1000.0).contains(&body.cost_usd) {
        return Err(AppError::Invalid(
            "cost_usd must be between 0 and 1000".into(),
        ));
    }
    let provider = match body.provider.as_deref().unwrap_or("fal") {
        p @ ("azure" | "fal" | "other") => p,
        other => {
            return Err(AppError::Invalid(format!(
                "provider must be azure, fal or other, got {other}"
            )));
        }
    };
    let status = match body.status.as_deref().unwrap_or("ok") {
        st @ ("ok" | "error") => st,
        other => {
            return Err(AppError::Invalid(format!(
                "status must be ok or error, got {other}"
            )));
        }
    };
    sqlx::query(
        r#"
        insert into ai_calls (
          id, project_id, purpose, deployment, source, provider,
          input_tokens, output_tokens, images, cost_usd, latency_ms, status, error, meta
        ) values ($1,$2,$3,$4,'browser',$5,$6,$7,$8,$9::float8::numeric,$10,$11,$12,$13)
        "#,
    )
    .bind(new_id())
    .bind(pid)
    .bind(purpose)
    .bind(body.model.trim())
    .bind(provider)
    .bind(body.input_tokens)
    .bind(body.output_tokens)
    .bind(body.images)
    .bind(body.cost_usd)
    .bind(body.latency_ms.unwrap_or(0))
    .bind(status)
    .bind(body.error.as_deref())
    .bind(body.meta.unwrap_or_else(|| serde_json::json!({})))
    .execute(&s.db)
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(usage_summary(&s.db, pid, 50).await?),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(cap: f64, a: i32, t: i32) -> PutBudgetRequest {
        PutBudgetRequest {
            daily_usd_cap: cap,
            audience_videos_per_platform: a,
            trend_videos_per_platform: t,
        }
    }

    #[test]
    fn budget_validation() {
        assert!(validate_budget(&req(25.0, 5, 5)).is_ok());
        assert!(validate_budget(&req(25.0, 1, 0)).is_ok());
        assert!(validate_budget(&req(25.0, 0, 0)).is_err());
        assert!(validate_budget(&req(25.0, 0, 3)).is_err());
        assert!(validate_budget(&req(-1.0, 1, 0)).is_err());
        assert!(validate_budget(&req(25.0, 6, 0)).is_err());
        assert!(validate_budget(&req(25.0, 1, 6)).is_err());
        assert!(validate_budget(&req(f64::NAN, 1, 1)).is_err());
    }
}
