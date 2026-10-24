//! Daily social trend snapshots (Google Trends RSS + web search via the planner model).

pub mod google;

use std::collections::HashSet;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::ai::{AiClient, planner};
use crate::domain::types::new_id;

pub use google::GoogleTrendItem;

const MAX_GOOGLE_ITEMS: usize = 10;
const MAX_ITEMS: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrendItem {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub source_urls: Vec<String>,
    /// "google" | "web"
    pub source: String,
}

impl From<GoogleTrendItem> for TrendItem {
    fn from(g: GoogleTrendItem) -> Self {
        let mut summary = String::from("Trending search");
        if let Some(t) = &g.traffic {
            summary.push_str(&format!(" ({t} searches)"));
        }
        if !g.news_titles.is_empty() {
            summary.push_str(": ");
            summary.push_str(&g.news_titles.join("; "));
        }
        Self {
            id: g.id,
            title: g.title,
            summary,
            source_urls: g.source_urls,
            source: "google".into(),
        }
    }
}

/// Calendar date in the project's timezone (falls back to UTC on unknown tz).
pub fn local_date_for_tz(timezone: &str, now: DateTime<Utc>) -> NaiveDate {
    match timezone.parse::<chrono_tz::Tz>() {
        Ok(tz) => now.with_timezone(&tz).date_naive(),
        Err(_) => now.date_naive(),
    }
}

/// Return today's trend items for a project/platform, fetching and caching if needed.
pub async fn ensure_trend_snapshot(
    db: &PgPool,
    ai: &AiClient,
    project_id: Uuid,
    platform: &str,
    local_date: NaiveDate,
    country: &str,
) -> anyhow::Result<Value> {
    let existing: Option<(String, Value)> = sqlx::query_as(
        "select status, items from trend_snapshots \
         where project_id = $1 and platform = $2 and local_date = $3",
    )
    .bind(project_id)
    .bind(platform)
    .bind(local_date)
    .fetch_optional(db)
    .await?;
    if let Some((status, items)) = existing
        && matches!(status.as_str(), "ready" | "partial")
        && items.as_array().is_some_and(|a| !a.is_empty())
    {
        return Ok(items);
    }

    let (google, web) = tokio::join!(
        google::fetch_daily(country),
        planner::social_trends_via_web(ai, country, platform),
    );

    let mut errors: Vec<String> = Vec::new();
    let mut items: Vec<TrendItem> = Vec::new();
    match google {
        Ok(g) => items.extend(g.into_iter().take(MAX_GOOGLE_ITEMS).map(TrendItem::from)),
        Err(e) => {
            tracing::warn!(error = %e, "google trends fetch failed");
            errors.push(format!("google: {e:#}"));
        }
    }
    match web {
        Ok(w) => items.extend(w.items.into_iter().map(|i| TrendItem {
            id: i.id,
            title: i.title,
            summary: i.summary,
            source_urls: i.source_urls,
            source: "web".into(),
        })),
        Err(e) => {
            tracing::warn!(error = %e, "web trends fetch failed");
            errors.push(format!("web: {e}"));
        }
    }

    let items = dedupe(items);
    let status = match (errors.len(), items.is_empty()) {
        (0, _) => "ready",
        (_, false) => "partial",
        _ => "failed",
    };
    let error = (!errors.is_empty()).then(|| errors.join("; "));
    let items_json = serde_json::to_value(&items)?;

    sqlx::query(
        "insert into trend_snapshots \
           (id, project_id, platform, local_date, country, status, items, error) \
         values ($1, $2, $3, $4, $5, $6, $7, $8) \
         on conflict (project_id, platform, local_date) do update set \
           country = excluded.country, status = excluded.status, \
           items = excluded.items, error = excluded.error, created_at = now()",
    )
    .bind(new_id())
    .bind(project_id)
    .bind(platform)
    .bind(local_date)
    .bind(country)
    .bind(status)
    .bind(&items_json)
    .bind(error)
    .execute(db)
    .await?;

    Ok(items_json)
}

/// Merge by lowercase title, keeping first occurrence (Google ranks first) and capping size.
fn dedupe(items: Vec<TrendItem>) -> Vec<TrendItem> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .filter(|i| !i.title.trim().is_empty() && seen.insert(i.title.trim().to_lowercase()))
        .take(MAX_ITEMS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str, source: &str) -> TrendItem {
        TrendItem {
            id: title.to_lowercase(),
            title: title.into(),
            summary: String::new(),
            source_urls: vec![],
            source: source.into(),
        }
    }

    #[test]
    fn dedupes_case_insensitively_keeping_first() {
        let out = dedupe(vec![
            item("Foo", "google"),
            item("foo ", "web"),
            item("Bar", "web"),
        ]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].source, "google");
    }
}
