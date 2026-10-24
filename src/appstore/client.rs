use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use reqwest::Client;
use serde_json::{Value, json};
use tokio::sync::RwLock;

use crate::config::Config;
use crate::secrets::Secrets;

use super::jwt::asc_jwt;

const JWT_CACHE_MINS: i64 = 15;

/// Thin App Store Connect client with JWT caching (15m).
pub struct AscClient {
    pub cfg: Arc<Config>,
    pub http: Client,
    cached_jwt: RwLock<Option<(String, DateTime<Utc>)>>,
}

impl AscClient {
    pub fn new(cfg: Arc<Config>) -> anyhow::Result<Arc<Self>> {
        Ok(Arc::new(Self {
            cfg,
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()?,
            cached_jwt: RwLock::new(None),
        }))
    }

    pub async fn clear_cached_jwt(&self) {
        *self.cached_jwt.write().await = None;
    }

    /// Return a cached ES256 JWT, refreshing when older than 15 minutes.
    pub async fn bearer_token(
        &self,
        secrets: &dyn Secrets,
        now: DateTime<Utc>,
    ) -> anyhow::Result<String> {
        {
            let guard = self.cached_jwt.read().await;
            if let Some((token, issued_at)) = guard.as_ref()
                && now - *issued_at < Duration::minutes(JWT_CACHE_MINS)
            {
                return Ok(token.clone());
            }
        }

        let pem = secrets.get(&self.cfg.asc_p8_secret_name).await?;
        let token = asc_jwt(
            &self.cfg.asc_key_id,
            &self.cfg.asc_issuer_id,
            pem.as_bytes(),
            now.timestamp(),
        )?;
        *self.cached_jwt.write().await = Some((token.clone(), now));
        Ok(token)
    }

    fn url(&self, path: &str) -> anyhow::Result<url::Url> {
        let base = self.cfg.asc_base.as_str().trim_end_matches('/');
        let path = path.trim_start_matches('/');
        Ok(url::Url::parse(&format!("{base}/{path}"))?)
    }

    pub async fn get_json(
        &self,
        path: &str,
        query: &[(&str, &str)],
        token: &str,
    ) -> anyhow::Result<(u16, Value)> {
        let mut url = self.url(path)?;
        {
            let mut q = url.query_pairs_mut();
            for (k, v) in query {
                q.append_pair(k, v);
            }
        }
        let resp = self
            .http
            .get(url)
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await?;
        let status = resp.status().as_u16();
        let text = resp.text().await?;
        let value: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({"raw": text}));
        Ok((status, value))
    }

    pub async fn post_json(
        &self,
        path: &str,
        body: &Value,
        token: &str,
    ) -> anyhow::Result<(u16, Value)> {
        let url = self.url(path)?;
        let resp = self
            .http
            .post(url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await?;
        let status = resp.status().as_u16();
        let text = resp.text().await?;
        let value: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({"raw": text}));
        Ok((status, value))
    }

    /// Download a presigned segment URL **without** the Authorization header.
    pub async fn download_bytes(&self, url: &str) -> anyhow::Result<Vec<u8>> {
        let resp = self.http.get(url).send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("segment download failed: http {}", resp.status());
        }
        Ok(resp.bytes().await?.to_vec())
    }

    /// Paginate `links.next` until exhausted; returns all `data` items.
    pub async fn get_all_data(
        &self,
        path: &str,
        query: &[(&str, &str)],
        token: &str,
    ) -> anyhow::Result<Vec<Value>> {
        let mut out = Vec::new();
        let (status, mut page) = self.get_json(path, query, token).await?;
        if !(200..300).contains(&status) {
            anyhow::bail!("ASC GET {path} failed: http {status}: {page}");
        }
        loop {
            if let Some(arr) = page.get("data").and_then(|d| d.as_array()) {
                out.extend(arr.iter().cloned());
            }
            let next = page
                .pointer("/links/next")
                .and_then(|n| n.as_str())
                .map(|s| s.to_owned());
            let Some(next_url) = next else { break };
            let resp = self
                .http
                .get(&next_url)
                .header("Authorization", format!("Bearer {token}"))
                .send()
                .await?;
            let status = resp.status().as_u16();
            let text = resp.text().await?;
            page = serde_json::from_str(&text).unwrap_or_else(|_| json!({"raw": text}));
            if !(200..300).contains(&status) {
                anyhow::bail!("ASC paginate failed: http {status}: {page}");
            }
        }
        Ok(out)
    }
}

/// Aggregated ASC daily metrics keyed by (date, campaign_or_source, metric).
pub type MetricAcc = HashMap<(NaiveDate, String, String), i64>;

#[derive(Debug, Default, Clone)]
pub struct AscAggregate {
    pub campaign: MetricAcc,
    pub source: MetricAcc,
}

/// Parse a gunzipped (or plain) App Store Analytics TSV segment into aggregates.
pub fn parse_asc_tsv(bytes: &[u8]) -> anyhow::Result<AscAggregate> {
    use anyhow::Context;
    use flate2::read::GzDecoder;
    use std::io::Read;

    let plain: Vec<u8> = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut dec = GzDecoder::new(bytes);
        let mut buf = Vec::new();
        dec.read_to_end(&mut buf).context("gunzip ASC segment")?;
        buf
    } else {
        bytes.to_vec()
    };

    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(b'\t')
        .flexible(true)
        .from_reader(plain.as_slice());
    let h = rdr.headers()?.clone();
    let col = |name: &str| h.iter().position(|x| x.eq_ignore_ascii_case(name));
    let date_i = col("Date").context("Date column required")?;
    let camp_i = col("Campaign");
    let src_i = col("Source Info");
    let counts_i = col("Counts").context("Counts column required")?;
    let event_i = col("Event");
    let dl_type_i = col("Download Type");

    let mut out = AscAggregate::default();
    for rec in rdr.records() {
        let rec = rec?;
        let metric = match (
            event_i.map(|i| rec.get(i).unwrap_or("").to_ascii_lowercase()),
            dl_type_i.map(|i| rec.get(i).unwrap_or("").to_ascii_lowercase()),
        ) {
            (Some(e), _) if e == "impression" => "impressions",
            (Some(e), _) if e == "page view" => "page_views",
            (Some(e), _) if e == "tap" => "taps",
            (_, Some(d)) if d == "first-time download" => "first_time_downloads",
            (_, Some(d)) if d == "redownload" => "redownloads",
            _ => continue,
        };
        let date_str = rec.get(date_i).unwrap_or("");
        let date = NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
            .with_context(|| format!("bad Date: {date_str}"))?;
        let n: i64 = rec.get(counts_i).unwrap_or("0").parse().unwrap_or(0);
        match camp_i
            .map(|i| rec.get(i).unwrap_or("").trim())
            .filter(|c| !c.is_empty())
        {
            Some(c) => {
                *out.campaign
                    .entry((date, c.to_owned(), metric.to_owned()))
                    .or_default() += n;
            }
            None => {
                if let Some(src) = src_i
                    .map(|i| rec.get(i).unwrap_or("").trim())
                    .filter(|s| !s.is_empty())
                {
                    *out.source
                        .entry((date, src.to_owned(), metric.to_owned()))
                        .or_default() += n;
                }
            }
        }
    }
    Ok(out)
}

/// Merge another aggregate into `self` (multi-segment).
impl AscAggregate {
    pub fn merge(&mut self, other: AscAggregate) {
        for (k, v) in other.campaign {
            *self.campaign.entry(k).or_default() += v;
        }
        for (k, v) in other.source {
            *self.source.entry(k).or_default() += v;
        }
    }
}

/// Whether an incoming processing_date should overwrite an existing row.
pub fn should_overwrite(existing: NaiveDate, incoming: NaiveDate) -> bool {
    incoming >= existing
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    fn fixture(name: &str) -> Vec<u8> {
        let path = format!("{}/fixtures/asc/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read(path).expect("fixture")
    }

    #[test]
    fn parses_engagement_campaign_and_source() {
        let agg = parse_asc_tsv(&fixture("engagement_detailed.txt")).unwrap();
        let d = NaiveDate::from_ymd_opt(2024, 1, 15).unwrap();
        assert_eq!(
            agg.campaign
                .get(&(d, "tok_abc".into(), "impressions".into())),
            Some(&100)
        );
        assert_eq!(
            agg.campaign
                .get(&(d, "tok_abc".into(), "page_views".into())),
            Some(&50)
        );
        assert_eq!(
            agg.source
                .get(&(d, "Instagram".into(), "impressions".into())),
            Some(&200)
        );
        assert!(!agg.campaign.keys().any(|(_, c, _)| c.is_empty()));
    }

    #[test]
    fn parses_downloads_fixture() {
        let agg = parse_asc_tsv(&fixture("downloads_detailed.txt")).unwrap();
        let d = NaiveDate::from_ymd_opt(2024, 1, 15).unwrap();
        assert_eq!(
            agg.campaign
                .get(&(d, "tok_abc".into(), "first_time_downloads".into())),
            Some(&5)
        );
        assert_eq!(
            agg.source
                .get(&(d, "Instagram".into(), "first_time_downloads".into())),
            Some(&20)
        );
    }

    #[test]
    fn multi_segment_aggregation() {
        let mut a = parse_asc_tsv(&fixture("engagement_detailed.txt")).unwrap();
        let b = parse_asc_tsv(&fixture("downloads_detailed.txt")).unwrap();
        a.merge(b);
        let d = NaiveDate::from_ymd_opt(2024, 1, 15).unwrap();
        assert_eq!(
            a.campaign.get(&(d, "tok_abc".into(), "impressions".into())),
            Some(&100)
        );
        assert_eq!(
            a.campaign
                .get(&(d, "tok_abc".into(), "first_time_downloads".into())),
            Some(&5)
        );
        // Same key across two engagement-like segments would sum:
        let seg1 = b"Date\tEvent\tCampaign\tSource Info\tCounts\n2024-01-15\tImpression\ttok_a\tInstagram\t10\n";
        let seg2 = b"Date\tEvent\tCampaign\tSource Info\tCounts\n2024-01-15\tImpression\ttok_a\tInstagram\t7\n";
        let mut m = parse_asc_tsv(seg1).unwrap();
        m.merge(parse_asc_tsv(seg2).unwrap());
        assert_eq!(
            m.campaign.get(&(d, "tok_a".into(), "impressions".into())),
            Some(&17)
        );
    }

    #[test]
    fn gzipped_segment_parses() {
        let plain = fixture("engagement_detailed.txt");
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&plain).unwrap();
        let gz = enc.finish().unwrap();
        let agg = parse_asc_tsv(&gz).unwrap();
        assert!(!agg.campaign.is_empty());
    }

    #[test]
    fn overwrite_semantics() {
        let older = NaiveDate::from_ymd_opt(2024, 1, 10).unwrap();
        let newer = NaiveDate::from_ymd_opt(2024, 1, 12).unwrap();
        assert!(should_overwrite(older, newer));
        assert!(should_overwrite(newer, newer));
        assert!(!should_overwrite(newer, older));
    }

    #[test]
    fn rows_without_campaign_go_to_source() {
        let tsv =
            b"Date\tEvent\tCampaign\tSource Info\tCounts\n2024-02-01\tImpression\t\tFacebook\t33\n";
        let agg = parse_asc_tsv(tsv).unwrap();
        assert!(agg.campaign.is_empty());
        assert_eq!(
            agg.source.get(&(
                NaiveDate::from_ymd_opt(2024, 2, 1).unwrap(),
                "Facebook".into(),
                "impressions".into()
            )),
            Some(&33)
        );
    }
}
