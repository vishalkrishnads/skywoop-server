//! Catalog of Meta post and account metrics we request and store.

use serde_json::Value;

/// A metric name optionally requested with a breakdown.
#[derive(Debug, Clone, Copy)]
pub struct MetricSpec {
    pub name: &'static str,
    pub breakdown: Option<&'static str>,
}

/// A Graph field to read off the post/media object itself (not insights).
#[derive(Debug, Clone, Copy)]
pub struct FieldSpec {
    pub name: &'static str,
    /// JSON pointer relative to the object, e.g. "shares/count".
    pub path: &'static str,
    /// Normalized metric name stored in post_metric_values.
    pub store_as: &'static str,
}

/// Flattened metric row ready for insert into post_metric_values / account_metric_daily.
#[derive(Debug, Clone, PartialEq)]
pub struct FlatMetric {
    pub metric: String,
    pub dim_key: String,
    pub dim_value: String,
    pub value: f64,
}

impl FlatMetric {
    pub fn simple(metric: impl Into<String>, value: f64) -> Self {
        Self {
            metric: metric.into(),
            dim_key: String::new(),
            dim_value: String::new(),
            value,
        }
    }

    pub fn with_dim(
        metric: impl Into<String>,
        dim_key: impl Into<String>,
        dim_value: impl Into<String>,
        value: f64,
    ) -> Self {
        Self {
            metric: metric.into(),
            dim_key: dim_key.into(),
            dim_value: dim_value.into(),
            value,
        }
    }
}

/// Instagram Reels media insights metrics.
pub const IG_POST_METRICS: &[MetricSpec] = &[
    MetricSpec {
        name: "views",
        breakdown: None,
    },
    MetricSpec {
        name: "reach",
        breakdown: None,
    },
    MetricSpec {
        name: "likes",
        breakdown: None,
    },
    MetricSpec {
        name: "comments",
        breakdown: None,
    },
    MetricSpec {
        name: "saved",
        breakdown: None,
    },
    MetricSpec {
        name: "shares",
        breakdown: None,
    },
    MetricSpec {
        name: "reposts",
        breakdown: None,
    },
    MetricSpec {
        name: "total_interactions",
        breakdown: None,
    },
    MetricSpec {
        name: "profile_visits",
        breakdown: None,
    },
    MetricSpec {
        name: "follows",
        breakdown: None,
    },
    MetricSpec {
        name: "total_likes",
        breakdown: None,
    },
    MetricSpec {
        name: "total_comments",
        breakdown: None,
    },
    MetricSpec {
        name: "total_views",
        breakdown: None,
    },
    MetricSpec {
        name: "profile_activity",
        breakdown: Some("action_type"),
    },
    // Reels video metrics.
    MetricSpec {
        name: "ig_reels_avg_watch_time",
        breakdown: None,
    },
    MetricSpec {
        name: "ig_reels_video_view_total_time",
        breakdown: None,
    },
    MetricSpec {
        name: "ig_reels_aggregated_all_plays_count",
        breakdown: None,
    },
    MetricSpec {
        name: "reels_skip_rate",
        breakdown: None,
    },
];

/// Instagram media object fields (engagement counts).
pub const IG_POST_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "like_count",
        path: "like_count",
        store_as: "field_like_count",
    },
    FieldSpec {
        name: "comments_count",
        path: "comments_count",
        store_as: "field_comments_count",
    },
];

/// Facebook Page post insights (lifetime). Avoid deprecated post_impressions*.
pub const FB_POST_METRICS: &[MetricSpec] = &[
    MetricSpec {
        name: "post_media_view",
        breakdown: None,
    },
    MetricSpec {
        name: "post_total_media_view_unique",
        breakdown: None,
    },
    MetricSpec {
        name: "post_clicks",
        breakdown: None,
    },
    MetricSpec {
        name: "post_clicks_by_type",
        breakdown: None,
    },
    MetricSpec {
        name: "post_reactions_by_type_total",
        breakdown: None,
    },
    MetricSpec {
        name: "post_reactions_like_total",
        breakdown: None,
    },
    MetricSpec {
        name: "post_reactions_love_total",
        breakdown: None,
    },
    MetricSpec {
        name: "post_reactions_wow_total",
        breakdown: None,
    },
    MetricSpec {
        name: "post_reactions_haha_total",
        breakdown: None,
    },
    MetricSpec {
        name: "post_reactions_sorry_total",
        breakdown: None,
    },
    MetricSpec {
        name: "post_reactions_anger_total",
        breakdown: None,
    },
];

/// Facebook Reel (video object) insights, read from `{video_id}/video_insights`.
pub const FB_VIDEO_METRICS: &[MetricSpec] = &[
    MetricSpec {
        name: "post_video_avg_time_watched",
        breakdown: None,
    },
    MetricSpec {
        name: "post_video_view_time",
        breakdown: None,
    },
    MetricSpec {
        name: "fb_reels_total_plays",
        breakdown: None,
    },
    MetricSpec {
        name: "fb_reels_replay_count",
        breakdown: None,
    },
];

/// Facebook post object fields.
pub const FB_POST_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "shares",
        path: "shares/count",
        store_as: "field_shares",
    },
    FieldSpec {
        name: "reactions.summary(true)",
        path: "reactions/summary/total_count",
        store_as: "field_reactions",
    },
    FieldSpec {
        name: "comments.summary(true)",
        path: "comments/summary/total_count",
        store_as: "field_comments",
    },
];

/// Instagram account insights (day period, total_value) excluding online_followers.
pub const IG_ACCOUNT_DAY_METRICS: &[&str] = &[
    "views",
    "reach",
    "accounts_engaged",
    "total_interactions",
    "likes",
    "comments",
    "saves",
    "shares",
    "replies",
    "reposts",
    "follows_and_unfollows",
    "profile_links_taps",
    "follower_count",
];

/// Instagram online_followers (lifetime / last 30 days).
pub const IG_ONLINE_FOLLOWERS: &str = "online_followers";

/// Facebook Page day metrics (avoid deprecated page_impressions* / page_fans_online*).
pub const FB_ACCOUNT_DAY_METRICS: &[&str] = &[
    "page_media_view",
    "page_total_media_view_unique",
    "page_follows",
    "page_daily_follows_unique",
    "page_post_engagements",
    "page_views_total",
    "page_actions_post_reactions_total",
];

/// Join metric names that share the same breakdown (or none) into a comma list.
pub fn batch_names(specs: &[MetricSpec], breakdown: Option<&str>) -> String {
    specs
        .iter()
        .filter(|s| s.breakdown == breakdown)
        .map(|s| s.name)
        .collect::<Vec<_>>()
        .join(",")
}

/// Distinct breakdown keys present in the catalog (None first).
pub fn breakdown_groups(specs: &[MetricSpec]) -> Vec<Option<&'static str>> {
    let mut out = vec![None];
    for s in specs {
        if let Some(b) = s.breakdown
            && !out.contains(&Some(b))
        {
            out.push(Some(b));
        }
    }
    out
}

/// Flatten an insights `/insights` payload into metric rows.
/// Handles numeric values, total_value, object-valued maps, and breakdown results.
pub fn flatten_insights(payload: &Value) -> Vec<FlatMetric> {
    let mut out = Vec::new();
    let Some(data) = payload.get("data").and_then(|d| d.as_array()) else {
        return out;
    };
    for item in data {
        let Some(name) = item.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        flatten_metric_item(name, item, &mut out);
    }
    out
}

fn flatten_metric_item(name: &str, item: &Value, out: &mut Vec<FlatMetric>) {
    // Prefer total_value when present (IG API style).
    if let Some(tv) = item.get("total_value") {
        flatten_value_node(name, tv, out);
        return;
    }
    // values array of {value, end_time} — online_followers uses hour-keyed objects.
    if let Some(values) = item.get("values").and_then(|v| v.as_array()) {
        let has_end_time = values
            .iter()
            .any(|e| e.get("end_time").and_then(|t| t.as_str()).is_some());
        if has_end_time {
            for entry in values {
                let Some(val) = entry.get("value") else {
                    continue;
                };
                match val {
                    Value::Number(n) => {
                        if let Some(f) = n.as_f64() {
                            if let Some(end) = entry.get("end_time").and_then(|e| e.as_str()) {
                                out.push(FlatMetric::with_dim(name, "end_time", end, f));
                            } else {
                                out.push(FlatMetric::simple(name, f));
                            }
                        }
                    }
                    Value::Object(map) => {
                        // online_followers: {"0": n, "1": n, ...}
                        for (k, v) in map {
                            if let Some(f) = value_as_f64(v) {
                                out.push(FlatMetric::with_dim(name, "hour", k, f));
                            }
                        }
                    }
                    _ => flatten_raw_value(name, val, out),
                }
            }
            return;
        }
        // Classic values[0].value without end_time series.
        if let Some(v) = item.pointer("/values/0/value") {
            flatten_raw_value(name, v, out);
        }
    }
}

fn flatten_value_node(name: &str, tv: &Value, out: &mut Vec<FlatMetric>) {
    if let Some(v) = tv.get("value") {
        flatten_raw_value(name, v, out);
    }
    // breakdowns
    if let Some(bds) = tv.get("breakdowns").and_then(|b| b.as_array()) {
        for bd in bds {
            let dim_keys: Vec<String> = bd
                .get("dimension_keys")
                .and_then(|d| d.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_owned()))
                        .collect()
                })
                .unwrap_or_default();
            let dim_key = dim_keys.first().cloned().unwrap_or_else(|| "dim".into());
            if let Some(results) = bd.get("results").and_then(|r| r.as_array()) {
                for r in results {
                    let dim_val = r
                        .get("dimension_values")
                        .and_then(|d| d.as_array())
                        .and_then(|a| a.first())
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_owned();
                    if let Some(f) = r.get("value").and_then(value_as_f64) {
                        out.push(FlatMetric::with_dim(name, &dim_key, dim_val, f));
                    }
                }
            }
        }
    }
}

fn flatten_raw_value(name: &str, v: &Value, out: &mut Vec<FlatMetric>) {
    match v {
        Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                out.push(FlatMetric::simple(name, f));
            }
        }
        Value::Object(map) => {
            for (k, val) in map {
                if let Some(f) = value_as_f64(val) {
                    out.push(FlatMetric::with_dim(name, "type", k, f));
                }
            }
        }
        Value::Array(_) => {}
        _ => {
            if let Some(f) = value_as_f64(v) {
                out.push(FlatMetric::simple(name, f));
            }
        }
    }
}

/// Flatten post/media object fields into metric rows.
pub fn flatten_fields(obj: &Value, fields: &[FieldSpec]) -> Vec<FlatMetric> {
    let mut out = Vec::new();
    for f in fields {
        let pointer = format!("/{}", f.path.replace('.', "/"));
        // Also try direct get for simple keys
        let val = obj
            .pointer(&pointer)
            .or_else(|| obj.get(f.name.split('.').next().unwrap_or(f.name)));
        // For nested paths like shares/count
        let val = val.or_else(|| {
            let parts: Vec<&str> = f.path.split('/').collect();
            let mut cur = obj;
            for p in &parts {
                cur = cur.get(*p)?;
            }
            Some(cur)
        });
        if let Some(v) = val
            && let Some(n) = value_as_f64(v)
        {
            out.push(FlatMetric::simple(f.store_as, n));
        }
    }
    out
}

pub fn value_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// Look up a simple (no-dim) metric from a flat list.
pub fn find_simple(rows: &[FlatMetric], name: &str) -> Option<i64> {
    rows.iter()
        .find(|r| r.metric.eq_ignore_ascii_case(name) && r.dim_key.is_empty())
        .map(|r| r.value.round() as i64)
}

/// Look up a simple (no-dim) metric as a float (rates, ratios).
pub fn find_f64(rows: &[FlatMetric], name: &str) -> Option<f64> {
    rows.iter()
        .find(|r| r.metric.eq_ignore_ascii_case(name) && r.dim_key.is_empty())
        .map(|r| r.value)
}

/// Normalize a skip rate to 0..=1 (Meta reports either a ratio or a percentage).
pub fn normalize_rate(v: f64) -> Option<f64> {
    if !v.is_finite() || v < 0.0 {
        return None;
    }
    let r = if v > 1.0 { v / 100.0 } else { v };
    (r <= 1.0).then_some(r)
}

/// Look up a dimensional metric (e.g. profile_activity / BIO_LINK_CLICKED).
pub fn find_dim(rows: &[FlatMetric], name: &str, dim_value: &str) -> Option<i64> {
    rows.iter()
        .find(|r| {
            r.metric.eq_ignore_ascii_case(name) && r.dim_value.eq_ignore_ascii_case(dim_value)
        })
        .map(|r| r.value.round() as i64)
}

/// Look up an object-map metric (e.g. post_clicks_by_type / link clicks).
pub fn find_type(rows: &[FlatMetric], name: &str, type_key: &str) -> Option<i64> {
    rows.iter()
        .find(|r| {
            r.metric.eq_ignore_ascii_case(name)
                && r.dim_key.eq_ignore_ascii_case("type")
                && r.dim_value.eq_ignore_ascii_case(type_key)
        })
        .map(|r| r.value.round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn skip_rate_normalizes_percent_and_ratio() {
        assert_eq!(normalize_rate(0.42), Some(0.42));
        assert_eq!(normalize_rate(42.0), Some(0.42));
        assert_eq!(normalize_rate(-1.0), None);
        assert_eq!(normalize_rate(250.0), None);
    }

    #[test]
    fn flattens_simple_number() {
        let payload = json!({
            "data": [{"name": "views", "values": [{"value": 42}]}]
        });
        let rows = flatten_insights(&payload);
        assert_eq!(rows, vec![FlatMetric::simple("views", 42.0)]);
    }

    #[test]
    fn flattens_total_value_and_breakdown() {
        let payload = json!({
            "data": [{
                "name": "profile_activity",
                "total_value": {
                    "value": 10,
                    "breakdowns": [{
                        "dimension_keys": ["action_type"],
                        "results": [
                            {"dimension_values": ["BIO_LINK_CLICKED"], "value": 7},
                            {"dimension_values": ["OTHER"], "value": 3}
                        ]
                    }]
                }
            }]
        });
        let rows = flatten_insights(&payload);
        assert!(rows.iter().any(|r| r.metric == "profile_activity"
            && r.dim_key.is_empty()
            && (r.value - 10.0).abs() < f64::EPSILON));
        assert_eq!(
            find_dim(&rows, "profile_activity", "BIO_LINK_CLICKED"),
            Some(7)
        );
    }

    #[test]
    fn flattens_object_map() {
        let payload = json!({
            "data": [{
                "name": "post_clicks_by_type",
                "values": [{"value": {"link clicks": 5, "other clicks": 2}}]
            }]
        });
        let rows = flatten_insights(&payload);
        assert_eq!(
            find_type(&rows, "post_clicks_by_type", "link clicks"),
            Some(5)
        );
    }

    #[test]
    fn flattens_online_followers_hours() {
        let payload = json!({
            "data": [{
                "name": "online_followers",
                "period": "lifetime",
                "values": [{
                    "value": {"0": 10, "14": 50, "20": 30},
                    "end_time": "2026-10-07T07:00:00+0000"
                }]
            }]
        });
        let rows = flatten_insights(&payload);
        assert_eq!(
            rows.iter()
                .find(|r| r.dim_key == "hour" && r.dim_value == "14")
                .map(|r| r.value),
            Some(50.0)
        );
    }

    #[test]
    fn batch_names_splits_by_breakdown() {
        let plain = batch_names(IG_POST_METRICS, None);
        assert!(plain.contains("views"));
        assert!(plain.contains("profile_visits"));
        assert!(!plain.contains("profile_activity"));
        let broken = batch_names(IG_POST_METRICS, Some("action_type"));
        assert_eq!(broken, "profile_activity");
    }
}
