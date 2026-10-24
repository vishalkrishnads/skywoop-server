//! Shared Reels publishing plumbing: context loading, finalize, failure and media cleanup.

use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::links;
use crate::domain::plan::maybe_finish_plan;
use crate::domain::{Platform, new_id};
use crate::jobs::{DataJob, DataJobQueue, JobError};
use crate::meta::error::{MetaError, MetaErrorKind};
use crate::plan_format;
use crate::state::AppState;
use crate::trends;

/// Hours after publish when `FetchPostInsights` snapshots are scheduled.
pub const SNAPSHOT_OFFSETS_H: &[i64] = &[1, 6, 24, 48, 72, 96, 120, 144, 168, 336, 504, 672];

/// Everything a platform publisher needs for one post.
#[derive(Debug, sqlx::FromRow)]
pub struct PublishCtx {
    pub post_id: Uuid,
    pub status: String,
    pub attempt: i32,
    pub container_id: Option<String>,
    pub platform_video_id: Option<String>,
    pub publish_started_at: Option<DateTime<Utc>>,
    pub project_id: Uuid,
    pub platform: String,
    pub plan_video_id: Uuid,
    pub title: String,
    /// Caption as authored in the plan (no tracked link / CTA yet).
    pub caption: String,
    pub tracked_token: Option<String>,
    pub render_blob_path: Option<String>,
    pub external_account_id: String,
    pub token_secret_name: String,
}

impl PublishCtx {
    /// Caption with the tracked link (Facebook) or the link-in-bio CTA (Instagram).
    pub fn final_caption(&self, state: &AppState) -> String {
        let base = if self.caption.trim().is_empty() {
            self.title.as_str()
        } else {
            self.caption.as_str()
        };
        let platform = Platform::parse(&self.platform).unwrap_or(Platform::Instagram);
        match self.tracked_token.as_deref() {
            Some(token) => {
                links::final_caption(platform, base, state.cfg.public_base_url.as_str(), token)
            }
            None => base.to_owned(),
        }
    }
}

pub async fn load_ctx(state: &AppState, post_id: Uuid) -> Result<PublishCtx, JobError> {
    sqlx::query_as::<_, PublishCtx>(
        r#"
        select po.id as post_id, po.status, po.attempt, po.container_id, po.platform_video_id,
               po.publish_started_at, po.project_id, po.platform, po.plan_video_id,
               pv.title, pv.caption, tl.token as tracked_token,
               vp.render_blob_path, pc.external_account_id, pc.token_secret_name
          from posts po
          join plan_videos pv on pv.id = po.plan_video_id
          left join tracked_links tl on tl.plan_video_id = pv.id
          left join video_projects vp on vp.plan_video_id = pv.id
          join platform_connections pc
            on pc.project_id = po.project_id and pc.platform = po.platform
         where po.id = $1
        "#,
    )
    .bind(post_id)
    .fetch_one(&state.db)
    .await
    .map_err(|e| match e {
        sqlx::Error::RowNotFound => JobError::permanent(anyhow::anyhow!(
            "post {post_id} has no plan video, connection or render"
        )),
        e => JobError::Retry(e.into()),
    })
}

/// Finalize a successful Meta publish in one transaction, then delete the MP4 from storage.
pub async fn finalize_publish(
    state: &AppState,
    post_id: Uuid,
    platform_post_id: &str,
    permalink: Option<&str>,
    published_at: DateTime<Utc>,
    platform_video_id: Option<&str>,
) -> Result<(), JobError> {
    let retry = |e: sqlx::Error| JobError::Retry(e.into());
    let mut tx = state.db.begin().await.map_err(retry)?;

    let row = sqlx::query_as::<_, FinalizeRow>(
        r#"
        select po.plan_video_id, po.project_id, po.platform,
               pv.plan_id, pv.anchor, pv.track, pv.audience_id,
               dp.markdown, proj.timezone
          from posts po
          join plan_videos pv on pv.id = po.plan_video_id
          join daily_plans dp on dp.id = pv.plan_id
          join projects proj on proj.id = po.project_id
         where po.id = $1
         for update of po
        "#,
    )
    .bind(post_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(retry)?;

    sqlx::query(
        r#"
        update posts
           set status = 'published',
               platform_post_id = $2,
               platform_video_id = coalesce($3, platform_video_id),
               permalink = $4,
               published_at = $5,
               error = null
         where id = $1
        "#,
    )
    .bind(post_id)
    .bind(platform_post_id)
    .bind(platform_video_id)
    .bind(permalink)
    .bind(published_at)
    .execute(&mut *tx)
    .await
    .map_err(retry)?;

    let hypothesis = plan_format::parse(&row.markdown)
        .videos
        .into_iter()
        .find(|v| v.anchor == row.anchor)
        .map(|v| v.hypothesis)
        .unwrap_or_default();
    let publish_local = trends::local_date_for_tz(&row.timezone, published_at);

    sqlx::query(
        r#"
        insert into experiments (
            id, plan_video_id, post_id, project_id, platform, track, audience_id,
            published_at, publish_local_date, hypothesis, maturity
        ) values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,'collecting')
        on conflict (plan_video_id) do nothing
        "#,
    )
    .bind(new_id())
    .bind(row.plan_video_id)
    .bind(post_id)
    .bind(row.project_id)
    .bind(&row.platform)
    .bind(&row.track)
    .bind(row.audience_id)
    .bind(published_at)
    .bind(publish_local)
    .bind(&hypothesis)
    .execute(&mut *tx)
    .await
    .map_err(retry)?;

    let experiment_id: Uuid =
        sqlx::query_scalar("select id from experiments where plan_video_id = $1")
            .bind(row.plan_video_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(retry)?;

    let data_jobs = DataJobQueue::new(state.db.clone());
    for (n, h) in SNAPSHOT_OFFSETS_H.iter().enumerate() {
        data_jobs
            .enqueue(
                &mut tx,
                &DataJob::FetchPostInsights {
                    experiment_id,
                    snapshot_no: n as i32,
                },
                Some(format!("insights:{experiment_id}:{n}")),
                published_at + Duration::hours(*h),
            )
            .await
            .map_err(retry)?;
    }

    tx.commit().await.map_err(retry)?;

    if let Err(e) = maybe_finish_plan(&state.db, row.plan_id).await {
        tracing::warn!(error = ?e, plan_id = %row.plan_id, "maybe_finish_plan failed");
    }
    // Best effort; the scheduler sweep retries failures.
    if let Err(e) = cleanup_post_media(state, post_id).await {
        tracing::warn!(error = ?e, %post_id, "post media cleanup failed; sweep will retry");
    }
    Ok(())
}

#[derive(Debug, sqlx::FromRow)]
struct FinalizeRow {
    plan_video_id: Uuid,
    project_id: Uuid,
    platform: String,
    plan_id: Uuid,
    anchor: String,
    track: String,
    audience_id: Option<Uuid>,
    markdown: String,
    timezone: String,
}

/// Delete the rendered MP4 of a published post from blob storage and mark the post.
/// Idempotent. Posts that are not published yet are left untouched.
pub async fn cleanup_post_media(state: &AppState, post_id: Uuid) -> anyhow::Result<()> {
    let row: Option<(Uuid, String, bool, Option<String>)> = sqlx::query_as(
        r#"
        select po.plan_video_id, po.status, po.media_deleted, vp.render_blob_path
          from posts po
          left join video_projects vp on vp.plan_video_id = po.plan_video_id
         where po.id = $1
        "#,
    )
    .bind(post_id)
    .fetch_optional(&state.db)
    .await?;
    let Some((plan_video_id, status, media_deleted, blob)) = row else {
        return Ok(());
    };
    if status != "published" || media_deleted {
        return Ok(());
    }
    if let Some(path) = blob {
        state.blob.delete(&path).await?;
        sqlx::query(
            "update video_projects set render_blob_path = null, render_bytes = null, \
                    updated_at = now() where plan_video_id = $1",
        )
        .bind(plan_video_id)
        .execute(&state.db)
        .await?;
    }
    sqlx::query("update posts set media_deleted = true where id = $1")
        .bind(post_id)
        .execute(&state.db)
        .await?;
    Ok(())
}

/// Record a terminal publish failure and close the plan when no post is left in flight.
pub async fn mark_post_failed(state: &AppState, post_id: Uuid, error: &str) {
    let plan_id: Option<Uuid> = sqlx::query_scalar(
        r#"
        update posts set status = 'failed', error = $2
         where id = $1 and status <> 'published'
        returning (select plan_id from plan_videos where id = posts.plan_video_id)
        "#,
    )
    .bind(post_id)
    .bind(error)
    .fetch_optional(&state.db)
    .await
    .unwrap_or_else(|e| {
        tracing::warn!(error = ?e, %post_id, "mark_post_failed update failed");
        None
    });
    if let Some(plan_id) = plan_id
        && let Err(e) = maybe_finish_plan(&state.db, plan_id).await
    {
        tracing::warn!(error = ?e, %plan_id, "maybe_finish_plan failed");
    }
}

pub async fn mark_connection_needs_reauth(
    db: &PgPool,
    project_id: Uuid,
    platform: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        update platform_connections
           set status = 'needs_reauth'
         where project_id = $1 and platform = $2
        "#,
    )
    .bind(project_id)
    .bind(platform)
    .execute(db)
    .await?;
    Ok(())
}

/// Map a Graph error into a job outcome (marking the connection when the token is dead).
pub async fn map_meta_err(state: &AppState, ctx: &PublishCtx, e: MetaError) -> JobError {
    match e.kind {
        MetaErrorKind::NeedsReauth => {
            let _ = mark_connection_needs_reauth(&state.db, ctx.project_id, &ctx.platform).await;
            JobError::permanent(anyhow::anyhow!("Reconnect Facebook: {}", e.message))
        }
        MetaErrorKind::IgPublishingLimit => {
            JobError::permanent(anyhow::anyhow!("Instagram daily publishing limit reached."))
        }
        MetaErrorKind::RateLimited => JobError::retry_after(
            e.retry_after_mins.max(1),
            anyhow::anyhow!("Meta rate limited: {}", e.message),
        ),
        MetaErrorKind::Retryable => JobError::Retry(anyhow::anyhow!("Meta error: {}", e.message)),
    }
}

pub fn parse_meta_ts(raw: Option<&str>) -> Option<DateTime<Utc>> {
    let s = raw?;
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            // Meta sometimes returns "+0000" instead of "+00:00"
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%z")
                .ok()
                .map(|n| n.and_utc())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_offsets_start_at_one_hour() {
        assert_eq!(SNAPSHOT_OFFSETS_H[0], 1);
        assert_eq!(SNAPSHOT_OFFSETS_H.len(), 12);
    }

    #[test]
    fn parses_meta_timestamps() {
        assert!(parse_meta_ts(Some("2026-01-02T03:04:05+0000")).is_some());
        assert!(parse_meta_ts(Some("2026-01-02T03:04:05+00:00")).is_some());
        assert!(parse_meta_ts(None).is_none());
    }
}
