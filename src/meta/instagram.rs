//! Instagram Reels publishing: create a REELS container from a SAS URL, poll until the video
//! is processed, then `media_publish`.

use chrono::{Duration, Utc};
use serde_json::Value;
use uuid::Uuid;

use crate::jobs::{JobError, UserJob};
use crate::meta::publish::{PublishCtx, finalize_publish, load_ctx, map_meta_err, parse_meta_ts};
use crate::state::AppState;

/// Meta fetches the video asynchronously; keep the URL valid well past processing time.
pub const VIDEO_URL_TTL_HOURS: i64 = 3;
/// Seconds between container polls and the number of polls (≈ 30 min of processing).
const POLL_INTERVAL_SECS: i64 = 30;
const MAX_POLLS: i32 = 60;

pub async fn publish_instagram(state: &AppState, post_id: Uuid) -> Result<(), JobError> {
    let row = load_ctx(state, post_id).await?;
    let token = state
        .secrets
        .get(&row.token_secret_name)
        .await
        .map_err(JobError::Retry)?;
    let ig = &row.external_account_id;

    // Restart while a container exists: only poll, never create a second container.
    if row.status == "publishing" || row.container_id.is_some() {
        return enqueue_poll(state, post_id, 1, Duration::seconds(15)).await;
    }

    let blob_path = row
        .render_blob_path
        .as_deref()
        .ok_or_else(|| JobError::permanent(anyhow::anyhow!("video has no render to publish")))?;

    let lim = match state
        .meta
        .get(
            &format!("{ig}/content_publishing_limit"),
            &[("fields", "config,quota_usage")],
            &token,
        )
        .await
    {
        Ok(v) => v,
        Err(e) => return Err(map_meta_err(state, &row, e).await),
    };
    if quota_exhausted(&lim) {
        return Err(JobError::permanent(anyhow::anyhow!(
            "Instagram daily publishing limit reached."
        )));
    }

    let video_url = state
        .blob
        .public_read_url(blob_path, Duration::hours(VIDEO_URL_TTL_HOURS))
        .await
        .map_err(JobError::Retry)?;
    let caption = row.final_caption(state);

    let r = match state
        .meta
        .post_form(
            &format!("{ig}/media"),
            &[
                ("media_type", "REELS"),
                ("video_url", video_url.as_str()),
                ("caption", caption.as_str()),
                ("share_to_feed", "true"),
                ("is_ai_generated", "true"),
            ],
            &token,
        )
        .await
    {
        Ok(v) => v,
        Err(e) => return Err(map_meta_err(state, &row, e).await),
    };

    let container_id = r["id"]
        .as_str()
        .ok_or_else(|| JobError::permanent(anyhow::anyhow!("IG media container missing id")))?
        .to_owned();

    sqlx::query(
        r#"
        update posts
           set container_id = $2, status = 'container_created', error = null,
               publish_started_at = coalesce(publish_started_at, now())
         where id = $1
        "#,
    )
    .bind(post_id)
    .bind(&container_id)
    .execute(&state.db)
    .await
    .map_err(|e| JobError::Retry(e.into()))?;

    enqueue_poll(state, post_id, 1, Duration::seconds(POLL_INTERVAL_SECS)).await
}

pub async fn poll_ig_container(
    state: &AppState,
    post_id: Uuid,
    poll_no: i32,
) -> Result<(), JobError> {
    let row = load_ctx(state, post_id).await?;
    if row.status == "published" || row.status == "failed" {
        return Ok(());
    }
    let token = state
        .secrets
        .get(&row.token_secret_name)
        .await
        .map_err(JobError::Retry)?;
    let ig = &row.external_account_id;
    let container = row
        .container_id
        .as_deref()
        .ok_or_else(|| JobError::permanent(anyhow::anyhow!("missing IG container_id")))?;

    let st = match state
        .meta
        .get(container, &[("fields", "status_code,status")], &token)
        .await
    {
        Ok(v) => v,
        Err(e) => return Err(map_meta_err(state, &row, e).await),
    };

    match st["status_code"].as_str() {
        Some("FINISHED") => {
            // Crash after media_publish: recover. Crash after mark_publishing only: publish once.
            if row.status == "publishing" {
                match recover_ig_media(state, &row, &token, ig).await {
                    Ok(()) => return Ok(()),
                    Err(JobError::Retry(_)) => { /* fall through to media_publish */ }
                    Err(e) => return Err(e),
                }
            } else {
                sqlx::query(
                    r#"
                    update posts
                       set status = 'publishing',
                           publish_started_at = coalesce(publish_started_at, now())
                     where id = $1
                    "#,
                )
                .bind(post_id)
                .execute(&state.db)
                .await
                .map_err(|e| JobError::Retry(e.into()))?;
            }

            let r = match state
                .meta
                .post_form(
                    &format!("{ig}/media_publish"),
                    &[("creation_id", container)],
                    &token,
                )
                .await
            {
                Ok(v) => v,
                Err(e) => return Err(map_meta_err(state, &row, e).await),
            };
            let media_id = r["id"]
                .as_str()
                .ok_or_else(|| JobError::permanent(anyhow::anyhow!("media_publish missing id")))?;
            let m = match state
                .meta
                .get(media_id, &[("fields", "permalink,timestamp")], &token)
                .await
            {
                Ok(v) => v,
                Err(e) => return Err(map_meta_err(state, &row, e).await),
            };
            let permalink = m["permalink"].as_str();
            let published_at = parse_meta_ts(m["timestamp"].as_str()).unwrap_or_else(Utc::now);
            finalize_publish(state, post_id, media_id, permalink, published_at, None).await
        }
        Some("PUBLISHED") => recover_ig_media(state, &row, &token, ig).await,
        Some("IN_PROGRESS") if poll_no < MAX_POLLS => {
            sqlx::query("update posts set container_polls = $2 where id = $1")
                .bind(post_id)
                .bind(poll_no)
                .execute(&state.db)
                .await
                .map_err(|e| JobError::Retry(e.into()))?;
            enqueue_poll(
                state,
                post_id,
                poll_no + 1,
                Duration::seconds(POLL_INTERVAL_SECS),
            )
            .await
        }
        other => {
            let detail = st["status"].as_str().unwrap_or("");
            Err(JobError::permanent(anyhow::anyhow!(
                "Instagram video processing ended with status {other:?} {detail}"
            )))
        }
    }
}

async fn recover_ig_media(
    state: &AppState,
    row: &PublishCtx,
    token: &str,
    ig: &str,
) -> Result<(), JobError> {
    let m = match state
        .meta
        .get(
            &format!("{ig}/media"),
            &[
                ("fields", "id,caption,timestamp,permalink"),
                ("limit", "10"),
            ],
            token,
        )
        .await
    {
        Ok(v) => v,
        Err(e) => return Err(map_meta_err(state, row, e).await),
    };

    let caption_sent = row.final_caption(state);
    let start = row
        .publish_started_at
        .unwrap_or_else(|| Utc::now() - Duration::hours(4));
    let data = m["data"].as_array().cloned().unwrap_or_default();
    for item in data {
        if item["caption"].as_str().unwrap_or("") != caption_sent {
            continue;
        }
        let ts = parse_meta_ts(item["timestamp"].as_str()).unwrap_or(Utc::now());
        if ts < start {
            continue;
        }
        let media_id = item["id"]
            .as_str()
            .ok_or_else(|| JobError::permanent(anyhow::anyhow!("recovered media missing id")))?;
        let permalink = item["permalink"].as_str();
        return finalize_publish(state, row.post_id, media_id, permalink, ts, None).await;
    }

    Err(JobError::Retry(anyhow::anyhow!(
        "IG recover: matching media not found"
    )))
}

fn quota_exhausted(lim: &Value) -> bool {
    let Some(data) = lim["data"].as_array().and_then(|a| a.first()) else {
        return false;
    };
    let usage = data["quota_usage"].as_i64().unwrap_or(0);
    let total = data
        .pointer("/config/quota_total")
        .and_then(|v| v.as_i64())
        .unwrap_or(i64::MAX);
    usage >= total
}

async fn enqueue_poll(
    state: &AppState,
    post_id: Uuid,
    poll_no: i32,
    delay: Duration,
) -> Result<(), JobError> {
    state
        .user_jobs
        .enqueue_at(
            &UserJob::PollIgContainer { post_id, poll_no },
            Some(format!("poll-ig:{post_id}:{poll_no}")),
            Utc::now() + delay,
        )
        .await
        .map_err(|e| JobError::Retry(e.into()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_detection() {
        let v = serde_json::json!({"data":[{"quota_usage":50,"config":{"quota_total":50}}]});
        assert!(quota_exhausted(&v));
        let v = serde_json::json!({"data":[{"quota_usage":3,"config":{"quota_total":50}}]});
        assert!(!quota_exhausted(&v));
        assert!(!quota_exhausted(&serde_json::json!({})));
    }

    #[test]
    fn sas_outlives_processing() {
        // ≥2 h for Meta's fetch, and longer than the full polling window.
        assert!(VIDEO_URL_TTL_HOURS >= 2);
        assert!(
            Duration::hours(VIDEO_URL_TTL_HOURS)
                > Duration::seconds(POLL_INTERVAL_SECS * MAX_POLLS as i64)
        );
    }
}
