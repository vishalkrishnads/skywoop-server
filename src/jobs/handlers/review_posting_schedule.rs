//! Weekly agent review of posting schedule against audience activity.

use chrono::{Duration, Utc};
use serde_json::json;
use uuid::Uuid;

use crate::ai::{build_knowledge_pack, planner};
use crate::domain::Platform;
use crate::jobs::JobError;
use crate::scheduling::industries::Industry;
use crate::scheduling::{
    PrimeWindow, ScheduleSource, Slot, load_schedule_with_dayparts, online_followers_by_hour,
    seed_from_industry, upsert_schedule_full, validate_prime_windows,
};
use crate::state::AppState;

pub async fn run(state: &AppState, project_id: Uuid, platform: Platform) -> Result<(), JobError> {
    let proj: Option<(Option<String>, String)> =
        sqlx::query_as("select industry, timezone from projects where id = $1")
            .bind(project_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| JobError::Retry(e.into()))?;

    let Some((industry_s, tz)) = proj else {
        return Err(JobError::permanent(anyhow::anyhow!("project not found")));
    };

    let max_daily: i32 = sqlx::query_scalar(
        r#"
        select max_daily_posts from platform_workspaces
         where project_id = $1 and platform = $2
        "#,
    )
    .bind(project_id)
    .bind(platform.as_str())
    .fetch_optional(&state.db)
    .await
    .map_err(|e| JobError::Retry(e.into()))?
    .unwrap_or(10);

    let mut schedule = load_schedule_with_dayparts(&state.db, project_id, platform.as_str())
        .await
        .map_err(JobError::Retry)?;
    if schedule.is_none()
        && let Some(ind) = industry_s.as_deref().and_then(Industry::parse)
    {
        seed_from_industry(&state.db, project_id, ind)
            .await
            .map_err(JobError::Retry)?;
        schedule = load_schedule_with_dayparts(&state.db, project_id, platform.as_str())
            .await
            .map_err(JobError::Retry)?;
    }

    let Some(schedule) = schedule else {
        tracing::info!(%project_id, %platform, "no schedule and no industry; skip review");
        return Ok(());
    };

    let pack = build_knowledge_pack(&state.db, project_id, platform, None)
        .await
        .map_err(JobError::permanent)?;

    let since = (Utc::now() - Duration::days(28)).date_naive();
    let online = online_followers_by_hour(&state.db, project_id, since, &tz)
        .await
        .map_err(JobError::Retry)?;
    let online_json: Vec<_> = online
        .iter()
        .map(|(h, v)| json!({"hour": h, "avg_online": v}))
        .collect();

    let perf: Vec<(i32, Option<f64>, Option<f64>, i64)> = sqlx::query_as(
        r#"
        select extract(hour from e.published_at at time zone p.timezone)::int as hour,
               avg(e.reach::float8),
               avg(e.profile_visits::float8),
               count(*)::bigint
          from experiments e
          join projects p on p.id = e.project_id
         where e.project_id = $1 and e.platform = $2
           and e.published_at >= now() - interval '60 days'
         group by 1
         order by 1
        "#,
    )
    .bind(project_id)
    .bind(platform.as_str())
    .fetch_all(&state.db)
    .await
    .map_err(|e| JobError::Retry(e.into()))?;

    let perf_json: Vec<_> = perf
        .iter()
        .map(|(h, reach, visits, n)| {
            json!({
                "hour": h,
                "avg_reach_24hish": reach,
                "avg_profile_visits": visits,
                "n_posts": n,
            })
        })
        .collect();

    let industry_prior = if let Some(ind) = industry_s.as_deref().and_then(Industry::parse) {
        json!({
            "industry": ind.as_str(),
            "display": ind.display_name(),
            "allowed_weekdays": ind.allowed_weekdays(),
            "seed_slots": crate::scheduling::industries::seed_slots_for_industry(ind, max_daily),
        })
    } else {
        json!({})
    };

    let current_json = json!({
        "slots": schedule.slots,
        "prime_windows": schedule.prime_windows,
        "dayparts": schedule.dayparts,
        "source": schedule.source.as_str(),
        "rationale": schedule.rationale,
        "version": schedule.version,
    });

    let out = planner::adjust_post_timing(
        state.ai.as_ref(),
        &pack,
        &current_json,
        &industry_prior,
        &json!(online_json),
        &json!(perf_json),
        max_daily,
    )
    .await
    .map_err(JobError::from_ai)?;

    if out.action.to_ascii_lowercase() != "update" {
        tracing::info!(%project_id, %platform, "schedule kept");
        return Ok(());
    }

    let mut prime_windows: Vec<PrimeWindow> = out
        .prime_windows
        .iter()
        .filter_map(|w| PrimeWindow::new(w.weekday, w.start_minute, w.end_minute))
        .collect();

    // Fallback: derive prime windows from legacy slots (±30 min).
    if prime_windows.is_empty() {
        for s in &out.slots {
            let start = s.minute.saturating_sub(30);
            let end = (s.minute + 30).min(1439);
            if let Some(w) = PrimeWindow::new(s.weekday, start, end) {
                prime_windows.push(w);
            }
        }
    }

    if let Err(e) = validate_prime_windows(&prime_windows) {
        return Err(JobError::permanent(anyhow::anyhow!(
            "invalid agent prime_windows: {e}"
        )));
    }

    let slots: Vec<Slot> = prime_windows
        .iter()
        .filter_map(|w| {
            let mid = w.start_minute + (w.end_minute - w.start_minute) / 2;
            Slot::new(w.weekday, mid)
        })
        .collect();

    if schedule.source == ScheduleSource::Manual {
        let _ = sqlx::query(
            r#"
            insert into posting_schedule_changes (
                id, project_id, platform, previous_slots, new_slots, source, rationale
            ) values ($1,$2,$3,$4,$5,'agent',$6)
            "#,
        )
        .bind(crate::domain::new_id())
        .bind(project_id)
        .bind(platform.as_str())
        .bind(serde_json::to_value(&schedule.prime_windows).unwrap_or(json!([])))
        .bind(serde_json::to_value(&prime_windows).unwrap_or(json!([])))
        .bind(format!("[manual-locked suggestion] {}", out.rationale))
        .execute(&state.db)
        .await;
        tracing::info!(%project_id, %platform, "manual schedule locked; suggestion recorded");
        return Ok(());
    }

    let dayparts = schedule.dayparts.clone();
    let updated = upsert_schedule_full(
        &state.db,
        project_id,
        platform.as_str(),
        &slots,
        &prime_windows,
        &dayparts,
        ScheduleSource::Agent,
        &out.rationale,
    )
    .await
    .map_err(JobError::Retry)?;

    tracing::info!(
        %project_id,
        %platform,
        version = updated.version,
        "schedule prime_windows updated by agent"
    );
    Ok(())
}
