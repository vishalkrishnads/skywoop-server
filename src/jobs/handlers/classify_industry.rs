//! Classify project industry with Luna and seed posting schedules.

use serde_json::json;
use uuid::Uuid;

use crate::ai::planner::{self, IndustryClassificationOut};
use crate::jobs::JobError;
use crate::scheduling::industries::Industry;
use crate::scheduling::seed_from_industry;
use crate::state::AppState;

pub async fn run(state: &AppState, project_id: Uuid) -> Result<(), JobError> {
    let row: Option<(String, String, String, Option<String>, Option<String>)> = sqlx::query_as(
        r#"
        select name, description, coalesce(website, ''), market_country, industry
          from projects where id = $1
        "#,
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| JobError::Retry(e.into()))?;

    let Some((name, description, website, country, existing)) = row else {
        return Err(JobError::permanent(anyhow::anyhow!("project not found")));
    };

    // If user already set industry manually via PATCH, only seed schedules if missing.
    if let Some(ref ind_s) = existing
        && let Some(ind) = Industry::parse(ind_s)
    {
        seed_from_industry(&state.db, project_id, ind)
            .await
            .map_err(JobError::Retry)?;
        return Ok(());
    }

    let industries: Vec<_> = Industry::ALL
        .iter()
        .map(|i| {
            json!({
                "id": i.as_str(),
                "name": i.display_name(),
            })
        })
        .collect();

    let out: IndustryClassificationOut = planner::classify_industry(
        state.ai.as_ref(),
        &json!({
            "name": name,
            "description": description,
            "website": website,
            "market_country": country,
        }),
        &json!(industries),
    )
    .await
    .map_err(JobError::from_ai)?;

    let industry = Industry::parse(&out.industry).ok_or_else(|| {
        JobError::permanent(anyhow::anyhow!(
            "model returned unknown industry: {}",
            out.industry
        ))
    })?;

    sqlx::query(
        r#"
        update projects
           set industry = $2, updated_at = now()
         where id = $1
        "#,
    )
    .bind(project_id)
    .bind(industry.as_str())
    .execute(&state.db)
    .await
    .map_err(|e| JobError::Retry(e.into()))?;

    seed_from_industry(&state.db, project_id, industry)
        .await
        .map_err(JobError::Retry)?;

    tracing::info!(
        %project_id,
        industry = industry.as_str(),
        confidence = out.confidence,
        "classified industry"
    );
    Ok(())
}
