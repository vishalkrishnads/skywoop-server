//! ASC instance upsert helpers and daily ingest.

use chrono::NaiveDate;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::analysis::rollups;
use crate::appstore::client::{AscAggregate, parse_asc_tsv};
use crate::jobs::JobError;
use crate::state::AppState;

const ENGAGEMENT_NAME: &str = "App Store Discovery and Engagement Detailed";
const DOWNLOADS_NAME: &str = "App Downloads Detailed";

/// Ensure ONGOING report request, discover reports, ingest new DAILY instances.
pub async fn asc_daily_ingest(state: &AppState, app_id: &str) -> Result<(), JobError> {
    if state.cfg.asc_issuer_id.is_empty() || state.cfg.asc_key_id.is_empty() {
        return Err(JobError::permanent(anyhow::anyhow!(
            "ASC_ISSUER_ID / ASC_KEY_ID not configured"
        )));
    }

    let now = state.clock.now();
    let token = state
        .asc
        .bearer_token(state.secrets.as_ref(), now)
        .await
        .map_err(JobError::Retry)?;

    let request_id = ensure_report_request(state, app_id, &token).await?;

    let reports = state
        .asc
        .get_all_data(
            &format!("v1/analyticsReportRequests/{request_id}/reports"),
            &[],
            &token,
        )
        .await
        .map_err(JobError::Retry)?;

    let wanted: Vec<&Value> = reports
        .iter()
        .filter(|r| {
            let name = r
                .pointer("/attributes/name")
                .and_then(|n| n.as_str())
                .unwrap_or("");
            name.eq_ignore_ascii_case(ENGAGEMENT_NAME) || name.eq_ignore_ascii_case(DOWNLOADS_NAME)
        })
        .collect();

    if wanted.len() < 2 {
        let available: Vec<_> = reports
            .iter()
            .filter_map(|r| r.pointer("/attributes/name").and_then(|n| n.as_str()))
            .collect();
        tracing::warn!(
            app_id,
            ?available,
            "ASC report names missing expected Engagement/Downloads Detailed"
        );
    }

    for report in wanted {
        let report_id = report
            .get("id")
            .and_then(|i| i.as_str())
            .ok_or_else(|| JobError::Retry(anyhow::anyhow!("report missing id")))?;
        let report_name = report
            .pointer("/attributes/name")
            .and_then(|n| n.as_str())
            .unwrap_or("unknown")
            .to_owned();

        let instances = state
            .asc
            .get_all_data(
                &format!("v1/analyticsReports/{report_id}/instances"),
                &[("filter[granularity]", "DAILY")],
                &token,
            )
            .await
            .map_err(JobError::Retry)?;

        for inst in instances {
            let instance_id = match inst.get("id").and_then(|i| i.as_str()) {
                Some(id) => id.to_owned(),
                None => continue,
            };
            let already: bool = sqlx::query_scalar(
                "select exists(select 1 from asc_ingested_instances where instance_id = $1)",
            )
            .bind(&instance_id)
            .fetch_one(&state.db)
            .await
            .map_err(|e| JobError::Retry(e.into()))?;
            if already {
                continue;
            }

            let processing_date = parse_processing_date(&inst).ok_or_else(|| {
                JobError::Retry(anyhow::anyhow!(
                    "instance {instance_id} missing processingDate"
                ))
            })?;

            let segments = state
                .asc
                .get_all_data(
                    &format!("v1/analyticsReportInstances/{instance_id}/segments"),
                    &[],
                    &token,
                )
                .await
                .map_err(JobError::Retry)?;

            let mut agg = AscAggregate::default();
            for seg in &segments {
                let url = seg
                    .pointer("/attributes/url")
                    .and_then(|u| u.as_str())
                    .ok_or_else(|| JobError::Retry(anyhow::anyhow!("segment missing url")))?;
                let bytes = state
                    .asc
                    .download_bytes(url)
                    .await
                    .map_err(JobError::Retry)?;
                let part = parse_asc_tsv(&bytes).map_err(JobError::Retry)?;
                agg.merge(part);
            }

            upsert_instance(
                &state.db,
                app_id,
                &instance_id,
                &report_name,
                processing_date,
                &agg,
            )
            .await
            .map_err(JobError::Retry)?;
        }
    }

    recompute_for_app(state, app_id)
        .await
        .map_err(JobError::Retry)?;

    Ok(())
}

async fn ensure_report_request(
    state: &AppState,
    app_id: &str,
    token: &str,
) -> Result<String, JobError> {
    if let Some((request_id,)) = sqlx::query_as::<_, (String,)>(
        "select request_id from asc_report_requests where app_id = $1",
    )
    .bind(app_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| JobError::Retry(e.into()))?
        && request_id != ADMIN_NEEDED_SENTINEL
    {
        let (status, body) = state
            .asc
            .get_json(
                &format!("v1/analyticsReportRequests/{request_id}"),
                &[],
                token,
            )
            .await
            .map_err(JobError::Retry)?;
        let stopped = body
            .pointer("/data/attributes/stoppedDueToInactivity")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if (200..300).contains(&status) && !stopped {
            sqlx::query("update asc_report_requests set last_seen_ok_at = now() where app_id = $1")
                .bind(app_id)
                .execute(&state.db)
                .await
                .map_err(|e| JobError::Retry(e.into()))?;
            return Ok(request_id);
        }
    }

    let existing = state
        .asc
        .get_all_data(
            &format!("v1/apps/{app_id}/analyticsReportRequests"),
            &[("filter[accessType]", "ONGOING")],
            token,
        )
        .await
        .map_err(JobError::Retry)?;

    for req in &existing {
        let stopped = req
            .pointer("/attributes/stoppedDueToInactivity")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if stopped {
            continue;
        }
        if let Some(id) = req.get("id").and_then(|i| i.as_str()) {
            store_request(&state.db, app_id, id)
                .await
                .map_err(JobError::Retry)?;
            return Ok(id.to_owned());
        }
    }

    let body = json!({
        "data": {
            "type": "analyticsReportRequests",
            "attributes": { "accessType": "ONGOING" },
            "relationships": {
                "app": {
                    "data": { "type": "apps", "id": app_id }
                }
            }
        }
    });
    let (status, resp) = state
        .asc
        .post_json("v1/analyticsReportRequests", &body, token)
        .await
        .map_err(JobError::Retry)?;

    if status == 403 {
        store_admin_needed(&state.db, app_id)
            .await
            .map_err(JobError::Retry)?;
        return Err(JobError::permanent(anyhow::anyhow!(
            "App Store key needs Admin role to start reporting"
        )));
    }
    if !(200..300).contains(&status) {
        return Err(JobError::Retry(anyhow::anyhow!(
            "create analyticsReportRequests failed: http {status}: {resp}"
        )));
    }

    let id = resp
        .pointer("/data/id")
        .and_then(|i| i.as_str())
        .ok_or_else(|| JobError::Retry(anyhow::anyhow!("create response missing id")))?
        .to_owned();
    store_request(&state.db, app_id, &id)
        .await
        .map_err(JobError::Retry)?;
    Ok(id)
}

async fn store_request(pool: &PgPool, app_id: &str, request_id: &str) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        insert into asc_report_requests (app_id, request_id, created_at, last_seen_ok_at)
        values ($1, $2, now(), now())
        on conflict (app_id) do update set
            request_id = excluded.request_id,
            last_seen_ok_at = now()
        "#,
    )
    .bind(app_id)
    .bind(request_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Sentinel request_id used when Admin role is missing (surfaced in status API).
pub const ADMIN_NEEDED_SENTINEL: &str = "__admin_needed__";

async fn store_admin_needed(pool: &PgPool, app_id: &str) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        insert into asc_report_requests (app_id, request_id, created_at, last_seen_ok_at)
        values ($1, $2, now(), null)
        on conflict (app_id) do update set
            request_id = excluded.request_id,
            last_seen_ok_at = null
        "#,
    )
    .bind(app_id)
    .bind(ADMIN_NEEDED_SENTINEL)
    .execute(pool)
    .await?;
    Ok(())
}

fn parse_processing_date(inst: &Value) -> Option<NaiveDate> {
    let s = inst
        .pointer("/attributes/processingDate")
        .and_then(|v| v.as_str())?;
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

/// Upsert aggregates and mark instance ingested (one transaction).
pub async fn upsert_instance(
    pool: &PgPool,
    app_id: &str,
    instance_id: &str,
    report_name: &str,
    processing_date: NaiveDate,
    agg: &AscAggregate,
) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;

    for ((date, campaign, metric), value) in &agg.campaign {
        sqlx::query(
            r#"
            insert into asc_campaign_daily (app_id, date, campaign, metric, value, processing_date)
            values ($1, $2, $3, $4, $5, $6)
            on conflict (app_id, date, campaign, metric) do update
               set value = excluded.value, processing_date = excluded.processing_date
             where excluded.processing_date >= asc_campaign_daily.processing_date
            "#,
        )
        .bind(app_id)
        .bind(date)
        .bind(campaign)
        .bind(metric)
        .bind(value)
        .bind(processing_date)
        .execute(&mut *tx)
        .await?;
    }

    for ((date, source_info, metric), value) in &agg.source {
        sqlx::query(
            r#"
            insert into asc_source_daily (app_id, date, source_info, metric, value, processing_date)
            values ($1, $2, $3, $4, $5, $6)
            on conflict (app_id, date, source_info, metric) do update
               set value = excluded.value, processing_date = excluded.processing_date
             where excluded.processing_date >= asc_source_daily.processing_date
            "#,
        )
        .bind(app_id)
        .bind(date)
        .bind(source_info)
        .bind(metric)
        .bind(value)
        .bind(processing_date)
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query(
        r#"
        insert into asc_ingested_instances (instance_id, app_id, report_name, processing_date, ingested_at)
        values ($1, $2, $3, $4, now())
        on conflict (instance_id) do nothing
        "#,
    )
    .bind(instance_id)
    .bind(app_id)
    .bind(report_name)
    .bind(processing_date)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

async fn recompute_for_app(state: &AppState, app_id: &str) -> anyhow::Result<()> {
    let ids: Vec<(Uuid,)> = sqlx::query_as(
        r#"
        select e.id
          from experiments e
          join projects p on p.id = e.project_id
          join tracked_links tl on tl.plan_video_id = e.plan_video_id
         where p.app_store_app_id = $1
           and exists (
             select 1 from asc_campaign_daily d where d.campaign = tl.token
           )
        "#,
    )
    .bind(app_id)
    .fetch_all(&state.db)
    .await?;

    for (id,) in ids {
        rollups::recompute(&state.db, id).await?;
    }
    Ok(())
}

/// Create / ensure an ONGOING report request for a project's App Store app.
pub async fn ensure_report_request_for_project(
    state: &AppState,
    project_id: Uuid,
) -> Result<String, JobError> {
    let app_id: Option<String> =
        sqlx::query_scalar("select app_store_app_id from projects where id = $1")
            .bind(project_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| JobError::Retry(e.into()))?
            .flatten();

    let app_id = app_id
        .ok_or_else(|| JobError::permanent(anyhow::anyhow!("project has no app_store_app_id")))?;

    let now = state.clock.now();
    let token = state
        .asc
        .bearer_token(state.secrets.as_ref(), now)
        .await
        .map_err(JobError::Retry)?;
    ensure_report_request(state, &app_id, &token).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appstore::client::parse_asc_tsv;
    use chrono::NaiveDate;

    #[sqlx::test(migrations = "./migrations")]
    async fn older_processing_date_does_not_overwrite(pool: PgPool) {
        let app = "app1";
        let d = NaiveDate::from_ymd_opt(2024, 1, 15).unwrap();
        let newer = NaiveDate::from_ymd_opt(2024, 1, 20).unwrap();
        let older = NaiveDate::from_ymd_opt(2024, 1, 10).unwrap();

        let mut agg_new = AscAggregate::default();
        agg_new
            .campaign
            .insert((d, "tok".into(), "impressions".into()), 100);
        upsert_instance(&pool, app, "inst-new", "Engagement", newer, &agg_new)
            .await
            .unwrap();

        let mut agg_old = AscAggregate::default();
        agg_old
            .campaign
            .insert((d, "tok".into(), "impressions".into()), 1);
        upsert_instance(&pool, app, "inst-old", "Engagement", older, &agg_old)
            .await
            .unwrap();

        let value: i64 = sqlx::query_scalar(
            r#"
            select value from asc_campaign_daily
             where app_id = $1 and date = $2 and campaign = 'tok' and metric = 'impressions'
            "#,
        )
        .bind(app)
        .bind(d)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(value, 100);

        let processing: NaiveDate = sqlx::query_scalar(
            r#"
            select processing_date from asc_campaign_daily
             where app_id = $1 and date = $2 and campaign = 'tok' and metric = 'impressions'
            "#,
        )
        .bind(app)
        .bind(d)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(processing, newer);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn source_rows_without_campaign(pool: PgPool) {
        let path = format!(
            "{}/fixtures/asc/engagement_detailed.txt",
            env!("CARGO_MANIFEST_DIR")
        );
        let bytes = std::fs::read(path).unwrap();
        let agg = parse_asc_tsv(&bytes).unwrap();
        assert!(!agg.source.is_empty());
        upsert_instance(
            &pool,
            "app2",
            "inst-src",
            "Engagement",
            NaiveDate::from_ymd_opt(2024, 1, 15).unwrap(),
            &agg,
        )
        .await
        .unwrap();
        let n: i64 = sqlx::query_scalar(
            "select count(*)::bigint from asc_source_daily where app_id = 'app2'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(n > 0);
    }
}
