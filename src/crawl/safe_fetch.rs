//! SSRF-resistant HTTP fetching: http/https only, public IPs only (re-checked on every
//! redirect), bounded redirects, body size, and total time.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use reqwest::header::{CONTENT_TYPE, LOCATION};
use url::Url;

const MAX_REDIRECTS: usize = 5;
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10);
const USER_AGENT: &str = "SkywoopBot/1.0";

/// Fetch a URL as text. Returns `(final_url, body)`.
pub async fn fetch_text(url: &str) -> anyhow::Result<(String, String)> {
    let (final_url, bytes, _) = fetch_bytes(url).await?;
    Ok((final_url, String::from_utf8_lossy(&bytes).into_owned()))
}

/// Fetch a URL as bytes. Returns `(final_url, bytes, content_type)`.
pub async fn fetch_bytes(url: &str) -> anyhow::Result<(String, Vec<u8>, String)> {
    tokio::time::timeout(TIMEOUT, fetch_inner(url))
        .await
        .map_err(|_| anyhow!("fetch timed out after {}s: {url}", TIMEOUT.as_secs()))?
}

async fn fetch_inner(url: &str) -> anyhow::Result<(String, Vec<u8>, String)> {
    let mut current = Url::parse(url).with_context(|| format!("invalid url: {url}"))?;

    for _ in 0..=MAX_REDIRECTS {
        let addr = resolve_public(&current).await?;
        let host = current
            .host_str()
            .ok_or_else(|| anyhow!("url has no host"))?
            .to_string();

        // Pin the connection to the address we validated (prevents DNS rebinding).
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(TIMEOUT)
            .user_agent(USER_AGENT)
            .resolve(host.trim_matches(['[', ']']), addr)
            .build()
            .context("build http client")?;

        let mut resp = client
            .get(current.clone())
            .send()
            .await
            .with_context(|| format!("request failed: {current}"))?;

        let status = resp.status();
        if status.is_redirection() {
            let loc = resp
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow!("redirect without Location header"))?;
            current = current
                .join(loc)
                .with_context(|| format!("invalid redirect target: {loc}"))?;
            continue;
        }
        if !status.is_success() {
            bail!("upstream returned {status} for {current}");
        }

        let content_type = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        if resp
            .content_length()
            .is_some_and(|l| l > MAX_BODY_BYTES as u64)
        {
            bail!("response body exceeds {MAX_BODY_BYTES} bytes");
        }
        let mut body: Vec<u8> = Vec::new();
        while let Some(chunk) = resp.chunk().await.context("read response body")? {
            if body.len() + chunk.len() > MAX_BODY_BYTES {
                bail!("response body exceeds {MAX_BODY_BYTES} bytes");
            }
            body.extend_from_slice(&chunk);
        }
        return Ok((current.to_string(), body, content_type));
    }
    bail!("too many redirects (max {MAX_REDIRECTS})")
}

/// Validate scheme and resolve DNS; every resolved address must be public.
async fn resolve_public(url: &Url) -> anyhow::Result<SocketAddr> {
    match url.scheme() {
        "http" | "https" => {}
        other => bail!("unsupported url scheme: {other}"),
    }
    let host = url.host_str().ok_or_else(|| anyhow!("url has no host"))?;
    let host = host.trim_matches(['[', ']']);
    let port = url
        .port_or_known_default()
        .ok_or_else(|| anyhow!("url has no port"))?;

    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .with_context(|| format!("dns lookup failed for {host}"))?
        .collect();
    if addrs.is_empty() {
        bail!("dns lookup returned no addresses for {host}");
    }
    if let Some(bad) = addrs.iter().find(|a| is_blocked_ip(a.ip())) {
        bail!("blocked address {} for host {host}", bad.ip());
    }
    Ok(addrs[0])
}

/// True for loopback, private, link-local, unspecified, multicast, and other non-public IPs.
fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_v4(v4),
        IpAddr::V6(v6) => is_blocked_v6(v6),
    }
}

fn is_blocked_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || o[0] == 0
        || (o[0] == 100 && (64..=127).contains(&o[1])) // CGNAT 100.64.0.0/10
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // IETF protocol assignments
        || o[0] >= 240 // reserved
}

fn is_blocked_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_blocked_v4(v4);
    }
    let seg0 = ip.segments()[0];
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (seg0 & 0xfe00) == 0xfc00 // unique local fc00::/7
        || (seg0 & 0xffc0) == 0xfe80 // link-local fe80::/10
        || (seg0 & 0xffc0) == 0xfec0 // deprecated site-local
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocked(s: &str) -> bool {
        is_blocked_ip(s.parse().expect("ip"))
    }

    #[test]
    fn blocks_private_and_loopback() {
        assert!(blocked("127.0.0.1"));
        assert!(blocked("10.1.2.3"));
        assert!(blocked("172.16.0.1"));
        assert!(blocked("192.168.1.1"));
        assert!(blocked("169.254.169.254"));
        assert!(blocked("0.0.0.0"));
        assert!(blocked("100.64.0.1"));
        assert!(blocked("::1"));
        assert!(blocked("::"));
        assert!(blocked("fe80::1"));
        assert!(blocked("fd00::1"));
        assert!(blocked("::ffff:127.0.0.1"));
    }

    #[test]
    fn allows_public() {
        assert!(!blocked("8.8.8.8"));
        assert!(!blocked("1.1.1.1"));
        assert!(!blocked("2606:4700:4700::1111"));
    }

    #[tokio::test]
    async fn rejects_bad_scheme_and_loopback() {
        assert!(fetch_text("file:///etc/passwd").await.is_err());
        assert!(fetch_text("ftp://example.com/").await.is_err());
        assert!(fetch_text("http://127.0.0.1:1/").await.is_err());
        assert!(fetch_text("http://[::1]/").await.is_err());
    }
}
