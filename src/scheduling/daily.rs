//! Daily plan posting: prime window + dayparts, assign_times.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc, Weekday};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::domain::Daypart;

/// A prime posting window for one weekday.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct PrimeWindow {
    /// 0 = Monday … 6 = Sunday.
    pub weekday: u8,
    pub start_minute: u16,
    pub end_minute: u16,
}

impl PrimeWindow {
    pub fn new(weekday: u8, start_minute: u16, end_minute: u16) -> Option<Self> {
        if weekday > 6 || start_minute >= 1440 || end_minute >= 1440 || start_minute >= end_minute {
            return None;
        }
        Some(Self {
            weekday,
            start_minute,
            end_minute,
        })
    }
}

/// Named daypart windows (minutes since midnight).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct DaypartWindows {
    pub morning: MinuteRange,
    pub afternoon: MinuteRange,
    pub evening: MinuteRange,
    pub night: MinuteRange,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct MinuteRange {
    pub start_minute: u16,
    pub end_minute: u16,
}

impl Default for DaypartWindows {
    fn default() -> Self {
        Self {
            morning: MinuteRange {
                start_minute: 420, // 07:00
                end_minute: 600,   // 10:00
            },
            afternoon: MinuteRange {
                start_minute: 720, // 12:00
                end_minute: 900,   // 15:00
            },
            evening: MinuteRange {
                start_minute: 1020, // 17:00
                end_minute: 1200,   // 20:00
            },
            night: MinuteRange {
                start_minute: 1230, // 20:30
                end_minute: 1380,   // 23:00
            },
        }
    }
}

impl DaypartWindows {
    pub fn for_daypart(&self, dp: Daypart) -> Option<MinuteRange> {
        match dp {
            Daypart::Morning => Some(self.morning),
            Daypart::Afternoon => Some(self.afternoon),
            Daypart::Evening => Some(self.evening),
            Daypart::Night => Some(self.night),
            Daypart::Prime => None,
        }
    }
}

/// One item to schedule.
#[derive(Debug, Clone)]
pub struct SchedulableItem {
    pub id: Uuid,
    pub track: String,
    pub rank: i32,
    pub daypart: Daypart,
}

#[derive(Debug, Clone)]
pub struct AssignedTime {
    pub item_id: Uuid,
    pub scheduled_for: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct AssignResult {
    pub assigned: Vec<AssignedTime>,
    pub missed: Vec<Uuid>,
}

/// Build prime windows from legacy slots: earliest slot per weekday ±30 minutes.
pub fn prime_windows_from_slots(slots: &[crate::scheduling::Slot]) -> Vec<PrimeWindow> {
    use std::collections::BTreeMap;
    let mut earliest: BTreeMap<u8, u16> = BTreeMap::new();
    for s in slots {
        earliest
            .entry(s.weekday)
            .and_modify(|m| *m = (*m).min(s.minute))
            .or_insert(s.minute);
    }
    let mut out = Vec::new();
    for (weekday, minute) in earliest {
        let start = minute.saturating_sub(30);
        let end = minute.saturating_add(30).min(1439);
        if let Some(w) = PrimeWindow::new(weekday, start, end) {
            out.push(w);
        }
    }
    out
}

/// Validate prime windows: weekday 0-6, start < end, minutes in range.
pub fn validate_prime_windows(windows: &[PrimeWindow]) -> Result<(), String> {
    if windows.is_empty() {
        return Err("at least one prime window required".into());
    }
    let mut seen = std::collections::HashSet::new();
    for w in windows {
        if w.weekday > 6 {
            return Err(format!("weekday out of range: {}", w.weekday));
        }
        if w.start_minute >= 1440 || w.end_minute >= 1440 {
            return Err("minute out of range".into());
        }
        if w.start_minute >= w.end_minute {
            return Err(format!(
                "weekday {}: start_minute must be < end_minute",
                w.weekday
            ));
        }
        if !seen.insert(w.weekday) {
            return Err(format!("duplicate weekday {}", w.weekday));
        }
    }
    Ok(())
}

pub fn validate_dayparts(dp: &DaypartWindows) -> Result<(), String> {
    for (name, r) in [
        ("morning", dp.morning),
        ("afternoon", dp.afternoon),
        ("evening", dp.evening),
        ("night", dp.night),
    ] {
        if r.start_minute >= 1440 || r.end_minute >= 1440 {
            return Err(format!("{name}: minute out of range"));
        }
        if r.start_minute >= r.end_minute {
            return Err(format!("{name}: start must be < end"));
        }
    }
    Ok(())
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

fn local_to_utc(tz: &chrono_tz::Tz, date: NaiveDate, minute: u16) -> Option<DateTime<Utc>> {
    let (h, m) = (minute / 60, minute % 60);
    let naive_t = NaiveTime::from_hms_opt(h as u32, m as u32, 0)?;
    let naive_dt = date.and_time(naive_t);
    let local_dt = tz.from_local_datetime(&naive_dt).single()?;
    Some(local_dt.with_timezone(&Utc))
}

fn peak_minute_in_range(start: u16, end: u16, online_followers: &[(u8, f64)]) -> u16 {
    if online_followers.is_empty() {
        return start + (end - start) / 3;
    }
    let start_h = (start / 60) as u8;
    let end_h = ((end.saturating_sub(1)) / 60) as u8;
    let mut best_h = start_h;
    let mut best_v = f64::NEG_INFINITY;
    for (h, v) in online_followers {
        if *h >= start_h && *h <= end_h && *v > best_v {
            best_v = *v;
            best_h = *h;
        }
    }
    let candidate = (best_h as u16) * 60 + 15;
    candidate.clamp(start, end.saturating_sub(1))
}

/// Assign posting times for plan items on `for_date`.
///
/// - Rank-1 items (both tracks) go in the prime window.
/// - Ranks 2-5 go in their assigned dayparts (2 items per daypart across tracks).
/// - Within a window, prefer online_followers peak; keep `min_gap` between any posts.
/// - If a window has already passed relative to `now`, try later windows that day; else mark missed.
pub fn assign_times(
    items: &[SchedulableItem],
    for_date: NaiveDate,
    prime_windows: &[PrimeWindow],
    dayparts: &DaypartWindows,
    online_followers: &[(u8, f64)],
    occupied: &[DateTime<Utc>],
    timezone: &str,
    now: DateTime<Utc>,
    min_gap: Duration,
) -> Result<AssignResult, String> {
    let tz: chrono_tz::Tz = timezone
        .parse()
        .map_err(|_| format!("invalid timezone {timezone}"))?;

    let wd = iso_weekday_u8(for_date.weekday());
    let prime = prime_windows
        .iter()
        .find(|w| w.weekday == wd)
        .copied()
        .or_else(|| {
            // Fallback: use first available prime window's minutes on this weekday
            prime_windows.first().map(|w| PrimeWindow {
                weekday: wd,
                start_minute: w.start_minute,
                end_minute: w.end_minute,
            })
        });

    let mut taken: Vec<DateTime<Utc>> = occupied.to_vec();
    taken.sort();
    let mut assigned = Vec::new();
    let mut missed = Vec::new();

    // Group items by their target window key
    #[derive(Clone, Copy, PartialEq, Eq, Hash)]
    enum WinKey {
        Prime,
        Daypart(Daypart),
    }

    let mut groups: std::collections::HashMap<WinKey, Vec<&SchedulableItem>> =
        std::collections::HashMap::new();
    for item in items {
        let key = if item.daypart == Daypart::Prime || item.rank == 1 {
            WinKey::Prime
        } else {
            WinKey::Daypart(item.daypart)
        };
        groups.entry(key).or_default().push(item);
    }

    // Process windows in chronological order
    let mut window_order: Vec<(WinKey, u16, u16)> = Vec::new();
    if let Some(p) = prime {
        window_order.push((WinKey::Prime, p.start_minute, p.end_minute));
    }
    for dp in Daypart::non_prime() {
        if let Some(r) = dayparts.for_daypart(dp) {
            window_order.push((WinKey::Daypart(dp), r.start_minute, r.end_minute));
        }
    }
    window_order.sort_by_key(|(_, s, _)| *s);

    for (key, start, end) in &window_order {
        let Some(group) = groups.get(key) else {
            continue;
        };
        if group.is_empty() {
            continue;
        }

        // Preferred peak, then space items within window
        let peak = peak_minute_in_range(*start, *end, online_followers);
        let n = group.len();
        let gap_mins = ((*end - *start) / (n as u16 + 1)).max(min_gap.num_minutes() as u16);

        for (i, item) in group.iter().enumerate() {
            let mut candidates: Vec<u16> = Vec::new();
            // Try peak first for first item, then spaced
            if i == 0 {
                candidates.push(peak);
            }
            let base = *start + gap_mins * ((i as u16) + 1);
            candidates.push(base.clamp(*start, end.saturating_sub(1)));
            // Fill more candidates across the window
            let mut m = *start;
            while m < *end {
                candidates.push(m);
                m = m.saturating_add(15);
            }

            let mut placed = None;
            for minute in candidates {
                let Some(utc) = local_to_utc(&tz, for_date, minute) else {
                    continue;
                };
                if utc < now + Duration::minutes(5) {
                    continue;
                }
                let conflict = taken
                    .iter()
                    .any(|t| (*t - utc).num_minutes().abs() < min_gap.num_minutes());
                if !conflict {
                    placed = Some(utc);
                    break;
                }
            }

            // If this window failed, try later windows that day
            if placed.is_none() {
                for (later_key, ls, le) in window_order.iter().filter(|(_, s, _)| *s > *end) {
                    if *later_key == *key {
                        continue;
                    }
                    let mut m = *ls;
                    while m < *le {
                        if let Some(utc) = local_to_utc(&tz, for_date, m)
                            && utc >= now + Duration::minutes(5)
                        {
                            let conflict = taken
                                .iter()
                                .any(|t| (*t - utc).num_minutes().abs() < min_gap.num_minutes());
                            if !conflict {
                                placed = Some(utc);
                                break;
                            }
                        }
                        m = m.saturating_add(15);
                    }
                    if placed.is_some() {
                        break;
                    }
                }
            }

            match placed {
                Some(utc) => {
                    taken.push(utc);
                    taken.sort();
                    assigned.push(AssignedTime {
                        item_id: item.id,
                        scheduled_for: utc,
                    });
                }
                None => missed.push(item.id),
            }
        }
    }

    Ok(AssignResult { assigned, missed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn items_two_tracks() -> Vec<SchedulableItem> {
        let mut out = Vec::new();
        // audience
        out.push(SchedulableItem {
            id: Uuid::from_u128(1),
            track: "audience".into(),
            rank: 1,
            daypart: Daypart::Prime,
        });
        out.push(SchedulableItem {
            id: Uuid::from_u128(2),
            track: "audience".into(),
            rank: 2,
            daypart: Daypart::Morning,
        });
        out.push(SchedulableItem {
            id: Uuid::from_u128(3),
            track: "audience".into(),
            rank: 3,
            daypart: Daypart::Afternoon,
        });
        out.push(SchedulableItem {
            id: Uuid::from_u128(4),
            track: "audience".into(),
            rank: 4,
            daypart: Daypart::Evening,
        });
        out.push(SchedulableItem {
            id: Uuid::from_u128(5),
            track: "audience".into(),
            rank: 5,
            daypart: Daypart::Night,
        });
        // trend
        out.push(SchedulableItem {
            id: Uuid::from_u128(11),
            track: "trend".into(),
            rank: 1,
            daypart: Daypart::Prime,
        });
        out.push(SchedulableItem {
            id: Uuid::from_u128(12),
            track: "trend".into(),
            rank: 2,
            daypart: Daypart::Morning,
        });
        out.push(SchedulableItem {
            id: Uuid::from_u128(13),
            track: "trend".into(),
            rank: 3,
            daypart: Daypart::Afternoon,
        });
        out.push(SchedulableItem {
            id: Uuid::from_u128(14),
            track: "trend".into(),
            rank: 4,
            daypart: Daypart::Evening,
        });
        out.push(SchedulableItem {
            id: Uuid::from_u128(15),
            track: "trend".into(),
            rank: 5,
            daypart: Daypart::Night,
        });
        out
    }

    #[test]
    fn assign_ten_posts_with_gap() {
        let date = NaiveDate::from_ymd_opt(2026, 10, 12).unwrap(); // Monday
        let prime = vec![PrimeWindow::new(0, 10 * 60, 12 * 60).unwrap()];
        let dayparts = DaypartWindows::default();
        let now = Utc.with_ymd_and_hms(2026, 10, 11, 12, 0, 0).unwrap();
        let followers = vec![(10, 100.0), (11, 200.0), (14, 150.0)];
        let result = assign_times(
            &items_two_tracks(),
            date,
            &prime,
            &dayparts,
            &followers,
            &[],
            "UTC",
            now,
            Duration::minutes(30),
        )
        .unwrap();
        assert_eq!(result.missed.len(), 0);
        assert_eq!(result.assigned.len(), 10);
        // Check min gap
        let mut times: Vec<_> = result.assigned.iter().map(|a| a.scheduled_for).collect();
        times.sort();
        for w in times.windows(2) {
            assert!(
                (w[1] - w[0]).num_minutes() >= 30,
                "gap too small: {:?} {:?}",
                w[0],
                w[1]
            );
        }
    }

    #[test]
    fn passed_windows_missed_or_moved() {
        let date = NaiveDate::from_ymd_opt(2026, 10, 12).unwrap();
        let prime = vec![PrimeWindow::new(0, 10 * 60, 11 * 60).unwrap()];
        let dayparts = DaypartWindows::default();
        // Confirm late at night — morning/afternoon/evening/prime all passed
        let now = Utc.with_ymd_and_hms(2026, 10, 12, 22, 0, 0).unwrap();
        let result = assign_times(
            &items_two_tracks(),
            date,
            &prime,
            &dayparts,
            &[],
            &[],
            "UTC",
            now,
            Duration::minutes(30),
        )
        .unwrap();
        // Night window (20:30-23:00) may still fit some; rest missed
        assert!(!result.missed.is_empty() || result.assigned.len() < 10);
    }

    #[test]
    fn validate_prime_ok() {
        assert!(validate_prime_windows(&[PrimeWindow::new(0, 600, 720).unwrap()]).is_ok());
        // start >= end is rejected at construction
        assert!(PrimeWindow::new(0, 720, 600).is_none());
        assert!(validate_prime_windows(&[]).is_err());
    }

    #[test]
    fn prime_windows_from_slots_earliest_per_day() {
        use crate::scheduling::Slot;
        let slots = vec![
            Slot::new(0, 660).unwrap(),
            Slot::new(0, 900).unwrap(),
            Slot::new(2, 720).unwrap(),
        ];
        let windows = prime_windows_from_slots(&slots);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].weekday, 0);
        assert_eq!(windows[0].start_minute, 630);
        assert_eq!(windows[0].end_minute, 690);
        assert_eq!(windows[1].weekday, 2);
        assert_eq!(windows[1].start_minute, 690);
        assert_eq!(windows[1].end_minute, 750);
    }
}
