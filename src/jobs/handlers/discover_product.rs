//! `DiscoverProduct`: crawl a product URL / App Store listing into a project draft.
//!
//! Discoveries are not tied to a project yet, so preview bytes live in blob storage under
//! `discoveries/{id}/...` and the draft only keeps their paths. Project creation copies
//! them into real `assets` rows (under `App Store/`). Image descriptions are filled in after
//! the draft is marked ready, so onboarding is not blocked on a vision call per screenshot.

use std::sync::Arc;
use std::sync::LazyLock;

use futures_util::future::join_all;
use regex::Regex;
use serde_json::{Value, json};
use url::Url;
use uuid::Uuid;

use crate::ai::{planner, vision};
use crate::crawl::{self, AppStoreInfo, SiteSummary};
use crate::domain::types::{normalize_color, normalize_font_family};
use crate::jobs::JobError;
use crate::state::AppState;

const MAX_SCREENSHOTS: usize = 6;

static APP_ID_IN_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/id(\d+)").expect("app id in url regex"));

pub async fn run(state: &AppState, discovery_id: Uuid) -> Result<(), JobError> {
    let row: Option<(String,)> =
        sqlx::query_as("select url from project_discoveries where id = $1")
            .bind(discovery_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| JobError::Retry(e.into()))?;
    let Some((url,)) = row else {
        return Err(JobError::permanent(anyhow::anyhow!(
            "discovery {discovery_id} not found"
        )));
    };

    sqlx::query("update project_discoveries set status = 'running', error = null where id = $1")
        .bind(discovery_id)
        .execute(&state.db)
        .await
        .map_err(|e| JobError::Retry(e.into()))?;

    match discover(state, discovery_id, &url).await {
        Ok((draft, pending)) => {
            sqlx::query(
                "update project_discoveries set status = 'ready', draft = $2, error = null \
                 where id = $1",
            )
            .bind(discovery_id)
            .bind(sqlx::types::Json(&draft))
            .execute(&state.db)
            .await
            .map_err(|e| JobError::Retry(e.into()))?;
            // The form is already unblocked. Descriptions are optional search text;
            // a slow vision call must not keep the operator on "Looking up…".
            attach_descriptions(state, discovery_id, draft, pending).await;
            Ok(())
        }
        Err(e) => {
            let msg = format!("{e:#}");
            tracing::warn!(%discovery_id, error = %msg, "product discovery failed");
            let _ = sqlx::query(
                "update project_discoveries set status = 'failed', error = $2 where id = $1",
            )
            .bind(discovery_id)
            .bind(&msg)
            .execute(&state.db)
            .await;
            Err(e)
        }
    }
}

fn app_store_url_parts(url: &str) -> Option<(String, Option<String>)> {
    let parsed = Url::parse(url).ok()?;
    if parsed.host_str()? != "apps.apple.com" {
        return None;
    }
    let id = APP_ID_IN_URL.captures(parsed.path())?[1].to_string();
    let country = parsed
        .path_segments()
        .and_then(|mut s| s.next())
        .filter(|c| c.len() == 2 && c.bytes().all(|b| b.is_ascii_alphabetic()))
        .map(|c| c.to_ascii_uppercase());
    Some((id, country))
}

struct PendingPreview {
    blob_path: String,
    kind: String,
    bytes: Vec<u8>,
}

async fn discover(
    state: &AppState,
    discovery_id: Uuid,
    url: &str,
) -> Result<(Value, Vec<PendingPreview>), JobError> {
    let (final_url, html) = crawl::fetch_text(url).await.map_err(JobError::permanent)?;
    let site: SiteSummary = crawl::parse_html(&final_url, &html);

    // App Store id: explicit apps.apple.com URL wins, else the first id found on the page.
    let from_url = app_store_url_parts(url).or_else(|| app_store_url_parts(&final_url));
    let (app_id, country) = match &from_url {
        Some((id, country)) => (Some(id.clone()), country.clone()),
        None => (site.app_store_ids.first().cloned(), None),
    };
    let country = country.unwrap_or_else(|| "US".into());

    let mut app_store: Option<AppStoreInfo> = None;
    if let Some(id) = &app_id {
        match crawl::lookup_app_store(id, &country).await {
            Ok(info) => app_store = info,
            Err(e) => tracing::warn!(error = %e, app_id = %id, "app store lookup failed"),
        }
        if let Some(info) = app_store.as_mut()
            && info.screenshot_urls.is_empty()
        {
            match crawl::scrape_screenshot_urls(id, &country).await {
                Ok(urls) => info.screenshot_urls = urls,
                Err(e) => tracing::warn!(error = %e, app_id = %id, "screenshot scrape failed"),
            }
        }
    }

    // Download icon + screenshots together. Vision descriptions happen after the
    // draft is ready (see `attach_descriptions`).
    let shot_urls: Vec<String> = app_store
        .as_ref()
        .map(|a| {
            a.screenshot_urls
                .iter()
                .take(MAX_SCREENSHOTS)
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let mut downloads = Vec::new();
    if let Some(art) = app_store.as_ref().and_then(|a| a.artwork_url.clone()) {
        downloads.push(download_preview(
            state,
            discovery_id,
            "app_icon",
            "icon".to_string(),
            art,
        ));
    }
    for (i, u) in shot_urls.into_iter().enumerate() {
        downloads.push(download_preview(
            state,
            discovery_id,
            "screenshot",
            format!("screenshot-{i}"),
            u,
        ));
    }
    let downloaded = join_all(downloads).await;
    let mut previews: Vec<Value> = Vec::new();
    let mut pending: Vec<PendingPreview> = Vec::new();
    let mut icon_bytes: Option<Vec<u8>> = None;
    let mut first_shot: Option<Vec<u8>> = None;
    for item in downloaded.into_iter().flatten() {
        let (bytes, entry) = item;
        let kind = entry
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let blob_path = entry
            .get("blob_path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if kind == "app_icon" {
            icon_bytes.get_or_insert_with(|| bytes.clone());
        }
        if kind == "screenshot" {
            first_shot.get_or_insert_with(|| bytes.clone());
        }
        if !blob_path.is_empty() {
            pending.push(PendingPreview {
                blob_path,
                kind,
                bytes,
            });
        }
        previews.push(entry);
    }

    // Palette from the icon (or first screenshot); fall back to the site's theme color.
    let mut palette: Vec<String> = icon_bytes
        .as_deref()
        .or(first_shot.as_deref())
        .and_then(|b| crawl::dominant_colors(b, 3).ok())
        .unwrap_or_default();
    if palette.is_empty()
        && let Some(t) = &site.theme_color
    {
        palette.push(t.clone());
    }

    let site_json = serde_json::to_value(&site).map_err(JobError::permanent)?;
    let app_json = app_store
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .map_err(JobError::permanent)?;
    let out = planner::discover_product(state.ai.as_ref(), &site_json, app_json.as_ref(), &palette)
        .await
        .map_err(JobError::from_ai)?;

    // Model output is advisory: bad colors/fonts become null rather than failing discovery.
    let color = |c: Option<String>| normalize_color(c).ok().flatten();
    let font = normalize_font_family(out.font_family.clone())
        .ok()
        .flatten();

    let app_store_app_id = out
        .app_store_app_id
        .clone()
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .or_else(|| app_store.as_ref().map(|a| a.track_id.clone()))
        .or(app_id);

    let on_app_store_host = Url::parse(&final_url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h == "apps.apple.com"))
        .unwrap_or(false);
    let website = if on_app_store_host {
        app_store.as_ref().and_then(|a| a.seller_url.clone())
    } else {
        Some(final_url.clone())
    };

    let name = if out.name.trim().is_empty() {
        app_store
            .as_ref()
            .map(|a| a.track_name.clone())
            .or(site.title.clone())
            .unwrap_or_default()
    } else {
        out.name.trim().to_string()
    };

    let mut suggested: Vec<serde_json::Value> = out
        .suggested_audiences
        .iter()
        .map(|a| {
            json!({
                "name": a.name.trim(),
                "description": a.description.trim(),
            })
        })
        .filter(|a| a["name"].as_str().is_some_and(|n| !n.is_empty()))
        .collect();
    if suggested.len() < 5 {
        let category = app_store
            .as_ref()
            .map(|a| a.track_name.as_str())
            .or(site.title.as_deref())
            .unwrap_or("this product");
        let templates = [
            (
                "Power users",
                "People who rely on the product daily and optimize their workflow.",
            ),
            (
                "New adopters",
                "First-time users comparing options and learning the category.",
            ),
            (
                "Budget-conscious shoppers",
                "Price-sensitive buyers looking for clear value.",
            ),
            (
                "Premium seekers",
                "Users who want the best experience and are willing to pay for it.",
            ),
            (
                "Local / mobile-first users",
                "On-the-go users who discover products on social feeds.",
            ),
        ];
        for (i, (n, d)) in templates.iter().enumerate() {
            if suggested.len() >= 5 {
                break;
            }
            if suggested.iter().any(|x| x["name"] == json!(n)) {
                continue;
            }
            let desc = if i == 0 {
                format!("{d} Relevant for {category}.")
            } else {
                (*d).to_string()
            };
            suggested.push(json!({ "name": n, "description": desc }));
        }
        while suggested.len() < 5 {
            let n = suggested.len() + 1;
            suggested.push(json!({
                "name": format!("Audience segment {n}"),
                "description": format!("Potential customers interested in {category}."),
            }));
        }
        suggested.truncate(5);
    }

    Ok((
        json!({
            "name": name,
            "description": out.description,
            "website": website,
            "app_store_app_id": app_store_app_id,
            "market_country": country,
            "primary_color": color(out.primary_color),
            "secondary_color": color(out.secondary_color),
            "tertiary_color": color(out.tertiary_color),
            "font_family": font,
            "preview_assets": previews,
            "suggested_audiences": suggested,
        }),
        pending,
    ))
}

/// Describe stored previews in parallel and write the text back onto the ready draft.
async fn attach_descriptions(
    state: &AppState,
    discovery_id: Uuid,
    mut draft: Value,
    pending: Vec<PendingPreview>,
) {
    if pending.is_empty() {
        return;
    }
    let described = join_all(pending.into_iter().map(|p| {
        let ai = Arc::clone(&state.ai);
        async move {
            let context = if p.kind == "screenshot" {
                "App Store listing screenshot"
            } else {
                "App icon from the App Store listing"
            };
            let description = vision::describe_bytes(&ai, &p.bytes, context).await;
            (p.blob_path, description)
        }
    }))
    .await;

    let Some(assets) = draft
        .get_mut("preview_assets")
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for (path, description) in described {
        let Some(description) = description else {
            continue;
        };
        let Some(asset) = assets
            .iter_mut()
            .find(|a| a.get("blob_path").and_then(Value::as_str) == Some(path.as_str()))
        else {
            continue;
        };
        let Some(obj) = asset.as_object_mut() else {
            continue;
        };
        obj.insert("description".into(), json!(description.description));
        obj.insert("tags".into(), json!(description.tags));
    }

    if let Err(e) =
        sqlx::query("update project_discoveries set draft = $2 where id = $1 and status = 'ready'")
            .bind(discovery_id)
            .bind(sqlx::types::Json(&draft))
            .execute(&state.db)
            .await
    {
        tracing::warn!(%discovery_id, error = %e, "could not store preview descriptions");
    }
}

fn mime_and_ext(content_type: &str, bytes: &[u8]) -> (&'static str, &'static str) {
    match image::guess_format(bytes) {
        Ok(image::ImageFormat::Png) => return ("image/png", "png"),
        Ok(image::ImageFormat::Jpeg) => return ("image/jpeg", "jpg"),
        Ok(image::ImageFormat::WebP) => return ("image/webp", "webp"),
        Ok(image::ImageFormat::Gif) => return ("image/gif", "gif"),
        _ => {}
    }
    let ct = content_type.to_ascii_lowercase();
    if ct.contains("png") {
        ("image/png", "png")
    } else if ct.contains("webp") {
        ("image/webp", "webp")
    } else if ct.contains("gif") {
        ("image/gif", "gif")
    } else {
        ("image/jpeg", "jpg")
    }
}

/// Fetch one image and stash it under `discoveries/{id}/{stem}.{ext}`; returns bytes + draft entry.
async fn download_preview(
    state: &AppState,
    discovery_id: Uuid,
    kind: &str,
    stem: String,
    url: String,
) -> Option<(Vec<u8>, Value)> {
    let (_, bytes, content_type) = match crawl::fetch_bytes(&url).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, %url, kind, "preview download failed");
            return None;
        }
    };
    if bytes.is_empty() {
        return None;
    }
    let (mime, ext) = mime_and_ext(&content_type, &bytes);
    let (width, height) = match image::load_from_memory(&bytes) {
        Ok(img) => (Some(img.width()), Some(img.height())),
        Err(_) => (None, None),
    };
    let blob_path = format!("discoveries/{discovery_id}/{stem}.{ext}");
    if let Err(e) = state.blob.put(&blob_path, bytes.clone(), mime).await {
        tracing::warn!(error = %e, %blob_path, "preview store failed");
        return None;
    }
    let mut entry = json!({ "kind": kind, "blob_path": blob_path, "mime": mime });
    if let (Some(w), Some(h)) = (width, height) {
        entry["width"] = json!(w);
        entry["height"] = json!(h);
    }
    Some((bytes, entry))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_app_store_urls() {
        assert_eq!(
            app_store_url_parts("https://apps.apple.com/gb/app/foo/id12345?mt=8"),
            Some(("12345".into(), Some("GB".into())))
        );
        assert_eq!(
            app_store_url_parts("https://apps.apple.com/app/id999"),
            Some(("999".into(), None))
        );
        assert_eq!(app_store_url_parts("https://example.com/id123"), None);
    }
}
