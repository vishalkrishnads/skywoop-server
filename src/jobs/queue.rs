use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::{DataJob, UserJob};

#[derive(Clone)]
pub struct UserJobQueue(pub PgPool);

#[derive(Clone)]
pub struct DataJobQueue(pub PgPool);

impl UserJobQueue {
    pub fn new(pool: PgPool) -> Self {
        Self(pool)
    }

    pub fn pool(&self) -> &PgPool {
        &self.0
    }

    /// Enqueue inside the caller's transaction so state change + job are atomic.
    pub async fn enqueue(
        &self,
        tx: &mut PgConnection,
        job: &UserJob,
        dedupe: Option<String>,
    ) -> sqlx::Result<Option<Uuid>> {
        enqueue_raw(tx, serde_json::to_value(job).unwrap(), dedupe, Utc::now()).await
    }

    /// Enqueue inside the caller's transaction with a future `run_at` (scheduled publish).
    pub async fn enqueue_at_tx(
        &self,
        tx: &mut PgConnection,
        job: &UserJob,
        dedupe: Option<String>,
        run_at: DateTime<Utc>,
    ) -> sqlx::Result<Option<Uuid>> {
        enqueue_raw(tx, serde_json::to_value(job).unwrap(), dedupe, run_at).await
    }

    /// Enqueue a delayed follow-up (e.g. PollIgContainer) outside a caller transaction.
    pub async fn enqueue_at(
        &self,
        job: &UserJob,
        dedupe: Option<String>,
        run_at: DateTime<Utc>,
    ) -> sqlx::Result<Option<Uuid>> {
        let mut tx = self.0.begin().await?;
        let id = enqueue_raw(&mut tx, serde_json::to_value(job).unwrap(), dedupe, run_at).await?;
        tx.commit().await?;
        Ok(id)
    }
}

impl DataJobQueue {
    pub fn new(pool: PgPool) -> Self {
        Self(pool)
    }

    pub fn pool(&self) -> &PgPool {
        &self.0
    }

    pub async fn enqueue(
        &self,
        tx: &mut PgConnection,
        job: &DataJob,
        dedupe: Option<String>,
        run_at: DateTime<Utc>,
    ) -> sqlx::Result<Option<Uuid>> {
        enqueue_raw(tx, serde_json::to_value(job).unwrap(), dedupe, run_at).await
    }
}

async fn enqueue_raw(
    tx: &mut PgConnection,
    v: Value,
    dedupe: Option<String>,
    run_at: DateTime<Utc>,
) -> sqlx::Result<Option<Uuid>> {
    let kind = v["kind"]
        .as_str()
        .ok_or_else(|| sqlx::Error::Protocol("job missing kind".into()))?
        .to_owned();
    let payload = v.get("payload").cloned().unwrap_or(Value::Null);
    let id = Uuid::now_v7();
    let row: Option<(Uuid,)> = sqlx::query_as(
        r#"
        insert into jobs (id, kind, payload, dedupe_key, run_at)
        values ($1, $2, $3, $4, $5)
        on conflict (dedupe_key) do nothing
        returning id
        "#,
    )
    .bind(id)
    .bind(kind)
    .bind(payload)
    .bind(dedupe)
    .bind(run_at)
    .fetch_optional(&mut *tx)
    .await?;
    Ok(row.map(|r| r.0))
}
