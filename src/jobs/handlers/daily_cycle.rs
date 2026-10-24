use chrono::{Duration, NaiveDate};
use uuid::Uuid;

use super::{plan_day, update_diary};
use crate::domain::Platform;
use crate::jobs::JobError;
use crate::state::AppState;

pub async fn run(
    state: &AppState,
    project_id: Uuid,
    platform: Platform,
    local_date: NaiveDate,
) -> Result<(), JobError> {
    update_diary::run(state, project_id, platform, local_date).await?;
    let for_date = local_date + Duration::days(1);
    plan_day::run(state, project_id, platform, for_date, true).await?;
    Ok(())
}
