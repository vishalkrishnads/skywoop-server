use std::time::Duration;

use chrono::{DateTime, Datelike, Timelike, Utc, Weekday};
use sqlx::PgPool;
use uuid::Uuid;

use super::worker::trunc_hour;
use super::{DataJob, DataJobQueue, UserJob, UserJobQueue};
use crate::clock::Clock;
use crate::domain::Platform;

pub async fn run_scheduler(
    pool: PgPool,
    data_jobs: DataJobQueue,
    user_jobs: UserJobQueue,
    clock: std::sync::Arc<dyn Clock>,
) {
    loop {
        if let Err(e) = tick(&pool, &data_jobs, &user_jobs, clock.now()).await {
            tracing::warn!(error = ?e, "scheduler tick failed");
        }
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

async fn tick(
    pool: &PgPool,
    data_jobs: &DataJobQueue,
    user_jobs: &UserJobQueue,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    let hour = trunc_hour(now);
    data_jobs
        .enqueue(
            &mut tx,
            &DataJob::EvaluateMaturity { hour },
            Some(format!("maturity:{}", now.format("%Y%m%d%H"))),
            now,
        )
        .await?;

    let apps: Vec<(String,)> = sqlx::query_as(
        "select distinct app_store_app_id from projects where app_store_app_id is not null",
    )
    .fetch_all(&mut *tx)
    .await?;
    if now.hour() >= 6 {
        for (app_id,) in apps {
            data_jobs
                .enqueue(
                    &mut tx,
                    &DataJob::AscDailyIngest {
                        app_id: app_id.clone(),
                        date: now.date_naive(),
                    },
                    Some(format!("asc:{app_id}:{}", now.date_naive())),
                    now,
                )
                .await?;
        }
    }

    tx.commit().await?;

    // Prefetch daily trends once each local morning so New ideas are warm.
    let mut tx = pool.begin().await?;
    for (project_id, platform, local_date, country) in
        workspaces_past_local_hour_with_country(pool, now, 6).await?
    {
        data_jobs
            .enqueue(
                &mut tx,
                &DataJob::RefreshTrends {
                    project_id,
                    platform,
                    local_date,
                    country: country.clone(),
                },
                Some(format!("trends:{project_id}:{platform}:{local_date}")),
                now,
            )
            .await?;
    }
    tx.commit().await?;

    // Daily account insights after local 07:00.
    let mut tx = pool.begin().await?;
    for (project_id, platform, local_date, _) in
        workspaces_past_local_hour_with_country(pool, now, 7).await?
    {
        // Only if connected.
        let connected: bool = sqlx::query_scalar(
            r#"
            select exists(
                select 1 from platform_connections
                 where project_id = $1 and platform = $2 and status = 'connected'
            )
            "#,
        )
        .bind(project_id)
        .bind(platform.as_str())
        .fetch_one(&mut *tx)
        .await?;
        if !connected {
            continue;
        }
        data_jobs
            .enqueue(
                &mut tx,
                &DataJob::FetchAccountInsights {
                    project_id,
                    platform,
                    date: local_date,
                },
                Some(format!("acct:{project_id}:{platform}:{local_date}")),
                now,
            )
            .await?;
    }
    tx.commit().await?;

    // Weekly schedule review: local Monday ≥ 05:00.
    let mut tx = pool.begin().await?;
    for (project_id, platform, local_date, _) in
        workspaces_past_local_hour_with_country(pool, now, 5).await?
    {
        let tz_name: Option<String> =
            sqlx::query_scalar("select timezone from projects where id = $1")
                .bind(project_id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some(tz_name) = tz_name else { continue };
        let Ok(tz) = tz_name.parse::<chrono_tz::Tz>() else {
            continue;
        };
        let local = now.with_timezone(&tz);
        if local.weekday() != Weekday::Mon {
            continue;
        }
        data_jobs
            .enqueue(
                &mut tx,
                &DataJob::ReviewPostingSchedule {
                    project_id,
                    platform,
                },
                Some(format!(
                    "sched_review:{project_id}:{platform}:{}",
                    local_date
                )),
                now,
            )
            .await?;
    }
    tx.commit().await?;

    // Daily cycle: after local 06:00, but only once data is ready (`cycle_ready`).
    // There is no forced deadline: the plan waits for account insights, ASC and D-1 snapshots.
    let mut tx = pool.begin().await?;
    for (project_id, platform, local_date, _) in
        workspaces_past_local_hour_with_country(pool, now, 6).await?
    {
        if !cycle_ready(pool, project_id, platform, local_date, now).await? {
            continue;
        }
        data_jobs
            .enqueue(
                &mut tx,
                &DataJob::DailyCycle {
                    project_id,
                    platform,
                    local_date,
                },
                Some(format!("cycle:{project_id}:{platform}:{local_date}")),
                now,
            )
            .await?;
    }
    tx.commit().await?;

    // The morning cycle runs once per local day. A transient model error marks
    // tomorrow's plan `failed` and nothing else picks it up. Retry those hourly
    // until the date has passed.
    retry_failed_drafts(pool, data_jobs, now).await?;

    // Post-publish media cleanup: published posts whose MP4 is still in blob storage.
    cleanup_sweep(pool, user_jobs, now).await?;
    Ok(())
}

async fn retry_failed_drafts(
    pool: &PgPool,
    data_jobs: &DataJobQueue,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let failed: Vec<(Uuid, String, chrono::NaiveDate)> = sqlx::query_as(
        r#"
        select dp.project_id, dp.platform, dp.for_date
          from daily_plans dp
          join projects p on p.id = dp.project_id
         where dp.status = 'failed'
           and dp.for_date >= (timezone(p.timezone, now()))::date
        "#,
    )
    .fetch_all(pool)
    .await?;
    if failed.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    for (project_id, platform_name, for_date) in failed {
        let Some(platform) = Platform::parse(&platform_name) else {
            continue;
        };
        let busy: bool = sqlx::query_scalar(
            r#"
            select exists(
                select 1 from jobs
                 where kind = 'draft_plan'
                   and status in ('queued', 'running')
                   and payload->>'project_id' = $1
                   and payload->>'platform' = $2
                   and payload->>'for_date' = $3
            )
            "#,
        )
        .bind(project_id.to_string())
        .bind(platform.as_str())
        .bind(for_date.to_string())
        .fetch_one(&mut *tx)
        .await?;
        if busy {
            continue;
        }
        data_jobs
            .enqueue(
                &mut tx,
                &DataJob::DraftPlan {
                    project_id,
                    platform,
                    for_date,
                    allow_no_op: true,
                },
                Some(format!(
                    "draft_plan:{project_id}:{}:{for_date}:retry:{}",
                    platform.as_str(),
                    now.format("%Y%m%d%H")
                )),
                now,
            )
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Enqueue `CleanupPostMedia` for published posts that still hold their rendered MP4.
/// The hourly dedupe suffix lets a failed cleanup be retried later.
async fn cleanup_sweep(
    pool: &PgPool,
    user_jobs: &UserJobQueue,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let stale: Vec<(Uuid,)> = sqlx::query_as(
        r#"
        select id from posts
         where status = 'published'
           and not media_deleted
           and coalesce(published_at, now()) < $1
         order by published_at
         limit 50
        "#,
    )
    .bind(now - chrono::Duration::minutes(5))
    .fetch_all(pool)
    .await?;
    if stale.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    for (post_id,) in stale {
        user_jobs
            .enqueue(
                &mut tx,
                &UserJob::CleanupPostMedia { post_id },
                Some(format!("cleanup:{post_id}:{}", now.format("%Y%m%d%H"))),
            )
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// True when account insights for D, ASC for the current UTC day, and due D-1
/// post snapshots are all finished (or ASC has no app configured).
pub async fn cycle_ready(
    pool: &PgPool,
    project_id: Uuid,
    platform: Platform,
    local_date: chrono::NaiveDate,
    now: DateTime<Utc>,
) -> anyhow::Result<bool> {
    // Bootstrap projects immediately. Account/ASC jobs can exist before Skywoop has
    // published anything, but there are no prior-post numbers worth waiting for.
    let has_prior_posts: bool = sqlx::query_scalar(
        r#"
        select exists(
            select 1 from experiments
             where project_id = $1
               and platform = $2
               and publish_local_date < $3
        )
        "#,
    )
    .bind(project_id)
    .bind(platform.as_str())
    .bind(local_date)
    .fetch_one(pool)
    .await?;
    if !has_prior_posts {
        return Ok(true);
    }

    let connected: bool = sqlx::query_scalar(
        r#"
        select exists(
            select 1 from platform_connections
             where project_id = $1 and platform = $2 and status = 'connected'
        )
        "#,
    )
    .bind(project_id)
    .bind(platform.as_str())
    .fetch_one(pool)
    .await?;

    if connected {
        let acct_done: bool = sqlx::query_scalar(
            r#"
            select exists(
                select 1 from jobs
                 where kind = 'fetch_account_insights'
                   and dedupe_key = $1
                   and status in ('succeeded', 'failed')
            )
            "#,
        )
        .bind(format!(
            "acct:{project_id}:{}:{local_date}",
            platform.as_str()
        ))
        .fetch_one(pool)
        .await?;
        if !acct_done {
            return Ok(false);
        }
    }

    let app_id: Option<String> =
        sqlx::query_scalar("select app_store_app_id from projects where id = $1")
            .bind(project_id)
            .fetch_one(pool)
            .await?;
    if let Some(app_id) = app_id {
        let utc_date = now.date_naive();
        let asc_done: bool = sqlx::query_scalar(
            r#"
            select exists(
                select 1 from jobs
                 where kind = 'asc_daily_ingest'
                   and dedupe_key = $1
                   and status in ('succeeded', 'failed')
            )
            "#,
        )
        .bind(format!("asc:{app_id}:{utc_date}"))
        .fetch_one(pool)
        .await?;
        if !asc_done {
            return Ok(false);
        }
    }

    let prev = local_date - chrono::Duration::days(1);
    let pending_insights: i64 = sqlx::query_scalar(
        r#"
        select count(*)
          from jobs j
          join experiments e on e.id = (j.payload->>'experiment_id')::uuid
         where j.kind = 'fetch_post_insights'
           and j.status in ('queued', 'running')
           and j.run_at <= $1
           and e.project_id = $2
           and e.platform = $3
           and e.publish_local_date = $4
        "#,
    )
    .bind(now)
    .bind(project_id)
    .bind(platform.as_str())
    .bind(prev)
    .fetch_one(pool)
    .await?;

    Ok(pending_insights == 0)
}

async fn workspaces_past_local_hour_with_country(
    pool: &PgPool,
    now: DateTime<Utc>,
    hour: u32,
) -> anyhow::Result<Vec<(Uuid, Platform, chrono::NaiveDate, String)>> {
    let rows: Vec<(Uuid, String, String, String)> = sqlx::query_as(
        r#"
        select pw.project_id, pw.platform, p.timezone, p.market_country
          from platform_workspaces pw
          join projects p on p.id = pw.project_id
         where p.onboarding_step = 'complete'
        "#,
    )
    .fetch_all(pool)
    .await?;

    let mut out = Vec::new();
    for (project_id, platform, tz_name, country) in rows {
        let Ok(tz) = tz_name.parse::<chrono_tz::Tz>() else {
            continue;
        };
        let local = now.with_timezone(&tz);
        if local.hour() >= hour {
            let Some(platform) = Platform::parse(&platform) else {
                continue;
            };
            out.push((project_id, platform, local.date_naive(), country));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[sqlx::test(migrations = "./migrations")]
    async fn first_plan_does_not_wait_for_numbers(pool: PgPool) {
        let project_id = Uuid::now_v7();
        let owner = format!("test-owner-{project_id}");
        sqlx::query("insert into app_users (id, email) values ($1, $2)")
            .bind(&owner)
            .bind(format!("{owner}@test.local"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "insert into projects (id, slug, name, app_store_app_id, owner_user_id) \
             values ($1, $2, $3, $4, $5)",
        )
        .bind(project_id)
        .bind(format!("bootstrap-{project_id}"))
        .bind("Bootstrap project")
        .bind("123456789")
        .bind(&owner)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "insert into platform_connections \
             (id, project_id, platform, external_account_id, display_name, token_secret_name, status) \
             values ($1, $2, 'instagram', 'account', 'Account', 'token', 'connected')",
        )
        .bind(Uuid::now_v7())
        .bind(project_id)
        .execute(&pool)
        .await
        .unwrap();

        let local_date = chrono::NaiveDate::from_ymd_opt(2026, 10, 18).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 10, 18, 12, 0, 0).unwrap();
        assert!(
            cycle_ready(&pool, project_id, Platform::Instagram, local_date, now)
                .await
                .unwrap()
        );
    }
}
