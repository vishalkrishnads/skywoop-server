//! Industry posting-time priors from docs/times.md.

use crate::scheduling::Slot;
use crate::scheduling::daily::PrimeWindow;

/// The nine industries listed in docs/times.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Industry {
    CivicNonProfitMembership,
    ComputerSoftwareTech,
    FinancialServices,
    FoodBeverage,
    Government,
    HospitalsHealthcare,
    Retail,
    SchoolsEducation,
    TravelHospitality,
}

impl Industry {
    pub const ALL: &[Industry] = &[
        Self::CivicNonProfitMembership,
        Self::ComputerSoftwareTech,
        Self::FinancialServices,
        Self::FoodBeverage,
        Self::Government,
        Self::HospitalsHealthcare,
        Self::Retail,
        Self::SchoolsEducation,
        Self::TravelHospitality,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CivicNonProfitMembership => "civic_nonprofit_membership",
            Self::ComputerSoftwareTech => "computer_software_tech",
            Self::FinancialServices => "financial_services",
            Self::FoodBeverage => "food_beverage",
            Self::Government => "government",
            Self::HospitalsHealthcare => "hospitals_healthcare",
            Self::Retail => "retail",
            Self::SchoolsEducation => "schools_education",
            Self::TravelHospitality => "travel_hospitality",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::CivicNonProfitMembership => "Civic, Non-Profit & Membership",
            Self::ComputerSoftwareTech => "Computer Software & Tech",
            Self::FinancialServices => "Financial Services",
            Self::FoodBeverage => "Food & Beverage",
            Self::Government => "Government",
            Self::HospitalsHealthcare => "Hospitals & Healthcare",
            Self::Retail => "Retail",
            Self::SchoolsEducation => "Schools & Education",
            Self::TravelHospitality => "Travel & Hospitality",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let key = s
            .trim()
            .to_ascii_lowercase()
            .replace([' ', '-', '&', ','], "_");
        let key = key.replace("__", "_").replace("__", "_");
        for ind in Self::ALL {
            if ind.as_str() == key || ind.display_name().eq_ignore_ascii_case(s.trim()) {
                return Some(*ind);
            }
        }
        // Fuzzy aliases
        match key.as_str() {
            "civic" | "nonprofit" | "non_profit" | "membership" => {
                Some(Self::CivicNonProfitMembership)
            }
            "tech" | "software" | "computer_software" | "saas" => Some(Self::ComputerSoftwareTech),
            "finance" | "financial" | "fintech" => Some(Self::FinancialServices),
            "food" | "beverage" | "restaurant" => Some(Self::FoodBeverage),
            "gov" | "government" => Some(Self::Government),
            "health" | "healthcare" | "hospital" | "hospitals" => Some(Self::HospitalsHealthcare),
            "retail" | "ecommerce" | "e_commerce" => Some(Self::Retail),
            "education" | "school" | "schools" | "edtech" => Some(Self::SchoolsEducation),
            "travel" | "hospitality" | "tourism" => Some(Self::TravelHospitality),
            _ => None,
        }
    }

    /// Allowed weekdays (0=Mon … 6=Sun). Worst days are excluded.
    pub fn allowed_weekdays(self) -> &'static [u8] {
        match self {
            Self::CivicNonProfitMembership
            | Self::ComputerSoftwareTech
            | Self::FoodBeverage
            | Self::HospitalsHealthcare
            | Self::Retail => &[0, 1, 2, 3, 4], // weekdays
            Self::FinancialServices | Self::Government | Self::TravelHospitality => &[1, 2, 3], // Tue-Thu
            Self::SchoolsEducation => &[0, 1, 2, 3, 5], // Mon-Thu + Sat
        }
    }

    /// Per-weekday local-time windows as (start_minute, end_minute) inclusive ranges.
    /// Multiple windows per day are supported.
    pub fn windows(self) -> &'static [(u8, u16, u16)] {
        // (weekday, start_minute, end_minute)
        match self {
            Self::CivicNonProfitMembership => &[
                (0, 13 * 60, 13 * 60), // Mon 1pm
                (1, 11 * 60, 16 * 60), // Tue 11-4
                (2, 11 * 60, 17 * 60), // Wed 11-5
                (3, 10 * 60, 16 * 60), // Thu 10-4
                (4, 11 * 60, 12 * 60), // Fri 11-12
                (4, 15 * 60, 15 * 60), // Fri 3pm
            ],
            Self::ComputerSoftwareTech => &[
                (0, 11 * 60, 13 * 60),
                (0, 15 * 60, 15 * 60),
                (1, 11 * 60, 16 * 60),
                (2, 11 * 60, 15 * 60),
                (3, 11 * 60, 15 * 60),
                (4, 11 * 60, 14 * 60),
            ],
            Self::FinancialServices | Self::TravelHospitality => &[
                (1, 3 * 60, 3 * 60),
                (1, 6 * 60, 6 * 60),
                (1, 9 * 60, 11 * 60),
                (1, 14 * 60, 14 * 60),
                (2, 3 * 60, 10 * 60),
                (2, 15 * 60, 16 * 60),
                (2, 21 * 60, 21 * 60),
                (2, 23 * 60, 23 * 60),
                (3, 2 * 60, 6 * 60),
                (3, 19 * 60, 19 * 60),
            ],
            Self::FoodBeverage => &[
                (0, 11 * 60, 18 * 60),
                (1, 11 * 60, 16 * 60),
                (2, 11 * 60, 14 * 60),
                (3, 11 * 60, 13 * 60),
                (4, 11 * 60, 13 * 60),
            ],
            Self::Government => &[
                (1, 13 * 60, 18 * 60),
                (2, 10 * 60, 19 * 60),
                (3, 10 * 60, 17 * 60),
            ],
            Self::HospitalsHealthcare => &[
                (0, 12 * 60, 21 * 60),
                (1, 11 * 60, 13 * 60),
                (2, 11 * 60, 17 * 60),
                (3, 11 * 60, 13 * 60),
                (4, 11 * 60, 11 * 60),
            ],
            Self::Retail => &[
                (0, 12 * 60, 15 * 60),
                (1, 12 * 60, 16 * 60),
                (2, 12 * 60, 15 * 60),
                (3, 11 * 60, 17 * 60),
                (4, 12 * 60, 15 * 60),
            ],
            Self::SchoolsEducation => &[
                (0, 16 * 60, 16 * 60),
                (1, 15 * 60, 17 * 60),
                (1, 19 * 60, 21 * 60),
                (2, 11 * 60, 19 * 60),
                (2, 21 * 60, 21 * 60),
                (3, 15 * 60, 15 * 60),
                (5, 15 * 60, 20 * 60),
            ],
        }
    }
}

/// Spread up to `max_daily` slots across each allowed day's windows.
pub fn seed_slots_for_industry(industry: Industry, max_daily: i32) -> Vec<Slot> {
    let max_daily = max_daily.max(1) as usize;
    let mut slots = Vec::new();
    for &day in industry.allowed_weekdays() {
        let windows: Vec<(u16, u16)> = industry
            .windows()
            .iter()
            .filter(|(d, _, _)| *d == day)
            .map(|(_, s, e)| (*s, *e))
            .collect();
        if windows.is_empty() {
            continue;
        }
        let day_slots = pick_in_windows(&windows, max_daily);
        for minute in day_slots {
            if let Some(s) = Slot::new(day, minute) {
                slots.push(s);
            }
        }
    }
    slots
}

/// One prime window per allowed weekday from the industry time table.
///
/// Uses the earliest start and latest end that day. Point priors (start == end)
/// expand by ±30 minutes so two rank-1 posts can share the window.
pub fn prime_windows_for_industry(industry: Industry) -> Vec<PrimeWindow> {
    let mut out = Vec::new();
    for &day in industry.allowed_weekdays() {
        let mut start = u16::MAX;
        let mut end = 0u16;
        let mut any = false;
        for &(d, s, e) in industry.windows() {
            if d != day {
                continue;
            }
            any = true;
            start = start.min(s);
            end = end.max(e.max(s));
        }
        if !any {
            continue;
        }
        if start == end {
            let expanded_start = start.saturating_sub(30);
            let expanded_end = end.saturating_add(30).min(1439);
            if let Some(w) = PrimeWindow::new(day, expanded_start, expanded_end) {
                out.push(w);
            }
        } else if let Some(w) = PrimeWindow::new(day, start, end) {
            out.push(w);
        }
    }
    out
}

fn pick_in_windows(windows: &[(u16, u16)], max: usize) -> Vec<u16> {
    // Expand windows into candidate minutes at 30-min resolution, then pick evenly.
    let mut candidates = Vec::new();
    for &(start, end) in windows {
        let end = end.max(start);
        let mut m = start;
        loop {
            candidates.push(m);
            if m >= end {
                break;
            }
            m = m.saturating_add(30);
            if m > end {
                candidates.push(end);
                break;
            }
        }
    }
    candidates.sort_unstable();
    candidates.dedup();
    if candidates.is_empty() {
        return vec![];
    }
    if candidates.len() <= max {
        return candidates;
    }
    // Evenly spaced picks, enforcing ≥ 2h gap when possible.
    let mut picked = Vec::new();
    let step = candidates.len() as f64 / max as f64;
    for i in 0..max {
        let idx = ((i as f64 + 0.5) * step) as usize;
        let idx = idx.min(candidates.len() - 1);
        let m = candidates[idx];
        if picked
            .last()
            .is_none_or(|prev: &u16| m.saturating_sub(*prev) >= 120)
        {
            picked.push(m);
        } else {
            // find next candidate ≥ 2h away
            if let Some(&next) = candidates
                .iter()
                .find(|c| c.saturating_sub(*picked.last().unwrap()) >= 120)
            {
                picked.push(next);
            }
        }
    }
    picked.sort_unstable();
    picked.dedup();
    picked
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn all_industries_have_windows() {
        for ind in Industry::ALL {
            assert!(!ind.windows().is_empty(), "{} has no windows", ind.as_str());
            let slots = seed_slots_for_industry(*ind, 2);
            assert!(!slots.is_empty(), "{} produced no seed slots", ind.as_str());
            let primes = prime_windows_for_industry(*ind);
            assert!(
                !primes.is_empty(),
                "{} produced no prime windows",
                ind.as_str()
            );
            for w in &primes {
                assert!(w.start_minute < w.end_minute);
            }
            // No weekend slots for industries that ban weekends
            if matches!(
                ind,
                Industry::CivicNonProfitMembership
                    | Industry::ComputerSoftwareTech
                    | Industry::FinancialServices
                    | Industry::FoodBeverage
                    | Industry::Government
                    | Industry::HospitalsHealthcare
                    | Industry::Retail
                    | Industry::TravelHospitality
            ) {
                assert!(
                    slots.iter().all(|s| s.weekday < 5),
                    "{} leaked a weekend slot",
                    ind.as_str()
                );
            }
        }
    }

    #[test]
    fn times_md_parity() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/times.md");
        let text = fs::read_to_string(&path).expect("docs/times.md readable");
        let expected = [
            (
                "Civic, Non-Profit & Membership",
                Industry::CivicNonProfitMembership,
            ),
            ("Computer Software & Tech", Industry::ComputerSoftwareTech),
            ("Financial Services", Industry::FinancialServices),
            ("Food & Beverage", Industry::FoodBeverage),
            ("Government", Industry::Government),
            ("Hospitals & Healthcare", Industry::HospitalsHealthcare),
            ("Retail", Industry::Retail),
            ("Schools & Education", Industry::SchoolsEducation),
            ("Travel & Hospitality", Industry::TravelHospitality),
        ];
        for (name, ind) in expected {
            assert!(
                text.contains(name),
                "docs/times.md missing industry row for {name}"
            );
            assert_eq!(Industry::parse(name), Some(ind));
            // Worst days: most say Weekends
            if ind != Industry::SchoolsEducation {
                assert!(
                    !ind.allowed_weekdays().contains(&5) && !ind.allowed_weekdays().contains(&6),
                    "{name} should ban weekends"
                );
            } else {
                assert!(
                    !ind.allowed_weekdays().contains(&6),
                    "Schools should ban Sundays"
                );
                assert!(
                    ind.allowed_weekdays().contains(&5),
                    "Schools should allow Saturdays"
                );
            }
        }
        // Spot-check a few windows against the table
        assert!(
            Industry::ComputerSoftwareTech
                .windows()
                .iter()
                .any(|(d, s, e)| *d == 0 && *s == 11 * 60 && *e == 13 * 60)
        );
        assert!(
            Industry::Retail
                .windows()
                .iter()
                .any(|(d, s, e)| *d == 3 && *s == 11 * 60 && *e == 17 * 60)
        );
    }

    #[test]
    fn parse_aliases() {
        assert_eq!(
            Industry::parse("tech"),
            Some(Industry::ComputerSoftwareTech)
        );
        assert_eq!(
            Industry::parse("fintech"),
            Some(Industry::FinancialServices)
        );
        assert_eq!(Industry::parse("retail"), Some(Industry::Retail));
    }
}
