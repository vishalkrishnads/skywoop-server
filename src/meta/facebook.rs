//! Facebook Reels publishing via the `video_reels` edge:
//! `start` → rupload with a hosted `file_url` → `finish` (PUBLISHED) → poll the video status.

use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use uuid::Uuid;

use crate::jobs::{JobError, UserJob};
use crate::meta::publish::{PublishCtx, finalize_publish, load_ctx, map_meta_err, parse_meta_ts};
use crate::state::AppState;

/// Facebook allows 30 API-published Reels per rolling 24 h per Page.
pub const REELS_PER_24H: usize = 30;
pub const VIDEO_URL_TTL_HOURS: i64 = 3;
const POLL_INTERVAL_SECS: i64 = 30;
const MAX_POLLS: i32 = 60;

pub async fn publish_facebook(state: &AppState, post_id: Uuid) -> Result<(), JobError> {
    let row = load_ctx(state, post_id).await?;
    // Already handed to Facebook: only poll, never upload a second copy.
    if row.status == "publishing" || row.container_id.is_some() {
        return enqueue_poll(state, post_id, 1, Duration::seconds(15)).await;
    }
    let blob_path = row
        .render_blob_path
        .as_deref()
        .ok_or_else(|| JobError::permanent(anyhow::anyhow!("video has no render to publish")))?;

    let token = state
        .secrets
        .get(&row.token_secret_name)
        .await
        .map_err(JobError::Retry)?;
    let page = &row.external_account_id;

    enforce_rate_limit(state, &row).await?;

    // 1. start
    let started = match state
        .meta
        .post_form(
            &format!("{page}/video_reels"),
            &[("upload_phase", "start")],
            &token,
        )
        .await
    {
        Ok(v) => v,
        Err(e) => return Err(map_meta_err(state, &row, e).await),
    };
    let video_id = started["video_id"]
        .as_str()
        .ok_or_else(|| JobError::permanent(anyhow::anyhow!("video_reels start missing video_id")))?
        .to_owned();
    let upload_url = started["upload_url"]
        .as_str()
        .ok_or_else(|| {
            JobError::permanent(anyhow::anyhow!("video_reels start missing upload_url"))
        })?
        .to_owned();

    // 2. rupload: Facebook pulls the file from our SAS URL.
    let file_url = state
        .blob
        .public_read_url(blob_path, Duration::hours(VIDEO_URL_TTL_HOURS))
        .await
        .map_err(JobError::Retry)?;
    let resp = state
        .meta
        .http
        .post(&upload_url)
        .header("Authorization", format!("OAuth {token}"))
        .header("file_url", file_url.as_str())
        .send()
        .await
        .map_err(|e| JobError::Retry(anyhow::anyhow!("rupload transport: {e}")))?;
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() || body["success"].as_bool() == Some(false) {
        let msg = format!("Facebook rupload failed ({status}): {body}");
        return Err(if status.is_client_error() && status.as_u16() != 429 {
            JobError::permanent(anyhow::anyhow!(msg))
        } else {
            JobError::Retry(anyhow::anyhow!(msg))
        });
    }

    // 3. finish + publish. Mark publishing first so a crash never re-uploads.
    sqlx::query(
        r#"
        update posts
           set status = 'publishing', platform_video_id = $2, container_id = $2,
               publish_started_at = coalesce(publish_started_at, now()), error = null
         where id = $1
        "#,
    )
    .bind(post_id)
    .bind(&video_id)
    .execute(&state.db)
    .await
    .map_err(|e| JobError::Retry(e.into()))?;

    let caption = row.final_caption(state);
    match state
        .meta
        .post_form(
            &format!("{page}/video_reels"),
            &[
                ("upload_phase", "finish"),
                ("video_id", video_id.as_str()),
                ("video_state", "PUBLISHED"),
                ("description", caption.as_str()),
            ],
            &token,
        )
        .await
    {
        Ok(_) => {}
        Err(e) => return Err(map_meta_err(state, &row, e).await),
    }

    enqueue_poll(state, post_id, 1, Duration::seconds(POLL_INTERVAL_SECS)).await
}

pub async fn poll_fb_video(state: &AppState, post_id: Uuid, poll_no: i32) -> Result<(), JobError> {
    let row = load_ctx(state, post_id).await?;
    if row.status == "published" || row.status == "failed" {
        return Ok(());
    }
    let video_id = row
        .platform_video_id
        .as_deref()
        .or(row.container_id.as_deref())
        .ok_or_else(|| JobError::permanent(anyhow::anyhow!("missing Facebook video id")))?
        .to_owned();
    let token = state
        .secrets
        .get(&row.token_secret_name)
        .await
        .map_err(JobError::Retry)?;

    let v = match state
        .meta
        .get(
            &video_id,
            &[("fields", "status,permalink_url,created_time")],
            &token,
        )
        .await
    {
        Ok(v) => v,
        Err(e) => return Err(map_meta_err(state, &row, e).await),
    };

    match classify_video_status(&v) {
        VideoState::Published => {
            let permalink = absolute_permalink(v["permalink_url"].as_str());
            let published_at = parse_meta_ts(v["created_time"].as_str()).unwrap_or_else(Utc::now);
            finalize_publish(
                state,
                post_id,
                &video_id,
                permalink.as_deref(),
                published_at,
                Some(&video_id),
            )
            .await
        }
        VideoState::Processing if poll_no < MAX_POLLS => {
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
        VideoState::Processing => Err(JobError::permanent(anyhow::anyhow!(
            "Facebook Reel still processing after {MAX_POLLS} polls"
        ))),
        VideoState::Failed(reason) => Err(JobError::permanent(anyhow::anyhow!(
            "Facebook Reel failed: {reason}"
        ))),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum VideoState {
    Processing,
    Published,
    Failed(String),
}

fn classify_video_status(v: &Value) -> VideoState {
    let status = &v["status"];
    let video_status = status["video_status"].as_str().unwrap_or("");
    let publishing = status
        .pointer("/publishing_phase/status")
        .and_then(Value::as_str)
        .unwrap_or("");
    let uploading = status
        .pointer("/uploading_phase/status")
        .and_then(Value::as_str)
        .unwrap_or("");
    let processing = status
        .pointer("/processing_phase/status")
        .and_then(Value::as_str)
        .unwrap_or("");

    if video_status == "error"
        || publishing == "error"
        || uploading == "error"
        || processing == "error"
    {
        let reason = status
            .pointer("/processing_phase/error/message")
            .or_else(|| status.pointer("/uploading_phase/error/message"))
            .or_else(|| status.pointer("/publishing_phase/errors/0/message"))
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return VideoState::Failed(reason.to_owned());
    }
    if video_status == "expired" {
        return VideoState::Failed("upload expired".into());
    }
    if video_status == "ready" && publishing == "complete" {
        return VideoState::Published;
    }
    VideoState::Processing
}

fn absolute_permalink(raw: Option<&str>) -> Option<String> {
    let p = raw?.trim();
    if p.is_empty() {
        None
    } else if p.starts_with("http") {
        Some(p.to_owned())
    } else {
        Some(format!("https://www.facebook.com{p}"))
    }
}

/// Defer when this Page already published [`REELS_PER_24H`] Reels in the last 24 hours.
async fn enforce_rate_limit(state: &AppState, row: &PublishCtx) -> Result<(), JobError> {
    let times: Vec<DateTime<Utc>> = sqlx::query_scalar(
        r#"
        select coalesce(published_at, publish_started_at)
          from posts
         where project_id = $1 and platform = 'facebook' and id <> $2
           and status in ('publishing', 'published')
           and coalesce(published_at, publish_started_at) > now() - interval '24 hours'
         order by 1 desc
         limit $3
        "#,
    )
    .bind(row.project_id)
    .bind(row.post_id)
    .bind(REELS_PER_24H as i64)
    .fetch_all(&state.db)
    .await
    .map_err(|e| JobError::Retry(e.into()))?;
    if let Some(wait) = reels_limit_wait_minutes(&times, Utc::now()) {
        return Err(JobError::retry_after(
            wait,
            anyhow::anyhow!("Facebook allows {REELS_PER_24H} Reels per 24 hours; waiting"),
        ));
    }
    Ok(())
}

/// Minutes to wait until the oldest of the last `REELS_PER_24H` Reels leaves the 24 h window.
/// `times` must be sorted newest first.
fn reels_limit_wait_minutes(times: &[DateTime<Utc>], now: DateTime<Utc>) -> Option<i64> {
    if times.len() < REELS_PER_24H {
        return None;
    }
    let frees_at = times[REELS_PER_24H - 1] + Duration::hours(24);
    Some(((frees_at - now).num_minutes() + 1).max(1))
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
            &UserJob::PollFbVideo { post_id, poll_no },
            Some(format!("poll-fb:{post_id}:{poll_no}")),
            Utc::now() + delay,
        )
        .await
        .map_err(|e| JobError::Retry(e.into()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn video_status_classification() {
        let ready =
            json!({"status":{"video_status":"ready","publishing_phase":{"status":"complete"}}});
        assert_eq!(classify_video_status(&ready), VideoState::Published);
        let proc = json!({"status":{"video_status":"processing"}});
        assert_eq!(classify_video_status(&proc), VideoState::Processing);
        let err = json!({"status":{"video_status":"error","processing_phase":{"status":"error","error":{"message":"bad codec"}}}});
        assert_eq!(
            classify_video_status(&err),
            VideoState::Failed("bad codec".into())
        );
    }

    #[test]
    fn permalink_is_absolute() {
        assert_eq!(
            absolute_permalink(Some("/reel/123")).as_deref(),
            Some("https://www.facebook.com/reel/123")
        );
        assert_eq!(
            absolute_permalink(Some("https://fb.com/x")).as_deref(),
            Some("https://fb.com/x")
        );
        assert_eq!(absolute_permalink(None), None);
    }

    #[test]
    fn limit_wait() {
        let now = Utc::now();
        let few: Vec<_> = (0..29).map(|i| now - Duration::minutes(i)).collect();
        assert_eq!(reels_limit_wait_minutes(&few, now), None);
        let full: Vec<_> = (0..30)
            .map(|i| now - Duration::hours(23) - Duration::minutes(i))
            .collect();
        let wait = reels_limit_wait_minutes(&full, now).unwrap();
        assert!((1..=60).contains(&wait), "wait {wait}");
    }
}
