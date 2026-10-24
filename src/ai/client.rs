use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use governor::clock::DefaultClock;
use governor::state::{InMemoryState, NotKeyed};
use governor::{Quota, RateLimiter};
use reqwest::header::{AUTHORIZATION, RETRY_AFTER};
use reqwest::multipart::{Form, Part};
use reqwest::{Client, StatusCode};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::azure::token::TokenCache;
use crate::config::Config;
use crate::domain::new_id;

use super::schema::strict_schema;

type ImageLimiter = RateLimiter<NotKeyed, InMemoryState, DefaultClock>;

const TEXT_TIMEOUT: Duration = Duration::from_secs(180);
/// One tool-loop turn can reason and write a full plan. 180s cuts those off.
const TOOL_LOOP_TIMEOUT: Duration = Duration::from_secs(600);
const IMAGE_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_RETRIES: u32 = 3;

#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("content filtered by Azure OpenAI")]
    ContentFiltered,
    #[error("incomplete response: {0}")]
    Incomplete(String),
    #[error("no model output")]
    NoOutput,
    #[error("invalid model output: {0}")]
    InvalidOutput(String),
    #[error("http {status}: {body}")]
    Http { status: u16, body: String },
    #[error("no Azure OpenAI credentials (set AZURE_OPENAI_API_KEY or configure managed identity)")]
    NoCredentials,
    /// Connection dropped or timed out before a usable response. Retryable.
    #[error("{0}")]
    Transport(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[derive(Debug, Clone, Copy)]
pub enum TextRole {
    Planner,
    Vision,
    Copy,
}

#[derive(Debug, Clone, Copy)]
pub enum ImageTier {
    Draft,
    Final,
}

#[derive(Debug, Clone, Copy)]
pub enum Effort {
    Low,
    Medium,
    High,
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// Generate at 1088×1088 (1:1, multiple of 16). Publish JPEG is 1080×1080.
#[derive(Debug, Clone, Copy)]
pub enum Size {
    Square,
    /// Legacy 4:5 — kept for reading old smoke fixtures; new renders use [`Size::Square`].
    P4x5,
}

impl Size {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Square => "1088x1088",
            Self::P4x5 => "1088x1360",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Quality {
    Low,
    Medium,
    High,
}

impl Quality {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImageInput {
    pub mime: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct RenderedPrompt {
    pub system: String,
    pub user: String,
    pub version: String,
}

pub struct AiClient {
    pub cfg: Arc<Config>,
    pub http: Client,
    pub tokens: Option<Arc<TokenCache>>,
    pub db: Option<PgPool>,
    pub image_limiter: ImageLimiter,
}

impl AiClient {
    pub fn new(
        cfg: Arc<Config>,
        tokens: Option<Arc<TokenCache>>,
        db: Option<PgPool>,
    ) -> anyhow::Result<Arc<Self>> {
        let rpm = NonZeroU32::new(cfg.image_rpm.max(1)).unwrap();
        Ok(Arc::new(Self {
            cfg,
            http: Client::builder().timeout(TOOL_LOOP_TIMEOUT).build()?,
            tokens,
            db,
            image_limiter: RateLimiter::direct(Quota::per_minute(rpm)),
        }))
    }

    pub async fn bearer(&self) -> Result<String, AiError> {
        if let Some(key) = &self.cfg.azure_openai_api_key
            && !key.is_empty()
        {
            return Ok(key.clone());
        }
        match &self.tokens {
            Some(t) => t.get().await.map_err(AiError::Other),
            None => Err(AiError::NoCredentials),
        }
    }

    pub fn deployment(&self, role: TextRole) -> &str {
        match role {
            TextRole::Planner => &self.cfg.deployment_planner,
            TextRole::Vision => &self.cfg.deployment_vision,
            TextRole::Copy => &self.cfg.deployment_copy,
        }
    }

    pub fn image_deployment(&self, tier: ImageTier) -> &str {
        match tier {
            ImageTier::Draft => &self.cfg.deployment_image_draft,
            ImageTier::Final => &self.cfg.deployment_image_final,
        }
    }

    fn url(&self, path: &str) -> String {
        let base = self.cfg.azure_openai_base.as_str().trim_end_matches('/');
        format!("{base}/{path}")
    }

    fn images_path(&self, p: &str) -> String {
        match &self.cfg.azure_openai_images_api_version {
            Some(ver) => format!("{p}?api-version={ver}"),
            None => p.to_owned(),
        }
    }

    pub async fn structured<T: DeserializeOwned + JsonSchema>(
        &self,
        role: TextRole,
        purpose: &'static str,
        prompt: &RenderedPrompt,
        images: &[ImageInput],
        effort: Effort,
    ) -> Result<T, AiError> {
        self.structured_with_tools(role, purpose, prompt, images, effort, &[], None)
            .await
    }

    /// Like [`Self::structured`], optionally enabling Responses tools (e.g. `web_search`)
    /// and a `user_location` hint for geo-aware search.
    pub async fn structured_with_tools<T: DeserializeOwned + JsonSchema>(
        &self,
        role: TextRole,
        purpose: &'static str,
        prompt: &RenderedPrompt,
        images: &[ImageInput],
        effort: Effort,
        tools: &[Value],
        user_location: Option<Value>,
    ) -> Result<T, AiError> {
        let started = Instant::now();
        let deployment = self.deployment(role).to_owned();
        let mut content = vec![json!({ "type": "input_text", "text": prompt.user })];
        for img in images {
            content.push(json!({
                "type": "input_image",
                "image_url": format!(
                    "data:{};base64,{}",
                    img.mime,
                    BASE64_STANDARD.encode(&img.bytes)
                )
            }));
        }
        let mut body = json!({
            "model": deployment,
            "instructions": prompt.system,
            "input": [{ "role": "user", "content": content }],
            "reasoning": { "effort": effort.as_str() },
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": purpose,
                    "schema": strict_schema::<T>(),
                    "strict": true
                }
            },
            "max_output_tokens": 16000,
            "store": false
        });
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools.to_vec());
            body["tool_choice"] = json!("auto");
        }
        if let Some(loc) = user_location {
            body["user_location"] = loc;
        }

        let resp = match self
            .post_json("responses", &body, TEXT_TIMEOUT, MAX_RETRIES)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                self.log_call(
                    purpose,
                    &prompt.version,
                    &deployment,
                    &Value::Null,
                    started.elapsed(),
                    "error",
                    Some(&e.to_string()),
                    Some(images.len() as i32),
                )
                .await;
                return Err(e);
            }
        };

        if resp["status"] == "incomplete" {
            let detail = resp["incomplete_details"].to_string();
            self.log_call(
                purpose,
                &prompt.version,
                &deployment,
                &resp["usage"],
                started.elapsed(),
                "incomplete",
                Some(&detail),
                Some(images.len() as i32),
            )
            .await;
            return Err(AiError::Incomplete(detail));
        }

        let text = resp["output"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| item["type"] == "message")
            .flat_map(|item| item["content"].as_array().cloned().unwrap_or_default())
            .find_map(|c| match c["type"].as_str() {
                Some("output_text") => c["text"].as_str().map(str::to_owned),
                _ => None,
            })
            .ok_or(AiError::NoOutput)?;

        self.log_call(
            purpose,
            &prompt.version,
            &deployment,
            &resp["usage"],
            started.elapsed(),
            "ok",
            None,
            Some(images.len() as i32),
        )
        .await;

        serde_json::from_str(&text).map_err(|e| AiError::InvalidOutput(e.to_string()))
    }

    pub async fn generate(
        &self,
        tier: ImageTier,
        prompt: &str,
        size: Size,
        quality: Quality,
    ) -> Result<Vec<u8>, AiError> {
        self.image_limiter.until_ready().await;
        let started = Instant::now();
        let deployment = self.image_deployment(tier).to_owned();
        let body = json!({
            "model": deployment,
            "prompt": prompt,
            "size": size.as_str(),
            "quality": quality.as_str(),
            "n": 1,
            "output_format": "png"
        });
        let path = self.images_path("images/generations");
        let v = match self
            .post_json(&path, &body, IMAGE_TIMEOUT, MAX_RETRIES)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                self.log_call(
                    "image_generate",
                    "n/a",
                    &deployment,
                    &Value::Null,
                    started.elapsed(),
                    "error",
                    Some(&e.to_string()),
                    Some(1),
                )
                .await;
                return Err(e);
            }
        };
        self.log_call(
            "image_generate",
            "n/a",
            &deployment,
            &v["usage"],
            started.elapsed(),
            "ok",
            None,
            Some(1),
        )
        .await;
        decode_b64(&v["data"][0]["b64_json"])
    }

    /// `images[0]` is the image being edited. The rest are references (logo, screenshots, parent).
    pub async fn edit(
        &self,
        tier: ImageTier,
        prompt: &str,
        images: &[ImageInput],
        size: Size,
        quality: Quality,
    ) -> Result<Vec<u8>, AiError> {
        self.image_limiter.until_ready().await;
        let started = Instant::now();
        let deployment = self.image_deployment(tier).to_owned();
        let path = self.images_path("images/edits");
        let v = match self
            .post_multipart_edit(&path, &deployment, prompt, images, size, quality)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                self.log_call(
                    "image_edit",
                    "n/a",
                    &deployment,
                    &Value::Null,
                    started.elapsed(),
                    "error",
                    Some(&e.to_string()),
                    Some(images.len() as i32),
                )
                .await;
                return Err(e);
            }
        };
        self.log_call(
            "image_edit",
            "n/a",
            &deployment,
            &v["usage"],
            started.elapsed(),
            "ok",
            None,
            Some(images.len() as i32),
        )
        .await;
        decode_b64(&v["data"][0]["b64_json"])
    }

    async fn post_json(
        &self,
        path: &str,
        body: &Value,
        timeout: Duration,
        max_retries: u32,
    ) -> Result<Value, AiError> {
        let url = self.url(path);
        // Stream text responses so Azure sends headers immediately. A non-streaming
        // call that thinks for ~30s is cut off before any status line comes back,
        // which surfaces as "error sending request".
        let stream = path == "responses";
        let payload = if stream {
            let mut payload = body.clone();
            payload["stream"] = json!(true);
            payload
        } else {
            body.clone()
        };
        let mut attempt = 0u32;
        loop {
            let token = self.bearer().await?;
            let resp = match self
                .http
                .post(&url)
                .timeout(timeout)
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .json(&payload)
                .send()
                .await
            {
                Ok(resp) => resp,
                Err(e) => {
                    let err = transport_error(e);
                    if attempt < max_retries {
                        sleep_backoff(attempt, None).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(err);
                }
            };
            let status = resp.status();
            let retry_after = parse_retry_after(resp.headers().get(RETRY_AFTER));
            let bytes = match resp.bytes().await {
                Ok(bytes) => bytes,
                Err(e) => {
                    let err = transport_error(e);
                    if attempt < max_retries {
                        sleep_backoff(attempt, None).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(err);
                }
            };
            if status.is_success() {
                let parsed = if stream {
                    parse_responses_payload(&bytes)
                } else {
                    serde_json::from_slice(&bytes)
                        .map_err(|e| AiError::InvalidOutput(e.to_string()))
                };
                match parsed {
                    Ok(v) => return Ok(v),
                    // Azure sometimes returns HTTP 200 with an SSE `error` event
                    // (`server_error`) a second into the stream. Retry that the
                    // same way as a 5xx instead of failing the whole draft.
                    Err(e) if transient_model_error(&e) && attempt < max_retries => {
                        sleep_backoff(attempt, None).await;
                        attempt += 1;
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            }
            let body_str = String::from_utf8_lossy(&bytes).into_owned();
            if is_content_filtered(status, &body_str) {
                return Err(AiError::ContentFiltered);
            }
            if should_retry(status) && attempt < max_retries {
                sleep_backoff(attempt, retry_after).await;
                attempt += 1;
                continue;
            }
            return Err(AiError::Http {
                status: status.as_u16(),
                body: body_str,
            });
        }
    }

    async fn post_multipart_edit(
        &self,
        path: &str,
        deployment: &str,
        prompt: &str,
        images: &[ImageInput],
        size: Size,
        quality: Quality,
    ) -> Result<Value, AiError> {
        let url = self.url(path);
        let mut attempt = 0u32;
        loop {
            let mut form = Form::new()
                .text("model", deployment.to_owned())
                .text("prompt", prompt.to_owned())
                .text("size", size.as_str())
                .text("quality", quality.as_str())
                .text("n", "1")
                .text("output_format", "png");
            // gpt-image-2.5-flare rejects input_fidelity; do not send it.
            for (i, img) in images.iter().enumerate() {
                form = form.part(
                    "image[]",
                    Part::bytes(img.bytes.clone())
                        .file_name(format!("input{i}.png"))
                        .mime_str(&img.mime)
                        .map_err(|e| AiError::Other(e.into()))?,
                );
            }
            let token = self.bearer().await?;
            let resp = self
                .http
                .post(&url)
                .timeout(IMAGE_TIMEOUT)
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .multipart(form)
                .send()
                .await
                .map_err(|e| AiError::Other(e.into()))?;
            let status = resp.status();
            let retry_after = parse_retry_after(resp.headers().get(RETRY_AFTER));
            let bytes = resp.bytes().await.map_err(|e| AiError::Other(e.into()))?;
            if status.is_success() {
                return serde_json::from_slice(&bytes)
                    .map_err(|e| AiError::InvalidOutput(e.to_string()));
            }
            let body_str = String::from_utf8_lossy(&bytes).into_owned();
            if is_content_filtered(status, &body_str) {
                return Err(AiError::ContentFiltered);
            }
            if should_retry(status) && attempt < MAX_RETRIES {
                sleep_backoff(attempt, retry_after).await;
                attempt += 1;
                continue;
            }
            return Err(AiError::Http {
                status: status.as_u16(),
                body: body_str,
            });
        }
    }

    /// One turn of a Responses API tool loop (function calls or final message).
    /// Request `include: ["reasoning.encrypted_content"]` and echo reasoning items via `input`.
    /// When `output_schema` is set, the final message must match that JSON schema.
    pub async fn responses_tool_loop_turn(
        &self,
        role: TextRole,
        purpose: &str,
        prompt: &RenderedPrompt,
        input: &[Value],
        tools: &[Value],
        effort: Effort,
        max_output_tokens: u32,
        output_schema: Option<&Value>,
    ) -> Result<crate::agent::AgentTurn, AiError> {
        use crate::agent::{AgentTurn, FunctionCall};

        let started = Instant::now();
        let deployment = self.deployment(role).to_owned();

        let mut conversation = input.to_vec();
        if conversation.is_empty() {
            conversation.push(json!({
                "role": "user",
                "content": [{ "type": "input_text", "text": prompt.user }]
            }));
        }

        let mut body = json!({
            "model": deployment,
            "instructions": prompt.system,
            "input": conversation,
            "reasoning": { "effort": effort.as_str() },
            "include": ["reasoning.encrypted_content"],
            "max_output_tokens": max_output_tokens,
            "store": false
        });
        if let Some(schema) = output_schema {
            body["text"] = json!({
                "format": {
                    "type": "json_schema",
                    "name": purpose,
                    "schema": schema,
                    "strict": true
                }
            });
        }
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools.to_vec());
            body["tool_choice"] = json!("auto");
        }

        let resp = match self
            .post_json("responses", &body, TOOL_LOOP_TIMEOUT, MAX_RETRIES)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                self.log_call(
                    purpose,
                    &prompt.version,
                    &deployment,
                    &Value::Null,
                    started.elapsed(),
                    "error",
                    Some(&e.to_string()),
                    None,
                )
                .await;
                return Err(e);
            }
        };

        if resp["status"] == "incomplete" {
            let detail = resp["incomplete_details"].to_string();
            self.log_call(
                purpose,
                &prompt.version,
                &deployment,
                &resp["usage"],
                started.elapsed(),
                "incomplete",
                Some(&detail),
                None,
            )
            .await;
            return Err(AiError::Incomplete(detail));
        }

        let output = resp["output"].as_array().cloned().unwrap_or_default();
        let mut calls = Vec::new();
        let mut echo_items = Vec::new();
        let mut final_text: Option<String> = None;

        for item in &output {
            let ty = item["type"].as_str().unwrap_or("");
            match ty {
                "function_call" => {
                    let call_id = item["call_id"]
                        .as_str()
                        .or_else(|| item["id"].as_str())
                        .unwrap_or("")
                        .to_owned();
                    let name = item["name"].as_str().unwrap_or("").to_owned();
                    let arguments = match &item["arguments"] {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    calls.push(FunctionCall {
                        call_id,
                        name,
                        arguments,
                    });
                    echo_items.push(item.clone());
                }
                "reasoning" => {
                    echo_items.push(item.clone());
                }
                "message" => {
                    if let Some(arr) = item["content"].as_array() {
                        for c in arr {
                            if c["type"] == "output_text"
                                && let Some(t) = c["text"].as_str()
                            {
                                final_text = Some(t.to_owned());
                            }
                        }
                    }
                    echo_items.push(item.clone());
                }
                _ => {
                    // Preserve unknown items that may be needed for continuity
                    if item.get("encrypted_content").is_some() || ty.contains("reasoning") {
                        echo_items.push(item.clone());
                    }
                }
            }
        }

        self.log_call(
            purpose,
            &prompt.version,
            &deployment,
            &resp["usage"],
            started.elapsed(),
            "ok",
            None,
            None,
        )
        .await;

        let usage = resp.get("usage").cloned();
        if !calls.is_empty() {
            return Ok(AgentTurn::FunctionCalls {
                calls,
                echo_items,
                usage,
            });
        }
        let text = final_text.ok_or(AiError::NoOutput)?;
        Ok(AgentTurn::Final { text, usage })
    }

    async fn log_call(
        &self,
        purpose: &str,
        prompt_version: &str,
        deployment: &str,
        usage: &Value,
        latency: Duration,
        status: &str,
        error: Option<&str>,
        images: Option<i32>,
    ) {
        let Some(db) = &self.db else {
            return;
        };
        let input_tokens = usage_i32(usage, "input_tokens");
        let output_tokens = usage_i32(usage, "output_tokens");
        let latency_ms = latency.as_millis().min(i32::MAX as u128) as i32;
        if let Err(e) = sqlx::query(
            r#"
            insert into ai_calls (
              id, purpose, prompt_version, deployment,
              input_tokens, output_tokens, images, latency_ms, status, error
            ) values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
            "#,
        )
        .bind(new_id())
        .bind(purpose)
        .bind(prompt_version)
        .bind(deployment)
        .bind(input_tokens)
        .bind(output_tokens)
        .bind(images)
        .bind(latency_ms)
        .bind(status)
        .bind(error)
        .execute(db)
        .await
        {
            tracing::warn!(error = ?e, purpose, "failed to log ai_calls row");
        }
    }
}

/// Resize master PNG to Instagram/Facebook 1:1 publish JPEG (1080×1080, quality 90).
/// Center-crops to square when the source is not already 1:1 (e.g. legacy 4:5 masters).
pub fn to_publish_jpeg(png: &[u8]) -> anyhow::Result<Vec<u8>> {
    let img = image::load_from_memory(png)?;
    let w = img.width();
    let h = img.height();
    let side = w.min(h);
    let x = (w - side) / 2;
    let y = (h - side) / 2;
    let cropped = img.crop_imm(x, y, side, side);
    let rgb = cropped
        .resize_exact(1080, 1080, image::imageops::FilterType::Lanczos3)
        .to_rgb8();
    let mut out = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90);
    encoder.encode_image(&rgb)?;
    anyhow::ensure!(
        out.len() < 8 * 1024 * 1024,
        "jpeg exceeds Instagram's 8 MB limit"
    );
    Ok(out)
}

fn decode_b64(v: &Value) -> Result<Vec<u8>, AiError> {
    let s = v
        .as_str()
        .ok_or_else(|| AiError::InvalidOutput("missing b64_json".into()))?;
    BASE64_STANDARD
        .decode(s)
        .map_err(|e| AiError::InvalidOutput(e.to_string()))
}

fn is_content_filtered(status: StatusCode, body: &str) -> bool {
    if status != StatusCode::BAD_REQUEST {
        return false;
    }
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return body.contains("content_filter") || body.contains("moderation_blocked");
    };
    let code = v
        .pointer("/error/code")
        .and_then(|c| c.as_str())
        .unwrap_or("");
    code == "content_filter" || code == "moderation_blocked"
}

fn transport_error(e: reqwest::Error) -> AiError {
    let mut msg = e.to_string();
    let mut src = std::error::Error::source(&e);
    while let Some(s) = src {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        src = s.source();
    }
    AiError::Transport(msg)
}

/// Accept either a plain Responses JSON body or an SSE stream, and return the
/// completed response object `structured` already knows how to read.
fn parse_responses_payload(bytes: &[u8]) -> Result<Value, AiError> {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.starts_with('{') {
        let v: Value =
            serde_json::from_str(trimmed).map_err(|e| AiError::InvalidOutput(e.to_string()))?;
        return response_or_fail(v);
    }

    let mut event_data = String::new();
    let mut completed: Option<Value> = None;
    let flush = |event_data: &mut String, completed: &mut Option<Value>| -> Result<(), AiError> {
        if event_data.is_empty() {
            return Ok(());
        }
        let raw = std::mem::take(event_data);
        let Ok(v) = serde_json::from_str::<Value>(&raw) else {
            return Ok(());
        };
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match ty {
            "response.completed" | "response.incomplete" | "response.failed" => {
                *completed = Some(v.get("response").cloned().unwrap_or(v));
            }
            "error" => {
                return Err(AiError::Transport(v.to_string()));
            }
            _ => {}
        }
        Ok(())
    };

    for line in text.lines() {
        if line.is_empty() {
            flush(&mut event_data, &mut completed)?;
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            let rest = rest.trim();
            if rest == "[DONE]" {
                continue;
            }
            if !event_data.is_empty() {
                event_data.push('\n');
            }
            event_data.push_str(rest);
        }
    }
    flush(&mut event_data, &mut completed)?;
    response_or_fail(completed.ok_or(AiError::NoOutput)?)
}

fn response_or_fail(v: Value) -> Result<Value, AiError> {
    if v.get("status").and_then(|s| s.as_str()) == Some("failed") {
        return Err(AiError::Transport(format!(
            "response failed: {}",
            v.get("error").unwrap_or(&Value::Null)
        )));
    }
    Ok(v)
}

fn should_retry(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn transient_model_error(err: &AiError) -> bool {
    match err {
        AiError::Transport(msg) => {
            msg.contains("server_error") || msg.contains("error processing your request")
        }
        _ => false,
    }
}

fn parse_retry_after(v: Option<&reqwest::header::HeaderValue>) -> Option<Duration> {
    let s = v?.to_str().ok()?;
    if let Ok(secs) = s.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    None
}

async fn sleep_backoff(attempt: u32, retry_after: Option<Duration>) {
    let backoff = retry_after.unwrap_or_else(|| Duration::from_secs(2u64.pow(attempt).min(30)));
    tokio::time::sleep(backoff).await;
}

fn usage_i32(usage: &Value, key: &str) -> Option<i32> {
    usage.get(key).and_then(|v| v.as_i64()).map(|n| n as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_square_is_1088x1088() {
        assert_eq!(Size::Square.as_str(), "1088x1088");
        assert_eq!(Size::P4x5.as_str(), "1088x1360");
    }

    #[test]
    fn responses_stream_returns_completed_object() {
        let sse = "\
event: response.created
data: {\"type\":\"response.created\",\"response\":{\"status\":\"in_progress\"}}

event: response.completed
data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}

";
        let v = parse_responses_payload(sse.as_bytes()).unwrap();
        assert_eq!(v["status"], "completed");
        assert!(v.get("output").unwrap().is_array());
    }

    #[test]
    fn content_filter_detection() {
        let body = r#"{"error":{"code":"content_filter","message":"blocked"}}"#;
        assert!(is_content_filtered(StatusCode::BAD_REQUEST, body));
        assert!(!is_content_filtered(
            StatusCode::BAD_REQUEST,
            r#"{"error":{"code":"invalid"}}"#
        ));
    }

    #[test]
    fn publish_jpeg_is_1080x1080() {
        let mut png = Vec::new();
        {
            let img = image::ImageBuffer::from_pixel(64, 80, image::Rgba([10, 20, 30, 255]));
            let enc = image::codecs::png::PngEncoder::new(&mut png);
            use image::ImageEncoder;
            enc.write_image(img.as_raw(), 64, 80, image::ExtendedColorType::Rgba8)
                .unwrap();
        }
        let jpeg = to_publish_jpeg(&png).unwrap();
        let decoded = image::load_from_memory(&jpeg).unwrap();
        assert_eq!(decoded.width(), 1080);
        assert_eq!(decoded.height(), 1080);
    }
}
