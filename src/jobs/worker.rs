use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Timelike, Utc};
use serde_json::{Value, json};
use sqlx::FromRow;
use tokio::sync::Semaphore;
use uuid::Uuid;

use super::handlers;
use super::{DataJob, JobError, UserJob};
use crate::state::AppState;

#[derive(Debug, FromRow)]
struct ClaimedJob {
    id: Uuid,
    kind: String,
    payload: Value,
    attempts: i32,
    max_attempts: i32,
}

pub async fn recover_stale_locks(pool: &sqlx::PgPool) -> anyhow::Result<()> {
    let n = sqlx::query(
        r#"
        update jobs
           set status = 'queued', locked_at = null, locked_by = null
         where status = 'running'
           and locked_at < now() - interval '15 minutes'
        "#,
    )
    .execute(pool)
    .await?
    .rows_affected();
    if n > 0 {
        tracing::warn!(count = n, "requeued stale running jobs");
    }
    Ok(())
}

pub async fn run_worker(state: AppState) {
    let worker_id = format!("worker-{}", Uuid::now_v7());
    if let Err(e) = recover_stale_locks(&state.db).await {
        tracing::error!(error = ?e, "stale lock recovery failed");
    }
    let sem = Arc::new(Semaphore::new(4));
    loop {
        match claim_job(&state.db, &worker_id).await {
            Ok(Some(job)) => {
                let permit = sem.clone().acquire_owned().await.expect("semaphore");
                let state2 = state.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(e) = execute_job(&state2, job).await {
                        tracing::error!(error = ?e, "job execution bookkeeping failed");
                    }
                });
            }
            Ok(None) => {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(e) => {
                tracing::error!(error = ?e, "claim job failed");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

async fn claim_job(pool: &sqlx::PgPool, worker_id: &str) -> sqlx::Result<Option<ClaimedJob>> {
    sqlx::query_as::<_, ClaimedJob>(
        r#"
        update jobs
           set status = 'running', locked_at = now(), locked_by = $1, attempts = attempts + 1
         where id = (
            select id from jobs
             where status = 'queued' and run_at <= now()
             order by run_at
             for update skip locked
             limit 1
         )
        returning id, kind, payload, attempts, max_attempts
        "#,
    )
    .bind(worker_id)
    .fetch_optional(pool)
    .await
}

async fn execute_job(state: &AppState, job: ClaimedJob) -> anyhow::Result<()> {
    let wrapped = json!({ "kind": job.kind, "payload": job.payload });
    let result = dispatch(state, &wrapped).await;
    match result {
        Ok(()) => {
            sqlx::query(
                r#"
                update jobs
                   set status = 'succeeded', finished_at = now(), last_error = null
                 where id = $1
                "#,
            )
            .bind(job.id)
            .execute(&state.db)
            .await?;
        }
        Err(e) if e.is_permanent() || job.attempts >= job.max_attempts => {
            sqlx::query(
                r#"
                update jobs
                   set status = 'failed', finished_at = now(), last_error = $2
                 where id = $1
                "#,
            )
            .bind(job.id)
            .bind(e.to_string())
            .execute(&state.db)
            .await?;
        }
        Err(e) => {
            let delay_mins = e
                .retry_delay_minutes()
                .unwrap_or_else(|| 2i64.pow(job.attempts as u32).min(60));
            let run_at = Utc::now() + chrono::Duration::minutes(delay_mins);
            sqlx::query(
                r#"
                update jobs
                   set status = 'queued', run_at = $2, locked_at = null, locked_by = null,
                       last_error = $3
                 where id = $1
                "#,
            )
            .bind(job.id)
            .bind(run_at)
            .bind(e.to_string())
            .execute(&state.db)
            .await?;
        }
    }
    Ok(())
}

async fn dispatch(state: &AppState, wrapped: &Value) -> Result<(), JobError> {
    if let Ok(job) = serde_json::from_value::<UserJob>(wrapped.clone()) {
        return handlers::handle_user(state, job).await;
    }
    if let Ok(job) = serde_json::from_value::<DataJob>(wrapped.clone()) {
        return handlers::handle_data(state, job).await;
    }
    Err(JobError::permanent(anyhow::anyhow!(
        "unknown job kind: {}",
        wrapped["kind"]
    )))
}

pub fn trunc_hour(now: DateTime<Utc>) -> DateTime<Utc> {
    now.date_naive()
        .and_hms_opt(now.hour(), 0, 0)
        .unwrap()
        .and_utc()
}
