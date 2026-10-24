use serde::Deserialize;
use serde_json::Value;

/// Parsed Meta Graph API error body.
#[derive(Debug, Clone, Deserialize)]
pub struct MetaErrorBody {
    pub message: String,
    #[serde(default, rename = "type")]
    pub error_type: Option<String>,
    #[serde(default)]
    pub code: Option<i64>,
    #[serde(default)]
    pub error_subcode: Option<i64>,
    #[serde(default)]
    pub fbtrace_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaErrorKind {
    /// Code 190 — invalid/expired token; caller should mark needs_reauth.
    NeedsReauth,
    /// Rate-limit codes; retry after `retry_after_mins`.
    RateLimited,
    /// Code 9 / subcode 2207042 — Instagram daily publishing limit.
    IgPublishingLimit,
    /// Everything else — retry.
    Retryable,
}

#[derive(Debug, thiserror::Error)]
#[error("{kind:?}: {message}")]
pub struct MetaError {
    pub kind: MetaErrorKind,
    pub message: String,
    pub code: Option<i64>,
    pub error_subcode: Option<i64>,
    pub fbtrace_id: Option<String>,
    /// Minutes until Meta expects access to recover (rate limits).
    pub retry_after_mins: i64,
}

impl MetaError {
    pub fn from_body(body: &MetaErrorBody, retry_after_mins: Option<i64>) -> Self {
        let kind = classify(body.code, body.error_subcode);
        let retry_after_mins = match kind {
            MetaErrorKind::RateLimited => retry_after_mins.unwrap_or(15),
            _ => retry_after_mins.unwrap_or(0),
        };
        if let Some(trace) = &body.fbtrace_id {
            tracing::warn!(
                fbtrace_id = %trace,
                code = ?body.code,
                subcode = ?body.error_subcode,
                "meta graph error"
            );
        }
        Self {
            kind,
            message: body.message.clone(),
            code: body.code,
            error_subcode: body.error_subcode,
            fbtrace_id: body.fbtrace_id.clone(),
            retry_after_mins,
        }
    }

    pub fn from_response_json(value: &Value, usage_header: Option<&str>) -> Option<Self> {
        let err = value.get("error")?;
        let body: MetaErrorBody = serde_json::from_value(err.clone()).ok()?;
        let retry = parse_usage_retry_mins(usage_header);
        Some(Self::from_body(&body, retry))
    }

    pub fn transport(msg: impl Into<String>) -> Self {
        Self {
            kind: MetaErrorKind::Retryable,
            message: msg.into(),
            code: None,
            error_subcode: None,
            fbtrace_id: None,
            retry_after_mins: 0,
        }
    }
}

fn classify(code: Option<i64>, subcode: Option<i64>) -> MetaErrorKind {
    match code {
        Some(190) => MetaErrorKind::NeedsReauth,
        Some(4 | 17 | 32 | 613 | 80001 | 80002) => MetaErrorKind::RateLimited,
        Some(9) if subcode == Some(2_207_042) => MetaErrorKind::IgPublishingLimit,
        _ => MetaErrorKind::Retryable,
    }
}

/// Walk `X-Business-Use-Case-Usage` JSON and return the max
/// `estimated_time_to_regain_access` (minutes).
pub fn parse_usage_retry_mins(header: Option<&str>) -> Option<i64> {
    let raw = header?.trim();
    if raw.is_empty() {
        return None;
    }
    let v: Value = serde_json::from_str(raw).ok()?;
    let mut max = 0i64;
    let obj = v.as_object()?;
    for (_k, arr) in obj {
        let Some(items) = arr.as_array() else {
            continue;
        };
        for item in items {
            if let Some(m) = item
                .get("estimated_time_to_regain_access")
                .and_then(|x| x.as_i64())
            {
                max = max.max(m);
            }
        }
    }
    if max > 0 { Some(max) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_token_error_190() {
        let body = MetaErrorBody {
            message: "Invalid OAuth access token.".into(),
            error_type: Some("OAuthException".into()),
            code: Some(190),
            error_subcode: None,
            fbtrace_id: Some("ABC".into()),
        };
        let e = MetaError::from_body(&body, None);
        assert_eq!(e.kind, MetaErrorKind::NeedsReauth);
    }

    #[test]
    fn maps_rate_limit_with_usage_header() {
        let body = MetaErrorBody {
            message: "Application request limit reached".into(),
            error_type: Some("OAuthException".into()),
            code: Some(4),
            error_subcode: None,
            fbtrace_id: Some("T".into()),
        };
        let header = r#"{"instagram":[{"type":"instagram","call_count":100,"total_cputime":1,"total_time":1,"estimated_time_to_regain_access":22}]}"#;
        let mins = parse_usage_retry_mins(Some(header));
        assert_eq!(mins, Some(22));
        let e = MetaError::from_body(&body, mins);
        assert_eq!(e.kind, MetaErrorKind::RateLimited);
        assert_eq!(e.retry_after_mins, 22);
    }

    #[test]
    fn rate_limit_defaults_to_15_minutes() {
        let body = MetaErrorBody {
            message: "rate".into(),
            error_type: None,
            code: Some(32),
            error_subcode: None,
            fbtrace_id: None,
        };
        let e = MetaError::from_body(&body, None);
        assert_eq!(e.kind, MetaErrorKind::RateLimited);
        assert_eq!(e.retry_after_mins, 15);
    }

    #[test]
    fn maps_ig_publishing_limit() {
        let body = MetaErrorBody {
            message: "limit".into(),
            error_type: None,
            code: Some(9),
            error_subcode: Some(2_207_042),
            fbtrace_id: None,
        };
        assert_eq!(
            MetaError::from_body(&body, None).kind,
            MetaErrorKind::IgPublishingLimit
        );
    }

    #[test]
    fn other_errors_are_retryable() {
        let v = json!({"error":{"message":"boom","code":100,"fbtrace_id":"x"}});
        let e = MetaError::from_response_json(&v, None).unwrap();
        assert_eq!(e.kind, MetaErrorKind::Retryable);
        assert_eq!(e.fbtrace_id.as_deref(), Some("x"));
    }
}
