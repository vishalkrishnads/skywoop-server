//! Google Trends daily RSS.

use anyhow::{Context, bail};
use quick_xml::Reader;
use quick_xml::events::Event;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleTrendItem {
    pub id: String,
    pub title: String,
    pub traffic: Option<String>,
    pub news_titles: Vec<String>,
    pub source_urls: Vec<String>,
}

const MAX_ID_LEN: usize = 48;

pub async fn fetch_daily(country: &str) -> anyhow::Result<Vec<GoogleTrendItem>> {
    if country.len() != 2 || !country.bytes().all(|b| b.is_ascii_alphabetic()) {
        bail!("invalid country code: {country}");
    }
    let url = format!(
        "https://trends.google.com/trending/rss?geo={}",
        country.to_ascii_uppercase()
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("SkywoopBot/1.0")
        .build()
        .context("build http client")?;
    let resp = client
        .get(&url)
        .send()
        .await
        .context("fetch google trends rss")?;
    if !resp.status().is_success() {
        bail!("google trends rss returned {}", resp.status());
    }
    let body = resp.text().await.context("read google trends rss")?;
    parse_rss(&body)
}

/// snake_case slug: lowercase alphanumerics joined by single underscores, truncated.
pub fn slugify(title: &str) -> String {
    let mut out = String::new();
    for ch in title.chars() {
        if ch.is_alphanumeric() {
            out.extend(ch.to_lowercase());
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
    }
    let mut out: String = out.chars().take(MAX_ID_LEN).collect();
    while out.ends_with('_') {
        out.pop();
    }
    out
}

pub fn parse_rss(xml: &str) -> anyhow::Result<Vec<GoogleTrendItem>> {
    let mut reader = Reader::from_str(xml);
    let mut items = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut cur: Option<GoogleTrendItem> = None;

    loop {
        match reader.read_event().context("parse google trends xml")? {
            Event::Start(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if name == "item" {
                    cur = Some(GoogleTrendItem {
                        id: String::new(),
                        title: String::new(),
                        traffic: None,
                        news_titles: vec![],
                        source_urls: vec![],
                    });
                }
                stack.push(name);
            }
            Event::End(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                stack.pop();
                if name == "item"
                    && let Some(mut it) = cur.take()
                {
                    it.id = slugify(&it.title);
                    if !it.title.is_empty() && !it.id.is_empty() {
                        items.push(it);
                    }
                }
            }
            Event::Text(t) => {
                let text = t
                    .unescape()
                    .context("unescape xml text")?
                    .trim()
                    .to_string();
                apply_text(&stack, cur.as_mut(), text);
            }
            Event::CData(t) => {
                let text = String::from_utf8_lossy(&t.into_inner()).trim().to_string();
                apply_text(&stack, cur.as_mut(), text);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(items)
}

fn apply_text(stack: &[String], cur: Option<&mut GoogleTrendItem>, text: String) {
    let (Some(it), Some(leaf)) = (cur, stack.last()) else {
        return;
    };
    if text.is_empty() {
        return;
    }
    // Only direct children of <item> (title, traffic) and news_item fields.
    let parent = stack.len().checked_sub(2).map(|i| stack[i].as_str());
    match (leaf.as_str(), parent) {
        ("title", Some("item")) => it.title.push_str(&text),
        ("ht:approx_traffic", Some("item")) => it.traffic = Some(text),
        ("ht:news_item_title", Some("ht:news_item")) => it.news_titles.push(text),
        ("ht:news_item_url", Some("ht:news_item")) => it.source_urls.push(text),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../fixtures/trends/google_rss_sample.xml");

    #[test]
    fn parses_fixture() {
        let items = parse_rss(FIXTURE).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].title, "Super Bowl halftime show");
        assert_eq!(items[0].id, "super_bowl_halftime_show");
        assert_eq!(items[0].traffic.as_deref(), Some("500K+"));
        assert_eq!(
            items[0].news_titles,
            vec![
                "Halftime performer announced & fans react".to_string(),
                "What to expect at the show".to_string()
            ]
        );
        assert_eq!(items[0].source_urls.len(), 2);
        assert_eq!(items[1].id, "pumpkin_spice_latte");
        assert_eq!(items[2].id, "daylight_saving_time_2026");
        assert!(items[2].news_titles.is_empty());
    }

    #[test]
    fn slug_truncates() {
        let s = slugify(&"ab ".repeat(40));
        assert!(s.len() <= MAX_ID_LEN);
        assert!(!s.ends_with('_'));
    }

    #[test]
    fn channel_title_is_not_an_item() {
        let items = parse_rss("<rss><channel><title>x</title></channel></rss>").unwrap();
        assert!(items.is_empty());
    }
}
