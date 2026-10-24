//! App Store lookup (iTunes API) and screenshot scraping.

use std::collections::HashMap;
use std::sync::LazyLock;

use anyhow::{Context, bail};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::safe_fetch;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppStoreInfo {
    pub track_id: String,
    pub track_name: String,
    pub description: String,
    pub artwork_url: Option<String>,
    pub screenshot_urls: Vec<String>,
    pub genres: Vec<String>,
    pub seller_url: Option<String>,
}

static MZSTATIC_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"https://is\d*-ssl\.mzstatic\.com/image/thumb/[^\s"'<>,)\\]+"#)
        .expect("mzstatic regex")
});
static SIZE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/(\d+)x(\d+)[a-z]*\.(?:png|jpe?g|webp)$").expect("size regex"));

fn validate(app_id: &str, country: &str) -> anyhow::Result<()> {
    if app_id.is_empty() || !app_id.bytes().all(|b| b.is_ascii_digit()) {
        bail!("invalid app store id: {app_id}");
    }
    if country.len() != 2 || !country.bytes().all(|b| b.is_ascii_alphabetic()) {
        bail!("invalid country code: {country}");
    }
    Ok(())
}

pub async fn lookup(app_id: &str, country: &str) -> anyhow::Result<Option<AppStoreInfo>> {
    validate(app_id, country)?;
    let url = format!(
        "https://itunes.apple.com/lookup?id={app_id}&country={}&entity=software",
        country.to_ascii_lowercase()
    );
    let (_, body) = safe_fetch::fetch_text(&url).await?;
    parse_lookup(&body)
}

fn parse_lookup(body: &str) -> anyhow::Result<Option<AppStoreInfo>> {
    let v: Value = serde_json::from_str(body).context("parse itunes lookup json")?;
    let Some(r) = v
        .get("results")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
    else {
        return Ok(None);
    };
    let strs = |key: &str| -> Vec<String> {
        r.get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let s = |key: &str| r.get(key).and_then(Value::as_str).map(str::to_string);
    let Some(track_id) = r.get("trackId").and_then(Value::as_i64) else {
        return Ok(None);
    };
    Ok(Some(AppStoreInfo {
        track_id: track_id.to_string(),
        track_name: s("trackName").unwrap_or_default(),
        description: s("description").unwrap_or_default(),
        artwork_url: s("artworkUrl512").or_else(|| s("artworkUrl100")),
        screenshot_urls: strs("screenshotUrls"),
        genres: strs("genres"),
        seller_url: s("sellerUrl"),
    }))
}

/// Scrape the public App Store page for screenshot images. Prefers portrait, then widest.
pub async fn scrape_screenshot_urls(app_id: &str, country: &str) -> anyhow::Result<Vec<String>> {
    validate(app_id, country)?;
    let url = format!(
        "https://apps.apple.com/{}/app/id{app_id}",
        country.to_ascii_lowercase()
    );
    let (_, html) = safe_fetch::fetch_text(&url).await?;
    Ok(extract_screenshot_urls(&html))
}

fn extract_screenshot_urls(html: &str) -> Vec<String> {
    let html = html.replace("&amp;", "&");
    // base (url sans size segment) -> (width, height, url)
    let mut best: HashMap<String, (u32, u32, String)> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for m in MZSTATIC_RE.find_iter(&html) {
        let u = m.as_str();
        let lower = u.to_ascii_lowercase();
        if lower.contains("appicon") || lower.contains("/icon") {
            continue;
        }
        if !(lower.contains("purple") || lower.contains("screenshot") || lower.contains("source")) {
            continue;
        }
        let Some(c) = SIZE_RE.captures(u) else {
            continue;
        };
        let (w, h): (u32, u32) = (c[1].parse().unwrap_or(0), c[2].parse().unwrap_or(0));
        if w == 0 || h == 0 || w == h {
            // squares are almost always icons
            continue;
        }
        let base = u[..c.get(0).map_or(u.len(), |m| m.start())].to_string();
        match best.get(&base) {
            Some((bw, _, _)) if *bw >= w => {}
            _ => {
                if !best.contains_key(&base) {
                    order.push(base.clone());
                }
                best.insert(base, (w, h, u.to_string()));
            }
        }
    }

    let mut items: Vec<(usize, (u32, u32, String))> = order
        .iter()
        .enumerate()
        .filter_map(|(i, b)| best.remove(b).map(|v| (i, v)))
        .collect();
    // Portrait first, then widest, then page order.
    items.sort_by(|a, b| {
        let (pa, pb) = (a.1.1 > a.1.0, b.1.1 > b.1.0);
        pb.cmp(&pa).then(b.1.0.cmp(&a.1.0)).then(a.0.cmp(&b.0))
    });
    items
        .into_iter()
        .map(|(_, (_, _, u))| to_jpg(&u))
        .take(10)
        .collect()
}

/// The image crate here only decodes png/jpeg; mzstatic serves any format by extension.
fn to_jpg(u: &str) -> String {
    match u.rsplit_once('.') {
        Some((stem, "webp")) => format!("{stem}.jpg"),
        _ => u.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lookup() {
        let body = r#"{"resultCount":1,"results":[{"trackId":42,"trackName":"Acme",
"description":"d","artworkUrl512":"https://a/512.png","screenshotUrls":["https://s/1.png"],
"genres":["Health"],"sellerUrl":"https://acme.test"}]}"#;
        let i = parse_lookup(body).unwrap().unwrap();
        assert_eq!(i.track_id, "42");
        assert_eq!(i.genres, vec!["Health"]);
        assert!(
            parse_lookup(r#"{"resultCount":0,"results":[]}"#)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn extracts_screenshots_portrait_largest() {
        let html = r#"
<source srcset="https://is1-ssl.mzstatic.com/image/thumb/PurpleSource1/v4/aa/bb/one.png/230x498bb.webp 230w,
https://is1-ssl.mzstatic.com/image/thumb/PurpleSource1/v4/aa/bb/one.png/392x696bb.webp 392w">
<source srcset="https://is1-ssl.mzstatic.com/image/thumb/PurpleSource1/v4/cc/dd/land.png/643x392bb.webp 643w">
<img src="https://is1-ssl.mzstatic.com/image/thumb/Purple1/v4/ee/ff/AppIcon.png/512x512bb.jpg">"#;
        let out = extract_screenshot_urls(html);
        assert_eq!(out.len(), 2);
        assert!(out[0].ends_with("one.png/392x696bb.jpg"));
        assert!(out[1].contains("land.png/643x392bb"));
    }

    #[test]
    fn validates_inputs() {
        assert!(validate("123", "us").is_ok());
        assert!(validate("12a", "us").is_err());
        assert!(validate("123", "usa").is_err());
    }
}
