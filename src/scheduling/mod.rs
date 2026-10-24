//! Posting schedules: industry priors, slot picking, and agent adjustments.

pub mod daily;
pub mod industries;

pub use daily::*;

use chrono::{
    DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Timelike, Utc, Weekday,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::domain::new_id;
use crate::scheduling::industries::{
    Industry, prime_windows_for_industry, seed_slots_for_industry,
};

/// One posting slot: weekday (Mon=0 … Sun=6) and minutes since midnight local.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct Slot {
    /// 0 = Monday … 6 = Sunday (ISO weekday - 1).
    pub weekday: u8,
    /// Minutes since local midnight (0..1440).
    pub minute: u16,
}

impl Slot {
    pub fn new(weekday: u8, minute: u16) -> Option<Self> {
        if weekday > 6 || minute >= 1440 {
            return None;
        }
        Some(Self { weekday, minute })
    }

    pub fn hour_minute(self) -> (u32, u32) {
        (self.minute as u32 / 60, self.minute as u32 % 60)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleSource {
    IndustryDefault,
    Agent,
    Manual,
}

impl ScheduleSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::IndustryDefault => "industry_default",
            Self::Agent => "agent",
            Self::Manual => "manual",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "industry_default" => Some(Self::IndustryDefault),
            "agent" => Some(Self::Agent),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostingSchedule {
    pub project_id: Uuid,
    pub platform: String,
    pub slots: Vec<Slot>,
    pub source: ScheduleSource,
    pub rationale: String,
    pub version: i32,
}

/// Schedule row including daily-plan timing fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostingScheduleDayparts {
    pub project_id: Uuid,
    pub platform: String,
    pub slots: Vec<Slot>,
    pub prime_windows: Vec<PrimeWindow>,
    pub dayparts: DaypartWindows,
    pub source: ScheduleSource,
    pub rationale: String,
    pub version: i32,
}

/// Validate slots: weekday 0-6, minute < 1440, ≤ max_daily_posts per day, ≥ 2h apart same day.
pub fn validate_slots(slots: &[Slot], max_daily_posts: i32) -> Result<(), String> {
    if slots.is_empty() {
        return Err("schedule needs at least one slot".into());
    }
    for s in slots {
        if s.weekday > 6 {
            return Err(format!("weekday out of range: {}", s.weekday));
        }
        if s.minute >= 1440 {
            return Err(format!("minute out of range: {}", s.minute));
        }
    }
    for day in 0u8..=6 {
        let mut mins: Vec<u16> = slots
            .iter()
            .filter(|s| s.weekday == day)
            .map(|s| s.minute)
            .collect();
        if mins.len() as i32 > max_daily_posts {
            return Err(format!(
                "day {day} has {} slots; max_daily_posts is {max_daily_posts}",
                mins.len()
            ));
        }
        mins.sort_unstable();
        for w in mins.windows(2) {
            if w[1] - w[0] < 120 {
                return Err(format!(
                    "day {day}: slots {} and {} are less than 2 hours apart",
                    w[0], w[1]
                ));
            }
        }
    }
    Ok(())
}

/// Seed schedules for both platforms from an industry prior, unless already agent/manual.
pub async fn seed_from_industry(
    pool: &PgPool,
    project_id: Uuid,
    industry: Industry,
) -> anyhow::Result<()> {
    let workspaces: Vec<(String, i32)> = sqlx::query_as(
        r#"
        select platform, max_daily_posts
          from platform_workspaces
         where project_id = $1
        "#,
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    for (platform, max_daily) in workspaces {
        let existing: Option<(String,)> = sqlx::query_as(
            r#"
            select source from posting_schedules
             where project_id = $1 and platform = $2
            "#,
        )
        .bind(project_id)
        .bind(&platform)
        .fetch_optional(pool)
        .await?;

        if let Some((src,)) = &existing
            && (src == "agent" || src == "manual")
        {
            continue;
        }

        let slots = seed_slots_for_industry(industry, max_daily);
        let prime_windows = prime_windows_for_industry(industry);
        upsert_schedule_full(
            pool,
            project_id,
            &platform,
            &slots,
            &prime_windows,
            &DaypartWindows::default(),
            ScheduleSource::IndustryDefault,
            &format!("Seeded from industry prior: {}", industry.as_str()),
        )
        .await?;
    }
    Ok(())
}

pub async fn upsert_schedule(
    pool: &PgPool,
    project_id: Uuid,
    platform: &str,
    slots: &[Slot],
    source: ScheduleSource,
    rationale: &str,
) -> anyhow::Result<PostingSchedule> {
    let slots_json = serde_json::to_value(slots)?;
    let prev: Option<(serde_json::Value, i32)> = sqlx::query_as(
        r#"
        select slots, version from posting_schedules
         where project_id = $1 and platform = $2
        "#,
    )
    .bind(project_id)
    .bind(platform)
    .fetch_optional(pool)
    .await?;

    let version = prev.as_ref().map(|(_, v)| v + 1).unwrap_or(1);

    sqlx::query(
        r#"
        insert into posting_schedules (project_id, platform, slots, source, rationale, version, updated_at)
        values ($1,$2,$3,$4,$5,$6,now())
        on conflict (project_id, platform) do update set
            slots = excluded.slots,
            source = excluded.source,
            rationale = excluded.rationale,
            version = excluded.version,
            updated_at = now()
        "#,
    )
    .bind(project_id)
    .bind(platform)
    .bind(&slots_json)
    .bind(source.as_str())
    .bind(rationale)
    .bind(version)
    .execute(pool)
    .await?;

    let change_id = new_id();
    sqlx::query(
        r#"
        insert into posting_schedule_changes (
            id, project_id, platform, previous_slots, new_slots, source, rationale
        ) values ($1,$2,$3,$4,$5,$6,$7)
        "#,
    )
    .bind(change_id)
    .bind(project_id)
    .bind(platform)
    .bind(prev.map(|(s, _)| s))
    .bind(&slots_json)
    .bind(source.as_str())
    .bind(rationale)
    .execute(pool)
    .await?;

    Ok(PostingSchedule {
        project_id,
        platform: platform.to_owned(),
        slots: slots.to_vec(),
        source,
        rationale: rationale.to_owned(),
        version,
    })
}

pub async fn upsert_schedule_full(
    pool: &PgPool,
    project_id: Uuid,
    platform: &str,
    slots: &[Slot],
    prime_windows: &[PrimeWindow],
    dayparts: &DaypartWindows,
    source: ScheduleSource,
    rationale: &str,
) -> anyhow::Result<PostingScheduleDayparts> {
    let slots_json = serde_json::to_value(slots)?;
    let prime_json = serde_json::to_value(prime_windows)?;
    let dayparts_json = serde_json::to_value(dayparts)?;
    let prev: Option<(serde_json::Value, i32)> = sqlx::query_as(
        r#"
        select slots, version from posting_schedules
         where project_id = $1 and platform = $2
        "#,
    )
    .bind(project_id)
    .bind(platform)
    .fetch_optional(pool)
    .await?;

    let version = prev.as_ref().map(|(_, v)| v + 1).unwrap_or(1);

    sqlx::query(
        r#"
        insert into posting_schedules (
            project_id, platform, slots, prime_windows, dayparts, source, rationale, version, updated_at
        )
        values ($1,$2,$3,$4,$5,$6,$7,$8,now())
        on conflict (project_id, platform) do update set
            slots = excluded.slots,
            prime_windows = excluded.prime_windows,
            dayparts = excluded.dayparts,
            source = excluded.source,
            rationale = excluded.rationale,
            version = excluded.version,
            updated_at = now()
        "#,
    )
    .bind(project_id)
    .bind(platform)
    .bind(&slots_json)
    .bind(&prime_json)
    .bind(&dayparts_json)
    .bind(source.as_str())
    .bind(rationale)
    .bind(version)
    .execute(pool)
    .await?;

    let change_id = new_id();
    sqlx::query(
        r#"
        insert into posting_schedule_changes (
            id, project_id, platform, previous_slots, new_slots, source, rationale
        ) values ($1,$2,$3,$4,$5,$6,$7)
        "#,
    )
    .bind(change_id)
    .bind(project_id)
    .bind(platform)
    .bind(prev.map(|(s, _)| s))
    .bind(&slots_json)
    .bind(source.as_str())
    .bind(rationale)
    .execute(pool)
    .await?;

    Ok(PostingScheduleDayparts {
        project_id,
        platform: platform.to_owned(),
        slots: slots.to_vec(),
        prime_windows: prime_windows.to_vec(),
        dayparts: dayparts.clone(),
        source,
        rationale: rationale.to_owned(),
        version,
    })
}

pub async fn load_schedule(
    pool: &PgPool,
    project_id: Uuid,
    platform: &str,
) -> anyhow::Result<Option<PostingSchedule>> {
    let row: Option<(serde_json::Value, String, String, i32)> = sqlx::query_as(
        r#"
        select slots, source, rationale, version
          from posting_schedules
         where project_id = $1 and platform = $2
        "#,
    )
    .bind(project_id)
    .bind(platform)
    .fetch_optional(pool)
    .await?;

    let Some((slots_v, source, rationale, version)) = row else {
        return Ok(None);
    };
    let slots: Vec<Slot> = serde_json::from_value(slots_v)?;
    Ok(Some(PostingSchedule {
        project_id,
        platform: platform.to_owned(),
        slots,
        source: ScheduleSource::parse(&source).unwrap_or(ScheduleSource::IndustryDefault),
        rationale,
        version,
    }))
}

pub async fn load_schedule_with_dayparts(
    pool: &PgPool,
    project_id: Uuid,
    platform: &str,
) -> anyhow::Result<Option<PostingScheduleDayparts>> {
    let row: Option<(
        serde_json::Value,
        serde_json::Value,
        serde_json::Value,
        String,
        String,
        i32,
    )> = sqlx::query_as(
        r#"
            select slots, prime_windows, dayparts, source, rationale, version
              from posting_schedules
             where project_id = $1 and platform = $2
            "#,
    )
    .bind(project_id)
    .bind(platform)
    .fetch_optional(pool)
    .await?;

    let Some((slots_v, prime_v, dayparts_v, source, rationale, version)) = row else {
        return Ok(None);
    };
    let slots: Vec<Slot> = serde_json::from_value(slots_v)?;
    let prime_windows: Vec<PrimeWindow> = serde_json::from_value(prime_v)?;
    let dayparts: DaypartWindows = serde_json::from_value(dayparts_v).unwrap_or_default();
    Ok(Some(PostingScheduleDayparts {
        project_id,
        platform: platform.to_owned(),
        slots,
        prime_windows,
        dayparts,
        source: ScheduleSource::parse(&source).unwrap_or(ScheduleSource::IndustryDefault),
        rationale,
        version,
    }))
}

/// Earliest free slot at least `min_lead` from now, skipping taken times.
pub async fn next_free_slot(
    pool: &PgPool,
    project_id: Uuid,
    platform: &str,
    timezone: &str,
    now: DateTime<Utc>,
    min_lead: Duration,
) -> anyhow::Result<Option<DateTime<Utc>>> {
    let schedule = load_schedule(pool, project_id, platform).await?;
    let Some(schedule) = schedule else {
        // No schedule yet — fall back to now + min_lead.
        return Ok(Some(now + min_lead));
    };
    if schedule.slots.is_empty() {
        return Ok(Some(now + min_lead));
    }

    let tz: chrono_tz::Tz = timezone
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid timezone {timezone}"))?;

    let taken: Vec<(Option<DateTime<Utc>>, Option<DateTime<Utc>>)> = sqlx::query_as(
        r#"
        select scheduled_for, published_at
          from posts
         where platform = $1
           and project_id = $2
           and status in ('scheduled','pending','container_created','publishing','published')
        "#,
    )
    .bind(platform)
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    let taken_times: Vec<DateTime<Utc>> = taken.into_iter().filter_map(|(s, p)| s.or(p)).collect();

    let earliest = now + min_lead;
    // Search up to 21 days ahead.
    for day_offset in 0..21 {
        let local_base = now.with_timezone(&tz).date_naive() + chrono::Duration::days(day_offset);
        let wd = iso_weekday_u8(local_base.weekday());
        let mut day_slots: Vec<u16> = schedule
            .slots
            .iter()
            .filter(|s| s.weekday == wd)
            .map(|s| s.minute)
            .collect();
        day_slots.sort_unstable();
        for minute in day_slots {
            let (h, m) = (minute / 60, minute % 60);
            let Some(naive_t) = NaiveTime::from_hms_opt(h as u32, m as u32, 0) else {
                continue;
            };
            let naive_dt = local_base.and_time(naive_t);
            let Some(local_dt) = tz.from_local_datetime(&naive_dt).single() else {
                continue;
            };
            let utc = local_dt.with_timezone(&Utc);
            if utc < earliest {
                continue;
            }
            let occupied = taken_times
                .iter()
                .any(|t| (*t - utc).num_minutes().abs() < 30);
            if !occupied {
                return Ok(Some(utc));
            }
        }
    }
    Ok(None)
}

fn iso_weekday_u8(wd: Weekday) -> u8 {
    match wd {
        Weekday::Mon => 0,
        Weekday::Tue => 1,
        Weekday::Wed => 2,
        Weekday::Thu => 3,
        Weekday::Fri => 4,
        Weekday::Sat => 5,
        Weekday::Sun => 6,
    }
}

/// Upcoming slot instants for the next `days` local days.
pub fn upcoming_slots(
    slots: &[Slot],
    timezone: &str,
    now: DateTime<Utc>,
    days: i64,
) -> anyhow::Result<Vec<DateTime<Utc>>> {
    let tz: chrono_tz::Tz = timezone
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid timezone {timezone}"))?;
    let mut out = Vec::new();
    for day_offset in 0..days {
        let local_base = now.with_timezone(&tz).date_naive() + chrono::Duration::days(day_offset);
        let wd = iso_weekday_u8(local_base.weekday());
        for s in slots.iter().filter(|s| s.weekday == wd) {
            let (h, m) = s.hour_minute();
            let Some(naive_t) = NaiveTime::from_hms_opt(h, m, 0) else {
                continue;
            };
            let naive_dt = local_base.and_time(naive_t);
            let Some(local_dt) = tz.from_local_datetime(&naive_dt).single() else {
                continue;
            };
            let utc = local_dt.with_timezone(&Utc);
            if utc >= now {
                out.push(utc);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Structured agent action for adjusting post timing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdjustPostTiming {
    pub action: AdjustAction,
    #[serde(default)]
    pub slots: Vec<Slot>,
    #[serde(default)]
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdjustAction {
    Keep,
    Update,
}

/// Apply an agent timing adjustment. Manual schedules are never overwritten.
pub async fn apply_adjustment(
    pool: &PgPool,
    project_id: Uuid,
    platform: &str,
    max_daily_posts: i32,
    adjustment: &AdjustPostTiming,
) -> anyhow::Result<ApplyResult> {
    let current = load_schedule(pool, project_id, platform).await?;
    if let Some(ref c) = current
        && c.source == ScheduleSource::Manual
    {
        return Ok(ApplyResult::LockedManual {
            suggestion: adjustment.clone(),
        });
    }

    match adjustment.action {
        AdjustAction::Keep => Ok(ApplyResult::Kept),
        AdjustAction::Update => {
            validate_slots(&adjustment.slots, max_daily_posts).map_err(|e| anyhow::anyhow!(e))?;
            let sched = upsert_schedule(
                pool,
                project_id,
                platform,
                &adjustment.slots,
                ScheduleSource::Agent,
                &adjustment.rationale,
            )
            .await?;
            Ok(ApplyResult::Updated(sched))
        }
    }
}

#[derive(Debug)]
pub enum ApplyResult {
    Kept,
    Updated(PostingSchedule),
    LockedManual { suggestion: AdjustPostTiming },
}

/// Meta reports `online_followers` hour keys in Pacific time.
pub const ONLINE_FOLLOWERS_SOURCE_TZ: &str = "America/Los_Angeles";

/// Re-key `(local_date, pacific_hour, value)` samples into the project's timezone hours and
/// average per hour. Pacific hours are mapped on the sample's date so DST is handled.
pub fn convert_online_followers(
    samples: &[(NaiveDate, u8, f64)],
    project_timezone: &str,
) -> Vec<(u8, f64)> {
    let target: chrono_tz::Tz = project_timezone.parse().unwrap_or(chrono_tz::UTC);
    let source: chrono_tz::Tz = ONLINE_FOLLOWERS_SOURCE_TZ.parse().expect("static timezone");
    let mut sums: [(f64, u32); 24] = [(0.0, 0); 24];
    for (date, hour, value) in samples {
        if *hour > 23 {
            continue;
        }
        let Some(naive) = date.and_hms_opt(*hour as u32, 0, 0) else {
            continue;
        };
        // Nonexistent/ambiguous local times (DST edges): take the earliest valid mapping.
        let Some(src_dt) = source.from_local_datetime(&naive).earliest() else {
            continue;
        };
        let h = src_dt.with_timezone(&target).hour() as usize;
        sums[h].0 += value;
        sums[h].1 += 1;
    }
    sums.iter()
        .enumerate()
        .filter(|(_, (_, n))| *n > 0)
        .map(|(h, (sum, n))| (h as u8, sum / f64::from(*n)))
        .collect()
}

/// Average online_followers by hour (0-23, **project timezone**) over the days since `since`.
///
/// Stored hour keys are Pacific time; they are converted using `project_timezone`.
pub async fn online_followers_by_hour(
    pool: &PgPool,
    project_id: Uuid,
    since: NaiveDate,
    project_timezone: &str,
) -> anyhow::Result<Vec<(u8, f64)>> {
    let rows: Vec<(NaiveDate, String, Option<f64>)> = sqlx::query_as(
        r#"
        select local_date, dim_value, value
          from account_metric_daily
         where project_id = $1
           and platform = 'instagram'
           and metric = 'online_followers'
           and dim_key = 'hour'
           and local_date >= $2
           and value is not null
        "#,
    )
    .bind(project_id)
    .bind(since)
    .fetch_all(pool)
    .await?;

    let samples: Vec<(NaiveDate, u8, f64)> = rows
        .into_iter()
        .filter_map(|(d, hour_s, v)| Some((d, hour_s.parse::<u8>().ok()?, v?)))
        .collect();
    Ok(convert_online_followers(&samples, project_timezone))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_ok_slots() {
        let slots = vec![
            Slot::new(0, 11 * 60).unwrap(),
            Slot::new(0, 14 * 60).unwrap(),
            Slot::new(1, 12 * 60).unwrap(),
        ];
        assert!(validate_slots(&slots, 4).is_ok());
    }

    #[test]
    fn rejects_too_close() {
        let slots = vec![
            Slot::new(0, 11 * 60).unwrap(),
            Slot::new(0, 12 * 60).unwrap(),
        ];
        assert!(validate_slots(&slots, 4).is_err());
    }

    #[test]
    fn rejects_over_max_daily() {
        let slots = vec![
            Slot::new(0, 9 * 60).unwrap(),
            Slot::new(0, 12 * 60).unwrap(),
            Slot::new(0, 15 * 60).unwrap(),
        ];
        assert!(validate_slots(&slots, 2).is_err());
    }

    #[test]
    fn iso_weekday_monday_zero() {
        assert_eq!(iso_weekday_u8(Weekday::Mon), 0);
        assert_eq!(iso_weekday_u8(Weekday::Sun), 6);
    }

    #[test]
    fn online_followers_pacific_to_project_tz() {
        // 2026-10-15 is PDT (UTC-7). Pacific 09:00 = 12:00 New York (EDT, UTC-4) = 16:00 UTC.
        let d = NaiveDate::from_ymd_opt(2026, 10, 15).unwrap();
        let ny = convert_online_followers(&[(d, 9, 10.0), (d, 10, 20.0)], "America/New_York");
        assert_eq!(ny, vec![(12, 10.0), (13, 20.0)]);
        let utc = convert_online_followers(&[(d, 9, 10.0)], "UTC");
        assert_eq!(utc, vec![(16, 10.0)]);
        // Same project tz as source is the identity.
        let la = convert_online_followers(&[(d, 9, 10.0)], "America/Los_Angeles");
        assert_eq!(la, vec![(9, 10.0)]);
        // Averages samples landing in the same target hour.
        let d2 = NaiveDate::from_ymd_opt(2026, 10, 16).unwrap();
        let avg = convert_online_followers(&[(d, 9, 10.0), (d2, 9, 30.0)], "UTC");
        assert_eq!(avg, vec![(16, 20.0)]);
    }
}
