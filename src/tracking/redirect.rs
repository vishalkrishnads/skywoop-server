use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use sha2::{Digest, Sha256};
use sqlx::FromRow;

use crate::state::AppState;

const BOT_MARKERS: &[&str] = &[
    "facebookexternalhit",
    "facebot",
    "meta-externalagent",
    "twitterbot",
    "slackbot",
    "whatsapp",
    "telegrambot",
    "discordbot",
    "bot",
    "crawler",
    "spider",
    "preview",
];

#[derive(Debug, FromRow)]
struct TrackedLink {
    destination_url: String,
}

/// `/r/{token}` — records click + explicit 302 FOUND (B15).
pub async fn handle(
    State(s): State<AppState>,
    Path(token): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    let link = sqlx::query_as::<_, TrackedLink>(
        "select destination_url from tracked_links where token = $1",
    )
    .bind(&token)
    .fetch_optional(&s.db)
    .await
    .ok()
    .flatten();

    let Some(link) = link else {
        return redirect_to(s.cfg.fallback_url.as_str());
    };

    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let lower = ua.to_ascii_lowercase();
    let is_bot = method == Method::HEAD || BOT_MARKERS.iter().any(|m| lower.contains(m));
    let ip = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .unwrap_or("")
        .trim()
        .to_owned();

    let secret = s
        .secrets
        .get(&s.cfg.link_hash_secret_name)
        .await
        .unwrap_or_default();
    let visitor = hex::encode(Sha256::digest(format!(
        "{ip}|{ua}|{}|{secret}",
        Utc::now().date_naive()
    )));

    let db = s.db.clone();
    let token_owned = token.clone();
    let ua_owned = ua.clone();
    tokio::spawn(async move {
        let _ = sqlx::query(
            r#"
            insert into link_clicks (token, visitor_hash, user_agent, is_bot)
            values ($1, $2, $3, $4)
            "#,
        )
        .bind(&token_owned)
        .bind(&visitor)
        .bind(&ua_owned)
        .bind(is_bot)
        .execute(&db)
        .await;
    });

    redirect_to(&link.destination_url)
}

fn redirect_to(url: &str) -> Response {
    let mut res = StatusCode::FOUND.into_response();
    if let Ok(val) = header::HeaderValue::from_str(url) {
        res.headers_mut().insert(header::LOCATION, val);
    }
    res.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    res
}
