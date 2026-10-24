//! EvaluateMaturity data job: apply maturity rules and enqueue analysis.

use chrono::{DateTime, Utc};
use sqlx::FromRow;
use uuid::Uuid;

use crate::analysis::maturity::{self, MaturityFacts};
use crate::domain::Maturity;
use crate::jobs::{DataJob, DataJobQueue, JobError};
use crate::state::AppState;

#[derive(Debug, FromRow)]
struct CollectingRow {
    id: Uuid,
    project_id: Uuid,
    platform: String,
    published_at: DateTime<Utc>,
    publish_local_date: chrono::NaiveDate,
    views: Option<i64>,
    app_store_app_id: Option<String>,
    min_evidence_views: i32,
}

pub async fn evaluate_maturity(state: &AppState, _hour: DateTime<Utc>) -> Result<(), JobError> {
    let now = state.clock.now();
    let rows: Vec<CollectingRow> = sqlx::query_as(
        r#"
        select e.id, e.project_id, e.platform, e.published_at, e.publish_local_date, e.views,
               p.app_store_app_id, pw.min_evidence_views
          from experiments e
          join projects p on p.id = e.project_id
          join platform_workspaces pw
            on pw.project_id = e.project_id and pw.platform = e.platform
         where e.maturity = 'collecting'
        "#,
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| JobError::Retry(e.into()))?;

    let data_jobs = DataJobQueue::new(state.db.clone());

    for row in rows {
        let last_snapshot_at: Option<DateTime<Utc>> = sqlx::query_scalar(
            r#"
            select max(fetched_at) from post_metric_snapshots where experiment_id = $1
            "#,
        )
        .bind(row.id)
        .fetch_one(&state.db)
        .await
        .map_err(|e| JobError::Retry(e.into()))?;

        let asc_configured = row.app_store_app_id.is_some();
        let asc_max_ingested_date: Option<chrono::NaiveDate> =
            if let Some(ref app_id) = row.app_store_app_id {
                sqlx::query_scalar(
                    "select max(processing_date) from asc_ingested_instances where app_id = $1",
                )
                .bind(app_id)
                .fetch_one(&state.db)
                .await
                .map_err(|e| JobError::Retry(e.into()))?
            } else {
                None
            };

        let facts = MaturityFacts {
            published_at: row.published_at,
            publish_local_date: row.publish_local_date,
            last_snapshot_at,
            views: row.views,
            asc_configured,
            asc_max_ingested_date,
        };
        let (m, meta_complete, asc_complete) =
            maturity::evaluate(&facts, now, row.min_evidence_views as i64);

        let prev: String = sqlx::query_scalar("select maturity from experiments where id = $1")
            .bind(row.id)
            .fetch_one(&state.db)
            .await
            .map_err(|e| JobError::Retry(e.into()))?;

        let matured_at = match m {
            Maturity::Mature | Maturity::Inconclusive if prev == "collecting" => Some(now),
            _ => None,
        };

        sqlx::query(
            r#"
            update experiments set
                maturity = $2,
                meta_complete = $3,
                asc_complete = $4,
                matured_at = coalesce($5, matured_at),
                updated_at = now()
             where id = $1
            "#,
        )
        .bind(row.id)
        .bind(m.as_str())
        .bind(meta_complete)
        .bind(asc_complete)
        .bind(matured_at)
        .execute(&state.db)
        .await
        .map_err(|e| JobError::Retry(e.into()))?;

        // Recompute asc_no_data now that asc_complete may have flipped.
        if asc_complete {
            let _ = crate::analysis::rollups::recompute(&state.db, row.id).await;
        }

        if prev == "collecting" && matches!(m, Maturity::Mature | Maturity::Inconclusive) {
            let mut tx = state
                .db
                .begin()
                .await
                .map_err(|e| JobError::Retry(e.into()))?;
            data_jobs
                .enqueue(
                    &mut tx,
                    &DataJob::AnalyzeExperiment {
                        experiment_id: row.id,
                    },
                    Some(format!("analyze:{}", row.id)),
                    now,
                )
                .await
                .map_err(|e| JobError::Retry(e.into()))?;
            tx.commit().await.map_err(|e| JobError::Retry(e.into()))?;
        }
    }

    Ok(())
}
