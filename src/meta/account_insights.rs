//! Daily account-level Meta insights (IG online_followers, Page metrics).

use chrono::NaiveDate;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::jobs::JobError;
use crate::meta::error::{MetaError, MetaErrorKind};
use crate::meta::metrics::{
    self, FB_ACCOUNT_DAY_METRICS, IG_ACCOUNT_DAY_METRICS, IG_ONLINE_FOLLOWERS,
};
use crate::meta::publish::mark_connection_needs_reauth;
use crate::state::AppState;

/// Fetch and store account-level insights for one project/platform/local day.
pub async fn fetch_account_insights(
    state: &AppState,
    project_id: Uuid,
    platform: &str,
    local_date: NaiveDate,
) -> Result<Value, JobError> {
    let conn = load_connection(state, project_id, platform).await?;
    let token = state
        .secrets
        .get(&conn.token_secret_name)
        .await
        .map_err(JobError::Retry)?;

    let (flat, raw) = match platform {
        "instagram" => match fetch_ig_account(state, &conn.external_account_id, &token).await {
            Ok(v) => v,
            Err(e) => return Err(handle_meta(state, project_id, platform, e).await),
        },
        "facebook" => {
            match fetch_fb_page(state, &conn.external_account_id, &token, local_date).await {
                Ok(v) => v,
                Err(e) => return Err(handle_meta(state, project_id, platform, e).await),
            }
        }
        other => {
            return Err(JobError::permanent(anyhow::anyhow!(
                "unknown platform: {other}"
            )));
        }
    };

    // Replace that day's rows for this project/platform.
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| JobError::Retry(e.into()))?;
    sqlx::query(
        r#"
        delete from account_metric_daily
         where project_id = $1 and platform = $2 and local_date = $3
        "#,
    )
    .bind(project_id)
    .bind(platform)
    .bind(local_date)
    .execute(&mut *tx)
    .await
    .map_err(|e| JobError::Retry(e.into()))?;

    if flat.is_empty() {
        // Store a sentinel so we know we fetched and got empty (e.g. <100 followers).
        sqlx::query(
            r#"
            insert into account_metric_daily (
                project_id, platform, local_date, metric, dim_key, dim_value, value, raw
            ) values ($1,$2,$3,'__empty__','','',null,$4)
            "#,
        )
        .bind(project_id)
        .bind(platform)
        .bind(local_date)
        .bind(&raw)
        .execute(&mut *tx)
        .await
        .map_err(|e| JobError::Retry(e.into()))?;
    } else {
        for row in &flat {
            sqlx::query(
                r#"
                insert into account_metric_daily (
                    project_id, platform, local_date, metric, dim_key, dim_value, value, raw
                ) values ($1,$2,$3,$4,$5,$6,$7,$8)
                on conflict (project_id, platform, local_date, metric, dim_key, dim_value)
                do update set value = excluded.value, raw = excluded.raw, fetched_at = now()
                "#,
            )
            .bind(project_id)
            .bind(platform)
            .bind(local_date)
            .bind(&row.metric)
            .bind(&row.dim_key)
            .bind(&row.dim_value)
            .bind(row.value)
            .bind(&raw)
            .execute(&mut *tx)
            .await
            .map_err(|e| JobError::Retry(e.into()))?;
        }
    }

    tx.commit().await.map_err(|e| JobError::Retry(e.into()))?;

    // First time we see online_followers data → kick a schedule review.
    if platform == "instagram" && flat.iter().any(|r| r.metric == "online_followers") {
        let prior: bool = sqlx::query_scalar(
            r#"
            select exists(
                select 1 from account_metric_daily
                 where project_id = $1 and platform = 'instagram'
                   and metric = 'online_followers'
                   and local_date < $2
            )
            "#,
        )
        .bind(project_id)
        .bind(local_date)
        .fetch_one(&state.db)
        .await
        .unwrap_or(true);
        if !prior {
            let queue = crate::jobs::DataJobQueue::new(state.db.clone());
            let mut tx = state
                .db
                .begin()
                .await
                .map_err(|e| JobError::Retry(e.into()))?;
            let _ = queue
                .enqueue(
                    &mut tx,
                    &crate::jobs::DataJob::ReviewPostingSchedule {
                        project_id,
                        platform: crate::domain::Platform::Instagram,
                    },
                    Some(format!("sched_review_first_online:{project_id}")),
                    chrono::Utc::now(),
                )
                .await;
            let _ = tx.commit().await;
        }
    }

    Ok(json!({
        "project_id": project_id,
        "platform": platform,
        "local_date": local_date,
        "flat_count": flat.len(),
        "empty": flat.is_empty(),
    }))
}

async fn fetch_ig_account(
    state: &AppState,
    ig_user_id: &str,
    token: &str,
) -> Result<(Vec<metrics::FlatMetric>, Value), MetaError> {
    let mut parts = Vec::new();
    let mut errors = Vec::new();

    // Day metrics with metric_type=total_value.
    let day_csv = IG_ACCOUNT_DAY_METRICS.join(",");
    match state
        .meta
        .get(
            &format!("{ig_user_id}/insights"),
            &[
                ("metric", &day_csv),
                ("period", "day"),
                ("metric_type", "total_value"),
            ],
            token,
        )
        .await
    {
        Ok(v) => parts.push(v),
        Err(e) if is_soft_fail(&e) => {
            // Retry one-by-one.
            for name in IG_ACCOUNT_DAY_METRICS {
                match state
                    .meta
                    .get(
                        &format!("{ig_user_id}/insights"),
                        &[
                            ("metric", *name),
                            ("period", "day"),
                            ("metric_type", "total_value"),
                        ],
                        token,
                    )
                    .await
                {
                    Ok(v) => parts.push(v),
                    Err(e2) if is_soft_fail(&e2) => {
                        errors.push(json!({"metric": name, "error": e2.message, "code": e2.code}));
                    }
                    Err(e2) => return Err(e2),
                }
            }
        }
        Err(e) => return Err(e),
    }

    // online_followers (lifetime, last 30 days). Empty when <100 followers.
    let online = match state
        .meta
        .get(
            &format!("{ig_user_id}/insights"),
            &[("metric", IG_ONLINE_FOLLOWERS), ("period", "lifetime")],
            token,
        )
        .await
    {
        Ok(v) => {
            parts.push(v.clone());
            Some(v)
        }
        Err(e) if is_soft_fail(&e) => {
            errors.push(json!({
                "metric": IG_ONLINE_FOLLOWERS,
                "error": e.message,
                "code": e.code,
            }));
            None
        }
        Err(e) => return Err(e),
    };

    let mut flat = Vec::new();
    for p in &parts {
        flat.extend(metrics::flatten_insights(p));
    }

    let raw = json!({
        "parts": parts,
        "online_followers": online,
        "errors": errors,
        // Meta reports online_followers hour keys in Pacific time (America/Los_Angeles).
        // Consumers convert them to the project timezone (see scheduling::online_followers_by_hour).
        "note": "online_followers hour keys are Pacific time (America/Los_Angeles); convert to the project timezone before use",
    });

    Ok((flat, raw))
}

async fn fetch_fb_page(
    state: &AppState,
    page_id: &str,
    token: &str,
    local_date: NaiveDate,
) -> Result<(Vec<metrics::FlatMetric>, Value), MetaError> {
    let mut parts = Vec::new();
    let mut errors = Vec::new();
    let since = local_date
        .and_hms_opt(0, 0, 0)
        .map(|dt| dt.and_utc().timestamp())
        .unwrap_or(0);
    let until = (local_date + chrono::Duration::days(1))
        .and_hms_opt(0, 0, 0)
        .map(|dt| dt.and_utc().timestamp())
        .unwrap_or(0);
    let since_s = since.to_string();
    let until_s = until.to_string();

    let day_csv = FB_ACCOUNT_DAY_METRICS.join(",");
    match state
        .meta
        .get(
            &format!("{page_id}/insights"),
            &[
                ("metric", &day_csv),
                ("period", "day"),
                ("since", &since_s),
                ("until", &until_s),
            ],
            token,
        )
        .await
    {
        Ok(v) => parts.push(v),
        Err(e) if is_soft_fail(&e) => {
            for name in FB_ACCOUNT_DAY_METRICS {
                match state
                    .meta
                    .get(
                        &format!("{page_id}/insights"),
                        &[
                            ("metric", *name),
                            ("period", "day"),
                            ("since", &since_s),
                            ("until", &until_s),
                        ],
                        token,
                    )
                    .await
                {
                    Ok(v) => parts.push(v),
                    Err(e2) if is_soft_fail(&e2) => {
                        errors.push(json!({"metric": name, "error": e2.message, "code": e2.code}));
                    }
                    Err(e2) => return Err(e2),
                }
            }
        }
        Err(e) => return Err(e),
    }

    let mut flat = Vec::new();
    for p in &parts {
        flat.extend(metrics::flatten_insights(p));
    }

    let raw = json!({ "parts": parts, "errors": errors, "since": since, "until": until });
    Ok((flat, raw))
}

fn is_soft_fail(e: &MetaError) -> bool {
    e.code == Some(100)
        || e.message.to_ascii_lowercase().contains("invalid metric")
        || e.message.to_ascii_lowercase().contains("not available")
        || e.message.to_ascii_lowercase().contains("(#10)")
        || e.kind == MetaErrorKind::Retryable
            && (e.message.contains("100 likes") || e.message.contains("100 followers"))
}

async fn handle_meta(state: &AppState, project_id: Uuid, platform: &str, e: MetaError) -> JobError {
    if e.kind == MetaErrorKind::NeedsReauth {
        let _ = mark_connection_needs_reauth(&state.db, project_id, platform).await;
        return JobError::permanent(anyhow::anyhow!(e));
    }
    match e.kind {
        MetaErrorKind::RateLimited => JobError::retry_after(e.retry_after_mins.max(1), e),
        MetaErrorKind::IgPublishingLimit | MetaErrorKind::NeedsReauth => {
            JobError::permanent(anyhow::anyhow!(e))
        }
        MetaErrorKind::Retryable => JobError::Retry(e.into()),
    }
}

struct ConnRow {
    external_account_id: String,
    token_secret_name: String,
}

async fn load_connection(
    state: &AppState,
    project_id: Uuid,
    platform: &str,
) -> Result<ConnRow, JobError> {
    let row: Option<(String, String)> = sqlx::query_as(
        r#"
        select external_account_id, token_secret_name
          from platform_connections
         where project_id = $1 and platform = $2 and status = 'connected'
        "#,
    )
    .bind(project_id)
    .bind(platform)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| JobError::Retry(e.into()))?;

    let Some((external_account_id, token_secret_name)) = row else {
        return Err(JobError::permanent(anyhow::anyhow!(
            "no connected {platform} account for project {project_id}"
        )));
    };
    Ok(ConnRow {
        external_account_id,
        token_secret_name,
    })
}
