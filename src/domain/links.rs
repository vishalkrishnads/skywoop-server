//! Tracked links (`/r/{token}` → App Store campaign link) and caption helpers.

use sqlx::PgConnection;
use uuid::Uuid;

use crate::domain::Platform;
use crate::error::{AppError, AppResult};
use crate::tracking::token::campaign_token;

/// Append the tracked redirect URL for Facebook captions when missing.
pub fn append_fb_tracked_link(caption: &str, public_base: &str, token: &str) -> String {
    let base = public_base.trim_end_matches('/');
    let link = format!("{base}/r/{token}");
    if caption.contains(&link) {
        caption.to_owned()
    } else if caption.is_empty() {
        link
    } else {
        format!("{caption}\n\n{link}")
    }
}

/// Ensure Instagram captions mention a link-in-bio CTA.
pub fn ensure_instagram_bio_cta(caption: &str) -> String {
    let lower = caption.to_ascii_lowercase();
    if lower.contains("link in bio") || lower.contains("link in the bio") {
        caption.to_owned()
    } else if caption.is_empty() {
        "Link in bio.".into()
    } else {
        format!("{caption}\n\nLink in bio.")
    }
}

/// Final caption posted to the platform for a plan video.
pub fn final_caption(platform: Platform, caption: &str, public_base: &str, token: &str) -> String {
    match platform {
        Platform::Facebook => append_fb_tracked_link(caption, public_base, token),
        Platform::Instagram => ensure_instagram_bio_cta(caption),
    }
}

pub fn app_store_destination(app_id: &str, pt: &str, token: &str) -> String {
    format!("https://apps.apple.com/app/apple-store/id{app_id}?pt={pt}&ct={token}&mt=8")
}

/// Get or create the tracked link for a plan video; returns its token.
/// Requires the project to have an App Store app id.
pub async fn ensure_tracked_link(
    conn: &mut PgConnection,
    plan_video_id: Uuid,
    project_id: Uuid,
    platform: Platform,
) -> AppResult<String> {
    if let Some(t) =
        sqlx::query_scalar::<_, String>("select token from tracked_links where plan_video_id = $1")
            .bind(plan_video_id)
            .fetch_optional(&mut *conn)
            .await?
    {
        return Ok(t);
    }
    let project: (String, Option<String>, Option<String>) = sqlx::query_as(
        "select slug, app_store_app_id, asc_provider_token from projects where id = $1",
    )
    .bind(project_id)
    .fetch_one(&mut *conn)
    .await?;
    let app_id = project.1.filter(|s| !s.is_empty()).ok_or_else(|| {
        AppError::Conflict("Configure the App Store app id before scheduling.".into())
    })?;
    let pt = project.2.unwrap_or_default();

    for _ in 0..8 {
        let candidate = campaign_token(&project.0, platform);
        let dest = app_store_destination(&app_id, &pt, &candidate);
        // Savepoint so a token collision does not abort the surrounding transaction.
        let mut sp = sqlx::Connection::begin(&mut *conn).await?;
        let res = sqlx::query(
            "insert into tracked_links (token, plan_video_id, destination_url) values ($1,$2,$3)",
        )
        .bind(&candidate)
        .bind(plan_video_id)
        .bind(&dest)
        .execute(&mut *sp)
        .await;
        match res {
            Ok(_) => {
                sp.commit().await?;
                return Ok(candidate);
            }
            Err(e)
                if e.as_database_error()
                    .and_then(|d| d.constraint())
                    .is_some_and(|c| c.contains("token") || c.contains("tracked_links_pkey")) =>
            {
                sp.rollback().await?;
                continue;
            }
            Err(e) => return Err(AppError::from(e)),
        }
    }
    Err(AppError::Other(anyhow::anyhow!(
        "could not allocate campaign token"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fb_link_appended_once() {
        let c = append_fb_tracked_link("Hello", "http://localhost:8080", "tok");
        assert!(c.contains("http://localhost:8080/r/tok"));
        let again = append_fb_tracked_link(&c, "http://localhost:8080", "tok");
        assert_eq!(c.matches("/r/tok").count(), 1);
        assert_eq!(again, c);
    }

    #[test]
    fn ig_bio_cta() {
        assert!(ensure_instagram_bio_cta("Try it").contains("Link in bio"));
        let already = "See link in bio for more";
        assert_eq!(ensure_instagram_bio_cta(already), already);
    }

    #[test]
    fn destination_url() {
        let u = app_store_destination("123", "pt9", "camp-ig-abc");
        assert!(u.contains("id123"));
        assert!(u.contains("pt=pt9"));
        assert!(u.contains("ct=camp-ig-abc"));
    }
}
