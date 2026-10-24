//! Meta post insights fetching, flattening, and normalization (B16 + funnel upgrades).

use chrono::Utc;
use serde_json::{Value, json};
use sqlx::FromRow;
use uuid::Uuid;

use crate::analysis::rollups;
use crate::domain::new_id;
use crate::jobs::JobError;
use crate::meta::error::{MetaError, MetaErrorKind};
use crate::meta::metrics::{
    self, FB_POST_FIELDS, FB_POST_METRICS, FB_VIDEO_METRICS, FlatMetric, IG_POST_FIELDS,
    IG_POST_METRICS, MetricSpec,
};
use crate::meta::publish::mark_connection_needs_reauth;
use crate::state::AppState;

/// Fetch insights for a published post, store a snapshot + flat values, and recompute rollups.
pub async fn fetch_post_insights(
    state: &AppState,
    experiment_id: Uuid,
    snapshot_no: i32,
) -> Result<Value, JobError> {
    let row = load_context(state, experiment_id).await?;
    let token = state
        .secrets
        .get(&row.token_secret_name)
        .await
        .map_err(JobError::Retry)?;

    let (raw, normalized, flat) =
        match row.platform.as_str() {
            "instagram" => {
                let media_id = row.platform_post_id.as_deref().ok_or_else(|| {
                    JobError::permanent(anyhow::anyhow!("missing platform_post_id"))
                })?;
                match fetch_instagram(state, media_id, &token).await {
                    Ok(v) => v,
                    Err(e) => {
                        return Err(handle_meta_err(state, row.project_id, &row.platform, e).await);
                    }
                }
            }
            "facebook" => {
                let post_id = row.platform_post_id.as_deref().ok_or_else(|| {
                    JobError::permanent(anyhow::anyhow!("missing platform_post_id"))
                })?;
                let video_id = row.platform_video_id.as_deref().unwrap_or(post_id);
                match fetch_facebook(state, post_id, video_id, &token).await {
                    Ok(v) => v,
                    Err(e) => {
                        return Err(handle_meta_err(state, row.project_id, &row.platform, e).await);
                    }
                }
            }
            other => {
                return Err(JobError::permanent(anyhow::anyhow!(
                    "unknown platform: {other}"
                )));
            }
        };

    let snap_id = new_id();
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| JobError::Retry(e.into()))?;

    sqlx::query(
        r#"
        insert into post_metric_snapshots (
            id, experiment_id, snapshot_no, fetched_at, raw,
            views, reach, interactions, platform_link_clicks,
            profile_visits, bio_link_taps, follows, likes, comments, saves, shares, reposts,
            avg_watch_time_ms, total_watch_time_ms, skip_rate
        ) values ($1,$2,$3,now(),$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19)
        on conflict (experiment_id, snapshot_no) do update set
            fetched_at = excluded.fetched_at,
            raw = excluded.raw,
            views = excluded.views,
            reach = excluded.reach,
            interactions = excluded.interactions,
            platform_link_clicks = excluded.platform_link_clicks,
            profile_visits = excluded.profile_visits,
            bio_link_taps = excluded.bio_link_taps,
            follows = excluded.follows,
            likes = excluded.likes,
            comments = excluded.comments,
            saves = excluded.saves,
            shares = excluded.shares,
            reposts = excluded.reposts,
            avg_watch_time_ms = excluded.avg_watch_time_ms,
            total_watch_time_ms = excluded.total_watch_time_ms,
            skip_rate = excluded.skip_rate
        "#,
    )
    .bind(snap_id)
    .bind(experiment_id)
    .bind(snapshot_no)
    .bind(&raw)
    .bind(normalized.views)
    .bind(normalized.reach)
    .bind(normalized.interactions)
    .bind(normalized.platform_link_clicks)
    .bind(normalized.profile_visits)
    .bind(normalized.bio_link_taps)
    .bind(normalized.follows)
    .bind(normalized.likes)
    .bind(normalized.comments)
    .bind(normalized.saves)
    .bind(normalized.shares)
    .bind(normalized.reposts)
    .bind(normalized.avg_watch_time_ms)
    .bind(normalized.total_watch_time_ms)
    .bind(normalized.skip_rate)
    .execute(&mut *tx)
    .await
    .map_err(|e| JobError::Retry(e.into()))?;

    // Resolve actual snapshot id (conflict may have kept the old one).
    let actual_id: Uuid = sqlx::query_scalar(
        "select id from post_metric_snapshots where experiment_id = $1 and snapshot_no = $2",
    )
    .bind(experiment_id)
    .bind(snapshot_no)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| JobError::Retry(e.into()))?;

    sqlx::query("delete from post_metric_values where snapshot_id = $1")
        .bind(actual_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| JobError::Retry(e.into()))?;

    for row in &flat {
        sqlx::query(
            r#"
            insert into post_metric_values (
                snapshot_id, experiment_id, snapshot_no, metric, dim_key, dim_value, value
            ) values ($1,$2,$3,$4,$5,$6,$7)
            on conflict (snapshot_id, metric, dim_key, dim_value) do update
               set value = excluded.value
            "#,
        )
        .bind(actual_id)
        .bind(experiment_id)
        .bind(snapshot_no)
        .bind(&row.metric)
        .bind(&row.dim_key)
        .bind(&row.dim_value)
        .bind(row.value)
        .execute(&mut *tx)
        .await
        .map_err(|e| JobError::Retry(e.into()))?;
    }

    tx.commit().await.map_err(|e| JobError::Retry(e.into()))?;

    rollups::recompute(&state.db, experiment_id)
        .await
        .map_err(JobError::Retry)?;

    Ok(json!({
        "experiment_id": experiment_id,
        "snapshot_no": snapshot_no,
        "fetched_at": Utc::now(),
        "normalized": {
            "views": normalized.views,
            "reach": normalized.reach,
            "interactions": normalized.interactions,
            "platform_link_clicks": normalized.platform_link_clicks,
            "profile_visits": normalized.profile_visits,
            "bio_link_taps": normalized.bio_link_taps,
            "follows": normalized.follows,
            "likes": normalized.likes,
            "comments": normalized.comments,
            "saves": normalized.saves,
            "shares": normalized.shares,
            "reposts": normalized.reposts,
            "avg_watch_time_ms": normalized.avg_watch_time_ms,
            "total_watch_time_ms": normalized.total_watch_time_ms,
            "skip_rate": normalized.skip_rate,
        },
        "flat_count": flat.len(),
    }))
}

async fn fetch_instagram(
    state: &AppState,
    media_id: &str,
    token: &str,
) -> Result<(Value, NormalizedMetrics, Vec<FlatMetric>), MetaError> {
    let (insights_parts, errors) =
        fetch_metric_catalog(&state.meta, media_id, IG_POST_METRICS, None, token).await?;

    let fields_csv = IG_POST_FIELDS
        .iter()
        .map(|f| f.name)
        .collect::<Vec<_>>()
        .join(",");
    let fields_obj = match state
        .meta
        .get(media_id, &[("fields", &fields_csv)], token)
        .await
    {
        Ok(v) => v,
        Err(e) if is_invalid_metric(&e) => {
            tracing::debug!(error = %e, "ig media fields unavailable");
            json!({})
        }
        Err(e) => return Err(e),
    };

    let mut flat = Vec::new();
    for part in &insights_parts {
        flat.extend(metrics::flatten_insights(part));
    }
    flat.extend(metrics::flatten_fields(&fields_obj, IG_POST_FIELDS));

    let raw = json!({
        "insights": insights_parts,
        "fields": fields_obj,
        "errors": errors,
    });

    let n = NormalizedMetrics {
        views: metrics::find_simple(&flat, "views")
            .or_else(|| metrics::find_simple(&flat, "total_views")),
        reach: metrics::find_simple(&flat, "reach"),
        interactions: metrics::find_simple(&flat, "total_interactions"),
        platform_link_clicks: metrics::find_dim(&flat, "profile_activity", "BIO_LINK_CLICKED"),
        profile_visits: metrics::find_simple(&flat, "profile_visits"),
        bio_link_taps: metrics::find_dim(&flat, "profile_activity", "BIO_LINK_CLICKED"),
        follows: metrics::find_simple(&flat, "follows"),
        likes: metrics::find_simple(&flat, "likes")
            .or_else(|| metrics::find_simple(&flat, "total_likes"))
            .or_else(|| metrics::find_simple(&flat, "field_like_count")),
        comments: metrics::find_simple(&flat, "comments")
            .or_else(|| metrics::find_simple(&flat, "total_comments"))
            .or_else(|| metrics::find_simple(&flat, "field_comments_count")),
        saves: metrics::find_simple(&flat, "saved"),
        shares: metrics::find_simple(&flat, "shares"),
        reposts: metrics::find_simple(&flat, "reposts"),
        avg_watch_time_ms: metrics::find_simple(&flat, "ig_reels_avg_watch_time"),
        total_watch_time_ms: metrics::find_simple(&flat, "ig_reels_video_view_total_time"),
        skip_rate: metrics::find_f64(&flat, "reels_skip_rate").and_then(metrics::normalize_rate),
    };
    Ok((raw, n, flat))
}

async fn fetch_facebook(
    state: &AppState,
    post_id: &str,
    video_id: &str,
    token: &str,
) -> Result<(Value, NormalizedMetrics, Vec<FlatMetric>), MetaError> {
    let (mut insights_parts, mut errors) = fetch_metric_catalog(
        &state.meta,
        post_id,
        FB_POST_METRICS,
        Some("lifetime"),
        token,
    )
    .await?;

    // Reel watch-time metrics live on the video object.
    let (video_parts, video_errors) = fetch_metric_catalog_at(
        &state.meta,
        &format!("{video_id}/video_insights"),
        FB_VIDEO_METRICS,
        None,
        token,
    )
    .await?;
    insights_parts.extend(video_parts);
    errors.extend(video_errors);

    // Also request post_media_view with breakdowns separately.
    let mut breakdown_parts = Vec::new();
    for bd in ["is_from_ads", "is_from_followers"] {
        match state
            .meta
            .get(
                &format!("{post_id}/insights"),
                &[
                    ("metric", "post_media_view"),
                    ("period", "lifetime"),
                    ("breakdown", bd),
                ],
                token,
            )
            .await
        {
            Ok(v) => breakdown_parts.push(json!({"breakdown": bd, "data": v})),
            Err(e) if is_invalid_metric(&e) || e.kind == MetaErrorKind::Retryable => {
                tracing::debug!(error = %e, breakdown = bd, "fb breakdown unavailable");
            }
            Err(e) => return Err(e),
        }
    }

    let fields_csv = "shares,reactions.summary(true),comments.summary(true)";
    let fields_obj = match state
        .meta
        .get(post_id, &[("fields", fields_csv)], token)
        .await
    {
        Ok(v) => v,
        Err(e) if is_invalid_metric(&e) => {
            tracing::debug!(error = %e, "fb post fields unavailable");
            json!({})
        }
        Err(e) => return Err(e),
    };

    let mut flat = Vec::new();
    for part in &insights_parts {
        flat.extend(metrics::flatten_insights(part));
    }
    for part in &breakdown_parts {
        if let Some(inner) = part.get("data") {
            let mut rows = metrics::flatten_insights(inner);
            let bd = part
                .get("breakdown")
                .and_then(|b| b.as_str())
                .unwrap_or("breakdown");
            for r in &mut rows {
                if r.dim_key.is_empty() {
                    r.dim_key = bd.to_owned();
                }
            }
            flat.extend(rows);
        }
    }
    flat.extend(metrics::flatten_fields(&fields_obj, FB_POST_FIELDS));

    let raw = json!({
        "insights": insights_parts,
        "breakdowns": breakdown_parts,
        "fields": fields_obj,
        "errors": errors,
    });

    let n = NormalizedMetrics {
        views: metrics::find_simple(&flat, "post_media_view"),
        reach: metrics::find_simple(&flat, "post_total_media_view_unique"),
        interactions: metrics::find_simple(&flat, "post_clicks"),
        platform_link_clicks: metrics::find_type(&flat, "post_clicks_by_type", "link clicks"),
        profile_visits: None,
        bio_link_taps: None,
        follows: None,
        likes: metrics::find_simple(&flat, "post_reactions_like_total")
            .or_else(|| metrics::find_simple(&flat, "field_reactions")),
        comments: metrics::find_simple(&flat, "field_comments"),
        saves: None,
        shares: metrics::find_simple(&flat, "field_shares"),
        reposts: None,
        avg_watch_time_ms: metrics::find_simple(&flat, "post_video_avg_time_watched"),
        total_watch_time_ms: metrics::find_simple(&flat, "post_video_view_time"),
        skip_rate: None,
    };
    Ok((raw, n, flat))
}

/// Fetch a catalog of metrics, batching by breakdown. On invalid-metric (code 100),
/// retry one metric at a time and collect errors instead of failing the snapshot.
async fn fetch_metric_catalog(
    meta: &crate::meta::MetaClient,
    object_id: &str,
    specs: &[MetricSpec],
    period: Option<&str>,
    token: &str,
) -> Result<(Vec<Value>, Vec<Value>), MetaError> {
    fetch_metric_catalog_at(meta, &format!("{object_id}/insights"), specs, period, token).await
}

/// Same as [`fetch_metric_catalog`] against an explicit insights edge path
/// (e.g. `{video_id}/video_insights`).
async fn fetch_metric_catalog_at(
    meta: &crate::meta::MetaClient,
    edge: &str,
    specs: &[MetricSpec],
    period: Option<&str>,
    token: &str,
) -> Result<(Vec<Value>, Vec<Value>), MetaError> {
    let object_id = edge;
    let mut parts = Vec::new();
    let mut errors = Vec::new();

    for group in metrics::breakdown_groups(specs) {
        let names = metrics::batch_names(specs, group);
        if names.is_empty() {
            continue;
        }
        match fetch_insights_batch(meta, object_id, &names, group, period, token).await {
            Ok(v) => parts.push(v),
            Err(e) if is_invalid_metric(&e) => {
                // Fall back to one metric at a time.
                let names_list: Vec<&str> = names.split(',').collect();
                for name in names_list {
                    match fetch_insights_batch(meta, object_id, name, group, period, token).await {
                        Ok(v) => parts.push(v),
                        Err(e2)
                            if is_invalid_metric(&e2) || e2.kind == MetaErrorKind::Retryable =>
                        {
                            errors.push(json!({
                                "metric": name,
                                "breakdown": group,
                                "error": e2.message,
                                "code": e2.code,
                            }));
                        }
                        Err(e2) => return Err(e2),
                    }
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok((parts, errors))
}

async fn fetch_insights_batch(
    meta: &crate::meta::MetaClient,
    edge: &str,
    metrics_csv: &str,
    breakdown: Option<&str>,
    period: Option<&str>,
    token: &str,
) -> Result<Value, MetaError> {
    let mut q: Vec<(&str, &str)> = vec![("metric", metrics_csv)];
    if let Some(p) = period {
        q.push(("period", p));
    }
    if let Some(b) = breakdown {
        q.push(("breakdown", b));
    }
    meta.get(edge, &q, token).await
}

fn is_invalid_metric(e: &MetaError) -> bool {
    e.code == Some(100)
        || e.message.to_ascii_lowercase().contains("invalid metric")
        || e.message.to_ascii_lowercase().contains("not available")
}

fn map_meta(e: MetaError) -> JobError {
    match e.kind {
        MetaErrorKind::NeedsReauth => JobError::permanent(anyhow::anyhow!(e)),
        MetaErrorKind::RateLimited => JobError::retry_after(e.retry_after_mins.max(1), e),
        MetaErrorKind::IgPublishingLimit => JobError::permanent(anyhow::anyhow!(e)),
        MetaErrorKind::Retryable => JobError::Retry(e.into()),
    }
}

async fn handle_meta_err(
    state: &AppState,
    project_id: Uuid,
    platform: &str,
    e: MetaError,
) -> JobError {
    if e.kind == MetaErrorKind::NeedsReauth {
        let _ = mark_connection_needs_reauth(&state.db, project_id, platform).await;
    }
    map_meta(e)
}

/// Instagram media insights helper (kept for callers/tests).
pub async fn ig_media_insights(
    meta: &crate::meta::MetaClient,
    media_id: &str,
    metrics: &str,
    token: &str,
) -> Result<Value, MetaError> {
    meta.get(
        &format!("{media_id}/insights"),
        &[("metric", metrics)],
        token,
    )
    .await
}

/// Facebook page post insights helper.
pub async fn fb_post_insights(
    meta: &crate::meta::MetaClient,
    post_id: &str,
    metrics: &str,
    token: &str,
) -> Result<Value, MetaError> {
    meta.get(
        &format!("{post_id}/insights"),
        &[("metric", metrics), ("period", "lifetime")],
        token,
    )
    .await
}

#[derive(Debug, Default, Clone)]
struct NormalizedMetrics {
    views: Option<i64>,
    reach: Option<i64>,
    interactions: Option<i64>,
    platform_link_clicks: Option<i64>,
    profile_visits: Option<i64>,
    bio_link_taps: Option<i64>,
    follows: Option<i64>,
    likes: Option<i64>,
    comments: Option<i64>,
    saves: Option<i64>,
    shares: Option<i64>,
    reposts: Option<i64>,
    avg_watch_time_ms: Option<i64>,
    total_watch_time_ms: Option<i64>,
    skip_rate: Option<f64>,
}

/// Extract a metric value: `values[0].value` (number) or `total_value.value`.
pub fn metric_number(payload: &Value, name: &str) -> Option<i64> {
    let item = find_metric(payload, name)?;
    extract_number_value(item)
}

/// Extract a named field from an object-valued metric (`values[0].value` object).
pub fn metric_object_field(payload: &Value, name: &str, field: &str) -> Option<i64> {
    let item = find_metric(payload, name)?;
    let obj = item
        .pointer("/values/0/value")
        .or_else(|| item.pointer("/total_value/value"))?;
    if let Some(n) = obj.get(field).and_then(value_as_i64) {
        return Some(n);
    }
    let map = obj.as_object()?;
    for (k, v) in map {
        if k.eq_ignore_ascii_case(field) {
            return value_as_i64(v);
        }
    }
    None
}

/// Breakdown lookup: `total_value.breakdowns[0].results[]` with matching dimension.
pub fn breakdown_value(payload: &Value, metric_name: &str, dimension: &str) -> Option<i64> {
    let item = find_metric(payload, metric_name)?;
    let results = item
        .pointer("/total_value/breakdowns/0/results")
        .or_else(|| item.pointer("/values/0/value"))?
        .as_array()?;
    for r in results {
        let dims = r
            .get("dimension_values")
            .and_then(|d| d.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();
        if dims.iter().any(|d| d.eq_ignore_ascii_case(dimension)) {
            return r.get("value").and_then(value_as_i64);
        }
    }
    None
}

fn find_metric<'a>(payload: &'a Value, name: &str) -> Option<&'a Value> {
    let data = payload.get("data").and_then(|d| d.as_array())?;
    data.iter().find(|m| {
        m.get("name")
            .and_then(|n| n.as_str())
            .is_some_and(|n| n.eq_ignore_ascii_case(name))
    })
}

fn extract_number_value(item: &Value) -> Option<i64> {
    if let Some(v) = item.pointer("/values/0/value")
        && let Some(n) = value_as_i64(v)
    {
        return Some(n);
    }
    if let Some(v) = item.pointer("/total_value/value") {
        return value_as_i64(v);
    }
    None
}

fn value_as_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

#[derive(Debug, FromRow)]
struct InsightCtx {
    platform: String,
    platform_post_id: Option<String>,
    platform_video_id: Option<String>,
    token_secret_name: String,
    project_id: Uuid,
}

async fn load_context(state: &AppState, experiment_id: Uuid) -> Result<InsightCtx, JobError> {
    let row = sqlx::query_as::<_, InsightCtx>(
        r#"
        select e.platform, p.platform_post_id, p.platform_video_id, pc.token_secret_name,
               e.project_id
          from experiments e
          join posts p on p.id = e.post_id
          join platform_connections pc
            on pc.project_id = e.project_id and pc.platform = e.platform
         where e.id = $1
        "#,
    )
    .bind(experiment_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| JobError::Retry(e.into()))?
    .ok_or_else(|| JobError::permanent(anyhow::anyhow!("experiment or connection missing")))?;

    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_values_array_number() {
        let payload = json!({
            "data": [{
                "name": "views",
                "values": [{"value": 42}]
            }]
        });
        assert_eq!(metric_number(&payload, "views"), Some(42));
    }

    #[test]
    fn parses_total_value() {
        let payload = json!({
            "data": [{
                "name": "reach",
                "total_value": {"value": 99}
            }]
        });
        assert_eq!(metric_number(&payload, "reach"), Some(99));
    }

    #[test]
    fn missing_metric_is_none_not_zero() {
        let payload = json!({"data": []});
        assert_eq!(metric_number(&payload, "views"), None);
    }

    #[test]
    fn parses_bio_link_breakdown() {
        let payload = json!({
            "data": [{
                "name": "profile_activity",
                "total_value": {
                    "value": 10,
                    "breakdowns": [{
                        "results": [
                            {"dimension_values": ["BIO_LINK_CLICKED"], "value": 7},
                            {"dimension_values": ["OTHER"], "value": 3}
                        ]
                    }]
                }
            }]
        });
        assert_eq!(
            breakdown_value(&payload, "profile_activity", "BIO_LINK_CLICKED"),
            Some(7)
        );
    }

    #[test]
    fn parses_fb_link_clicks_object() {
        let payload = json!({
            "data": [{
                "name": "post_clicks_by_type",
                "values": [{"value": {"link clicks": 5, "other clicks": 2}}]
            }]
        });
        assert_eq!(
            metric_object_field(&payload, "post_clicks_by_type", "link clicks"),
            Some(5)
        );
    }

    #[test]
    fn invalid_metric_detection() {
        let e = MetaError {
            kind: MetaErrorKind::Retryable,
            message: "Invalid metric".into(),
            code: Some(100),
            error_subcode: None,
            fbtrace_id: None,
            retry_after_mins: 0,
        };
        assert!(is_invalid_metric(&e));
    }
}
