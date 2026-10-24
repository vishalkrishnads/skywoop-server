//! Experiment maturity evaluation.

use chrono::{DateTime, Days, Duration, NaiveDate, Utc};

use crate::domain::Maturity;

/// Inputs for the pure maturity decision.
#[derive(Debug, Clone)]
pub struct MaturityFacts {
    pub published_at: DateTime<Utc>,
    pub publish_local_date: NaiveDate,
    pub last_snapshot_at: Option<DateTime<Utc>>,
    pub views: Option<i64>,
    pub asc_configured: bool,
    /// Max App Store data date ingested for this app (instance processing_date).
    pub asc_max_ingested_date: Option<NaiveDate>,
}

/// Pure maturity evaluation (B16).
///
/// Returns `(maturity, meta_complete, asc_complete)`.
pub fn evaluate(f: &MaturityFacts, now: DateTime<Utc>, min_views: i64) -> (Maturity, bool, bool) {
    let meta_complete = f
        .last_snapshot_at
        .is_some_and(|t| t - f.published_at >= Duration::hours(72));
    let asc_complete = !f.asc_configured
        || f.asc_max_ingested_date
            .is_some_and(|d| d >= f.publish_local_date + Days::new(3));
    let window_closed = now - f.published_at >= Duration::days(7);
    let enough = f.views.unwrap_or(0) >= min_views;
    let m = if ((meta_complete && asc_complete) || window_closed) && enough {
        Maturity::Mature
    } else if window_closed {
        Maturity::Inconclusive
    } else {
        Maturity::Collecting
    };
    (m, meta_complete, asc_complete)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn facts() -> MaturityFacts {
        MaturityFacts {
            published_at: Utc.with_ymd_and_hms(2024, 1, 1, 12, 0, 0).unwrap(),
            publish_local_date: NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
            last_snapshot_at: None,
            views: Some(200),
            asc_configured: false,
            asc_max_ingested_date: None,
        }
    }

    #[test]
    fn stays_collecting_before_window() {
        let f = facts();
        let now = f.published_at + Duration::days(2);
        let (m, meta, asc) = evaluate(&f, now, 100);
        assert_eq!(m, Maturity::Collecting);
        assert!(!meta);
        assert!(asc); // not configured → complete
    }

    #[test]
    fn mature_when_meta_ready_and_enough_views() {
        let mut f = facts();
        f.last_snapshot_at = Some(f.published_at + Duration::hours(72));
        let now = f.published_at + Duration::hours(73);
        let (m, meta, asc) = evaluate(&f, now, 100);
        assert_eq!(m, Maturity::Mature);
        assert!(meta);
        assert!(asc);
    }

    #[test]
    fn inconclusive_when_window_closed_but_low_views() {
        let mut f = facts();
        f.views = Some(10);
        let now = f.published_at + Duration::days(8);
        let (m, _, _) = evaluate(&f, now, 100);
        assert_eq!(m, Maturity::Inconclusive);
    }

    #[test]
    fn asc_required_when_configured() {
        let mut f = facts();
        f.asc_configured = true;
        f.last_snapshot_at = Some(f.published_at + Duration::hours(72));
        let now = f.published_at + Duration::hours(80);
        let (m, meta, asc) = evaluate(&f, now, 100);
        assert_eq!(m, Maturity::Collecting);
        assert!(meta);
        assert!(!asc);

        f.asc_max_ingested_date = Some(f.publish_local_date + Days::new(3));
        let (m2, _, asc2) = evaluate(&f, now, 100);
        assert_eq!(m2, Maturity::Mature);
        assert!(asc2);
    }
}
