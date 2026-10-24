use std::sync::Arc;

use reqwest::multipart::Form;
use reqwest::{Client, Method, StatusCode};
use serde_json::Value;
use url::Url;

use crate::config::Config;

use super::error::{MetaError, MetaErrorKind};

/// Thin Meta Graph API client.
pub struct MetaClient {
    pub cfg: Arc<Config>,
    pub http: Client,
}

impl MetaClient {
    pub fn new(cfg: Arc<Config>) -> anyhow::Result<Arc<Self>> {
        Ok(Arc::new(Self {
            cfg,
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()?,
        }))
    }

    /// Build `{META_GRAPH_BASE}/{version}/{path}`.
    pub fn graph_url(&self, path: &str) -> anyhow::Result<Url> {
        let base = self.cfg.meta_graph_base.as_str().trim_end_matches('/');
        let version = self.cfg.meta_graph_version.trim_start_matches('/');
        let path = path.trim_start_matches('/');
        Ok(Url::parse(&format!("{base}/{version}/{path}"))?)
    }

    pub async fn get(
        &self,
        path: &str,
        query: &[(&str, &str)],
        access_token: &str,
    ) -> Result<Value, MetaError> {
        let mut url = self
            .graph_url(path)
            .map_err(|e| MetaError::transport(e.to_string()))?;
        {
            let mut q = url.query_pairs_mut();
            for (k, v) in query {
                q.append_pair(k, v);
            }
            q.append_pair("access_token", access_token);
        }
        self.send(Method::GET, url, None).await
    }

    /// Unauthenticated GET (OAuth token exchange).
    pub async fn get_public(&self, path: &str, query: &[(&str, &str)]) -> Result<Value, MetaError> {
        let mut url = self
            .graph_url(path)
            .map_err(|e| MetaError::transport(e.to_string()))?;
        {
            let mut q = url.query_pairs_mut();
            for (k, v) in query {
                q.append_pair(k, v);
            }
        }
        self.send(Method::GET, url, None).await
    }

    pub async fn post_form(
        &self,
        path: &str,
        fields: &[(&str, &str)],
        access_token: &str,
    ) -> Result<Value, MetaError> {
        let url = self
            .graph_url(path)
            .map_err(|e| MetaError::transport(e.to_string()))?;
        let mut body = vec![("access_token", access_token)];
        body.extend(fields.iter().copied());
        self.send(Method::POST, url, Some(body)).await
    }

    pub async fn post_multipart(&self, path: &str, form: Form) -> Result<Value, MetaError> {
        let url = self
            .graph_url(path)
            .map_err(|e| MetaError::transport(e.to_string()))?;
        let resp = self
            .http
            .post(url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| MetaError::transport(e.to_string()))?;
        self.parse_response(resp).await
    }

    async fn send(
        &self,
        method: Method,
        url: Url,
        form: Option<Vec<(&str, &str)>>,
    ) -> Result<Value, MetaError> {
        let mut req = self.http.request(method, url);
        if let Some(fields) = form {
            req = req.form(&fields);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| MetaError::transport(e.to_string()))?;
        self.parse_response(resp).await
    }

    async fn parse_response(&self, resp: reqwest::Response) -> Result<Value, MetaError> {
        let status = resp.status();
        let usage = resp
            .headers()
            .get("x-business-use-case-usage")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_owned());
        let text = resp
            .text()
            .await
            .map_err(|e| MetaError::transport(e.to_string()))?;
        let value: Value = serde_json::from_str(&text).unwrap_or_else(|_| {
            Value::Object(serde_json::Map::from_iter([(
                "raw".into(),
                Value::String(text.clone()),
            )]))
        });
        if let Some(err) = MetaError::from_response_json(&value, usage.as_deref()) {
            return Err(err);
        }
        if !status.is_success() && status != StatusCode::OK {
            return Err(MetaError {
                kind: if status == StatusCode::TOO_MANY_REQUESTS {
                    MetaErrorKind::RateLimited
                } else {
                    MetaErrorKind::Retryable
                },
                message: format!("http {status}: {text}"),
                code: None,
                error_subcode: None,
                fbtrace_id: None,
                retry_after_mins: if status == StatusCode::TOO_MANY_REQUESTS {
                    15
                } else {
                    0
                },
            });
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::config::Config;
    use crate::meta::error::MetaErrorKind;

    use super::MetaClient;

    fn test_cfg(base: &str) -> Arc<Config> {
        let mut cfg = Config::from_env().expect("config");
        cfg.meta_graph_base = url::Url::parse(base).unwrap();
        cfg.meta_graph_version = "v26.0".into();
        Arc::new(cfg)
    }

    #[tokio::test]
    async fn maps_error_body_from_wiremock() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v26.0/me"))
            .respond_with(
                ResponseTemplate::new(400)
                    .insert_header(
                        "X-Business-Use-Case-Usage",
                        r#"{"instagram":[{"estimated_time_to_regain_access":12}]}"#,
                    )
                    .set_body_json(json!({
                        "error": {
                            "message": "Application request limit reached",
                            "type": "OAuthException",
                            "code": 4,
                            "fbtrace_id": "trace1"
                        }
                    })),
            )
            .mount(&server)
            .await;

        let client = MetaClient::new(test_cfg(&server.uri())).unwrap();
        let err = client.get("me", &[], "token").await.unwrap_err();
        assert_eq!(err.kind, MetaErrorKind::RateLimited);
        assert_eq!(err.retry_after_mins, 12);
        assert_eq!(err.fbtrace_id.as_deref(), Some("trace1"));
    }

    #[tokio::test]
    async fn get_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v26.0/act"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "1"})))
            .mount(&server)
            .await;
        let client = MetaClient::new(test_cfg(&server.uri())).unwrap();
        let v = client.get("act", &[], "t").await.unwrap();
        assert_eq!(v["id"], "1");
    }
}
