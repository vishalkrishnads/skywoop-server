//! Lightweight HTML summary used by product discovery.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SiteSummary {
    pub url: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub og: HashMap<String, String>,
    pub json_ld: Vec<Value>,
    pub theme_color: Option<String>,
    pub font_families: Vec<String>,
    pub app_store_urls: Vec<String>,
    pub app_store_ids: Vec<String>,
}

static FONT_FAMILY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)font-family\s*:\s*([^;}]+)"#).expect("font-family regex"));
static APP_ID_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"id(\d+)").expect("app id regex"));
static BANNER_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"app-id=(\d+)").expect("banner id regex"));

const GENERIC_FONTS: &[&str] = &[
    "serif",
    "sans-serif",
    "monospace",
    "cursive",
    "fantasy",
    "system-ui",
    "ui-serif",
    "ui-sans-serif",
    "ui-monospace",
    "ui-rounded",
    "emoji",
    "math",
    "fangsong",
    "inherit",
    "initial",
    "unset",
    "revert",
    "-apple-system",
    "blinkmacsystemfont",
];

fn sel(s: &str) -> Selector {
    Selector::parse(s).expect("static selector")
}

fn push_unique(v: &mut Vec<String>, s: String) {
    if !s.is_empty() && !v.contains(&s) {
        v.push(s);
    }
}

fn clean(s: &str) -> Option<String> {
    let t = s.split_whitespace().collect::<Vec<_>>().join(" ");
    (!t.is_empty()).then_some(t)
}

pub fn parse_html(url: &str, html: &str) -> SiteSummary {
    let doc = Html::parse_document(html);
    let mut out = SiteSummary {
        url: url.to_string(),
        ..Default::default()
    };

    out.title = doc
        .select(&sel("title"))
        .next()
        .and_then(|e| clean(&e.text().collect::<String>()));

    for el in doc.select(&sel("meta")) {
        let v = el.value();
        let content = v.attr("content").unwrap_or("").trim();
        if content.is_empty() {
            continue;
        }
        if let Some(name) = v.attr("name") {
            match name.to_ascii_lowercase().as_str() {
                "description" if out.description.is_none() => {
                    out.description = clean(content);
                }
                "theme-color" if out.theme_color.is_none() => {
                    out.theme_color = Some(content.to_string());
                }
                "apple-itunes-app" => {
                    for c in BANNER_ID_RE.captures_iter(content) {
                        push_unique(&mut out.app_store_ids, c[1].to_string());
                    }
                }
                _ => {}
            }
        }
        if let Some(prop) = v.attr("property")
            && let Some(key) = prop.strip_prefix("og:")
        {
            out.og
                .entry(key.to_string())
                .or_insert_with(|| content.to_string());
        }
    }

    for el in doc.select(&sel(r#"script[type="application/ld+json"]"#)) {
        let raw: String = el.text().collect();
        if let Ok(val) = serde_json::from_str::<Value>(raw.trim()) {
            out.json_ld.push(val);
        }
    }

    for el in doc.select(&sel("link[href]")) {
        let href = el.value().attr("href").unwrap_or("");
        if href.contains("fonts.googleapis.com") {
            for f in google_font_families(href) {
                push_unique(&mut out.font_families, f);
            }
        }
    }

    for el in doc.select(&sel("[style]")) {
        let style = el.value().attr("style").unwrap_or("");
        for f in inline_font_families(style) {
            push_unique(&mut out.font_families, f);
        }
    }
    for el in doc.select(&sel("style")) {
        let css: String = el.text().collect();
        for f in inline_font_families(&css) {
            push_unique(&mut out.font_families, f);
        }
    }

    for el in doc.select(&sel("a[href]")) {
        let href = el.value().attr("href").unwrap_or("").trim();
        if href.contains("apps.apple.com") || href.contains("itunes.apple.com") {
            push_unique(&mut out.app_store_urls, href.to_string());
            if let Some(c) = APP_ID_RE.captures(href) {
                push_unique(&mut out.app_store_ids, c[1].to_string());
            }
        }
    }

    out
}

/// Extract family names from a Google Fonts css/css2 URL (`family=Inter:wght@400&family=Lora`).
fn google_font_families(href: &str) -> Vec<String> {
    let href = href.replace("&amp;", "&");
    let full = if href.starts_with("//") {
        format!("https:{href}")
    } else {
        href
    };
    let Ok(u) = url::Url::parse(&full) else {
        return vec![];
    };
    let mut out = Vec::new();
    for (k, v) in u.query_pairs() {
        if k != "family" {
            continue;
        }
        // css (v1) joins families with `|`; css2 uses repeated params.
        for part in v.split('|') {
            let name = part.split(':').next().unwrap_or("").trim();
            if !name.is_empty() {
                push_unique(&mut out, name.to_string());
            }
        }
    }
    out
}

fn inline_font_families(css: &str) -> Vec<String> {
    let mut out = Vec::new();
    for c in FONT_FAMILY_RE.captures_iter(css) {
        for part in c[1].split(',') {
            let name = part.trim().trim_matches(['"', '\'']).trim();
            if name.is_empty()
                || name.starts_with("var(")
                || GENERIC_FONTS.contains(&name.to_ascii_lowercase().as_str())
            {
                continue;
            }
            push_unique(&mut out, name.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const HTML: &str = r##"<html><head>
<title> Acme  App </title>
<meta name="description" content="Track habits">
<meta name="theme-color" content="#112233">
<meta property="og:title" content="Acme">
<meta property="og:image" content="https://x.test/i.png">
<link href="https://fonts.googleapis.com/css2?family=Inter:wght@400;700&amp;family=Playfair+Display&display=swap" rel="stylesheet">
<script type="application/ld+json">{"@type":"SoftwareApplication","name":"Acme"}</script>
<style>body{font-family: "Poppins", sans-serif}</style>
</head><body style="font-family: 'DM Sans', Arial, sans-serif">
<a href="https://apps.apple.com/us/app/acme/id123456789">Get</a>
</body></html>"##;

    #[test]
    fn parses_summary() {
        let s = parse_html("https://acme.test", HTML);
        assert_eq!(s.title.as_deref(), Some("Acme App"));
        assert_eq!(s.description.as_deref(), Some("Track habits"));
        assert_eq!(s.theme_color.as_deref(), Some("#112233"));
        assert_eq!(s.og.get("title").map(String::as_str), Some("Acme"));
        assert_eq!(s.json_ld.len(), 1);
        assert!(s.font_families.contains(&"Inter".to_string()));
        assert!(s.font_families.contains(&"Playfair Display".to_string()));
        assert!(s.font_families.contains(&"Poppins".to_string()));
        assert!(s.font_families.contains(&"DM Sans".to_string()));
        assert!(s.font_families.contains(&"Arial".to_string()));
        assert_eq!(s.app_store_ids, vec!["123456789".to_string()]);
        assert_eq!(s.app_store_urls.len(), 1);
    }
}
