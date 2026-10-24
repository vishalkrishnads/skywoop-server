//! Multi-user auth: Better Auth JWTs (production) or a fixed DevToken (local/CI).

mod jwks;

use axum::extract::{FromRequestParts, Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::Response;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::config::AuthMode;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub use jwks::JwksCache;

pub const DEV_USER_ID: &str = "dev-user";
pub const DEV_USER_EMAIL: &str = "dev@localhost";

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AuthUser {
    pub id: String,
    pub email: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct JwtClaims {
    pub sub: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub iss: Option<String>,
    #[serde(default)]
    pub aud: Option<Aud>,
    pub exp: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum Aud {
    One(String),
    Many(Vec<String>),
}

/// Middleware for all authenticated API routes: verify bearer, upsert `app_users`,
/// stash [`AuthUser`] in request extensions.
pub async fn require_user(
    State(s): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let user = authenticate(&s, req.headers()).await?;
    upsert_user(&s.db, &user).await?;
    req.extensions_mut().insert(user);
    Ok(next.run(req).await)
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<AuthUser>()
            .cloned()
            .ok_or(AppError::Unauthorized)
    }
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| raw.strip_prefix("Bearer ").filter(|t| !t.is_empty()))
}

async fn authenticate(state: &AppState, headers: &HeaderMap) -> AppResult<AuthUser> {
    let Some(token) = bearer_token(headers) else {
        tracing::warn!(
            has_authorization = headers.contains_key(AUTHORIZATION),
            "missing bearer token"
        );
        return Err(AppError::Unauthorized);
    };

    match state.cfg.auth_mode {
        AuthMode::DevToken => {
            if token == state.cfg.dev_token {
                Ok(AuthUser {
                    id: DEV_USER_ID.into(),
                    email: DEV_USER_EMAIL.into(),
                })
            } else {
                tracing::warn!("dev token mismatch");
                Err(AppError::Unauthorized)
            }
        }
        AuthMode::Jwt => {
            let claims = state
                .jwks
                .as_ref()
                .ok_or_else(|| AppError::Other(anyhow::anyhow!("AUTH_JWKS_URL not configured")))?
                .verify(token, &state.cfg)
                .await?;
            let email = claims
                .email
                .filter(|e| !e.is_empty())
                .unwrap_or_else(|| format!("{}@unknown", claims.sub));
            Ok(AuthUser {
                id: claims.sub,
                email,
            })
        }
    }
}

const UPSERT_USER_SQL: &str = r#"
    insert into app_users (id, email, last_seen_at)
    values ($1, $2, now())
    on conflict (id) do update set
      email = excluded.email,
      last_seen_at = case
        when app_users.last_seen_at is null or app_users.last_seen_at < $3
        then now()
        else app_users.last_seen_at
      end
"#;

async fn upsert_user(db: &sqlx::PgPool, user: &AuthUser) -> AppResult<()> {
    let cutoff = Utc::now() - Duration::minutes(5);
    let res = sqlx::query(UPSERT_USER_SQL)
        .bind(&user.id)
        .bind(&user.email)
        .bind(cutoff)
        .execute(db)
        .await;
    match res {
        Ok(_) => Ok(()),
        Err(sqlx::Error::Database(e)) if e.constraint() == Some("app_users_email_key") => {
            adopt_email(db, user, cutoff).await
        }
        Err(e) => Err(e.into()),
    }
}

/// A new Better Auth id arrived with an email that an older `app_users` row still holds
/// (the old auth account was deleted and the address signed up again). Better Auth keeps
/// emails unique, so the new id takes over the old row's projects and the old row is removed.
async fn adopt_email(
    db: &sqlx::PgPool,
    user: &AuthUser,
    cutoff: chrono::DateTime<Utc>,
) -> AppResult<()> {
    let mut tx = db.begin().await?;
    let old: Option<String> =
        sqlx::query_scalar("select id from app_users where email = $1 and id <> $2 for update")
            .bind(&user.email)
            .bind(&user.id)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some(old_id) = &old {
        sqlx::query("update app_users set email = $2 where id = $1")
            .bind(old_id)
            .bind(format!("replaced+{old_id}@skywoop.invalid"))
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(UPSERT_USER_SQL)
        .bind(&user.id)
        .bind(&user.email)
        .bind(cutoff)
        .execute(&mut *tx)
        .await?;
    if let Some(old_id) = &old {
        let moved = sqlx::query("update projects set owner_user_id = $2 where owner_user_id = $1")
            .bind(old_id)
            .bind(&user.id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        sqlx::query("delete from app_users where id = $1")
            .bind(old_id)
            .execute(&mut *tx)
            .await?;
        tracing::info!(old_user = %old_id, new_user = %user.id, projects = moved, "app user re-keyed by email");
    }
    tx.commit().await?;
    Ok(())
}
