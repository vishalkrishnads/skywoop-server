use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use chrono::{Duration, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::domain::access;
use crate::domain::{Platform, new_id};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

const OAUTH_SCOPES: &str = "pages_show_list,pages_read_engagement,pages_manage_posts,read_insights,instagram_basic,instagram_content_publish,instagram_manage_insights,business_management";

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct StartOAuthResponse {
    pub authorize_url: String,
    pub state: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SelectConnectionRequest {
    pub state: String,
    pub page_id: String,
    pub instagram_account_id: Option<String>,
}

#[derive(Debug, Serialize, FromRow, ToSchema)]
pub struct ConnectionView {
    pub id: Uuid,
    pub project_id: Uuid,
    pub platform: String,
    pub external_account_id: String,
    pub display_name: String,
    pub status: String,
    pub connected_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OAuthCandidate {
    pub page_id: String,
    pub page_name: String,
    pub instagram_account_id: Option<String>,
    pub instagram_username: Option<String>,
}

fn redirect_uri(s: &AppState) -> String {
    let base = s.cfg.public_base_url.as_str().trim_end_matches('/');
    format!("{base}/oauth/meta/callback")
}

fn authorize_url(s: &AppState, state: &str) -> String {
    let version = s.cfg.meta_graph_version.trim_start_matches('/');
    let mut url = url::Url::parse(&format!("https://www.facebook.com/{version}/dialog/oauth"))
        .expect("static oauth url");
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("client_id", &s.cfg.meta_app_id);
        q.append_pair("redirect_uri", &redirect_uri(s));
        q.append_pair("state", state);
        q.append_pair("scope", OAUTH_SCOPES);
    }
    url.to_string()
}

fn random_state() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

#[utoipa::path(
    post,
    path = "/projects/{pid}/connections/meta/start",
    params(("pid" = Uuid, Path, description = "Project id")),
    responses((status = 200, body = StartOAuthResponse))
)]
pub async fn start(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
) -> AppResult<Json<StartOAuthResponse>> {
    access::ensure_project_owner(&s.db, &user.id, pid).await?;
    if s.cfg.meta_app_id.is_empty() {
        return Err(AppError::Invalid("META_APP_ID is not configured".into()));
    }
    let state = random_state();
    sqlx::query(
        r#"
        insert into oauth_states (state, project_id)
        values ($1, $2)
        "#,
    )
    .bind(&state)
    .bind(pid)
    .execute(&s.db)
    .await?;

    Ok(Json(StartOAuthResponse {
        authorize_url: authorize_url(&s, &state),
        state: state.clone(),
    }))
}

const MAX_ERROR_DESCRIPTION_CHARS: usize = 300;

/// `OAUTH_DONE_URL` with `state`, `project_id`, and (on failure) `error` / `error_description`.
fn done_url(
    base: &url::Url,
    state: Option<&str>,
    project_id: Option<Uuid>,
    error: Option<(&str, Option<&str>)>,
) -> url::Url {
    let mut done = base.clone();
    {
        let mut q = done.query_pairs_mut();
        if let Some(st) = state {
            q.append_pair("state", st);
        }
        if let Some(pid) = project_id {
            q.append_pair("project_id", &pid.to_string());
        }
        if let Some((err, desc)) = error {
            q.append_pair("error", err);
            if let Some(d) = desc.map(str::trim).filter(|d| !d.is_empty()) {
                let d: String = d.chars().take(MAX_ERROR_DESCRIPTION_CHARS).collect();
                q.append_pair("error_description", &d);
            }
        }
    }
    done
}

/// Public OAuth callback — `state` is the only authentication. Always redirects to
/// `OAUTH_DONE_URL` (errors and cancels included) so the user lands back in the app.
pub async fn callback(State(s): State<AppState>, Query(q): Query<CallbackQuery>) -> Response {
    let known_project: Option<Uuid> = match q.state.as_deref() {
        Some(st) => sqlx::query_scalar("select project_id from oauth_states where state = $1")
            .bind(st)
            .fetch_optional(&s.db)
            .await
            .ok()
            .flatten(),
        None => None,
    };
    let redirect = |state: Option<&str>, pid: Option<Uuid>, err: Option<(&str, Option<&str>)>| {
        Redirect::temporary(done_url(&s.cfg.oauth_done_url, state, pid, err).as_str())
            .into_response()
    };

    if let Some(err) = q.error.as_deref() {
        return redirect(
            q.state.as_deref(),
            known_project,
            Some((err, q.error_description.as_deref())),
        );
    }
    let (Some(code), Some(state)) = (q.code.as_deref(), q.state.as_deref()) else {
        return redirect(
            q.state.as_deref(),
            known_project,
            Some((
                "missing_code",
                Some("Meta did not return a code and state."),
            )),
        );
    };

    match complete_callback(&s, code, state).await {
        Ok(project_id) => redirect(Some(state), Some(project_id), None),
        Err(e) => {
            tracing::warn!(error = %e, "oauth callback failed");
            let desc = e.to_string();
            redirect(
                Some(state),
                known_project,
                Some(("callback_failed", Some(desc.as_str()))),
            )
        }
    }
}

async fn complete_callback(s: &AppState, code: &str, state: &str) -> anyhow::Result<Uuid> {
    let row = sqlx::query_as::<_, OAuthStateRow>(
        r#"
        select state, project_id, candidates, user_token_secret_name, created_at, consumed_at
          from oauth_states
         where state = $1
        "#,
    )
    .bind(state)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| anyhow::anyhow!("unknown OAuth state"))?;

    if row.consumed_at.is_some() {
        anyhow::bail!("OAuth state already used");
    }
    if Utc::now() - row.created_at > Duration::minutes(15) {
        anyhow::bail!("OAuth state expired");
    }

    let app_secret = s.secrets.get(&s.cfg.meta_app_secret_name).await?;
    let redirect = redirect_uri(s);

    let short = s
        .meta
        .get_public(
            "oauth/access_token",
            &[
                ("client_id", s.cfg.meta_app_id.as_str()),
                ("client_secret", app_secret.as_str()),
                ("redirect_uri", redirect.as_str()),
                ("code", code),
            ],
        )
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    let short_token = short["access_token"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("short-lived token missing"))?;

    let long = s
        .meta
        .get_public(
            "oauth/access_token",
            &[
                ("grant_type", "fb_exchange_token"),
                ("client_id", s.cfg.meta_app_id.as_str()),
                ("client_secret", app_secret.as_str()),
                ("fb_exchange_token", short_token),
            ],
        )
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    let user_token = long["access_token"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("long-lived token missing"))?;

    let accounts = s
        .meta
        .get(
            "me/accounts",
            &[(
                "fields",
                "id,name,access_token,instagram_business_account{id,username}",
            )],
            user_token,
        )
        .await
        .map_err(|e| anyhow::anyhow!(e))?;

    let candidates = candidates_from_accounts(&accounts);
    let secret_name = format!("meta-user-token-{state}");
    s.secrets.set(&secret_name, user_token).await?;

    sqlx::query(
        r#"
        update oauth_states
           set candidates = $2, user_token_secret_name = $3
         where state = $1
        "#,
    )
    .bind(state)
    .bind(json!(candidates))
    .bind(&secret_name)
    .execute(&s.db)
    .await?;

    Ok(row.project_id)
}

fn candidates_from_accounts(accounts: &Value) -> Vec<OAuthCandidate> {
    let mut out = Vec::new();
    let Some(data) = accounts["data"].as_array() else {
        return out;
    };
    for page in data {
        let page_id = page["id"].as_str().unwrap_or_default().to_owned();
        if page_id.is_empty() {
            continue;
        }
        let ig = page.get("instagram_business_account");
        out.push(OAuthCandidate {
            page_id,
            page_name: page["name"].as_str().unwrap_or_default().to_owned(),
            instagram_account_id: ig
                .and_then(|v| v.get("id"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_owned()),
            instagram_username: ig
                .and_then(|v| v.get("username"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_owned()),
        });
    }
    out
}

#[utoipa::path(
    post,
    path = "/projects/{pid}/connections/meta/select",
    params(("pid" = Uuid, Path, description = "Project id")),
    request_body = SelectConnectionRequest,
    responses((status = 200, description = "Connections upserted"))
)]
pub async fn select(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
    Json(body): Json<SelectConnectionRequest>,
) -> AppResult<Json<Value>> {
    access::ensure_project_owner(&s.db, &user.id, pid).await?;

    let row = sqlx::query_as::<_, OAuthStateRow>(
        r#"
        select state, project_id, candidates, user_token_secret_name, created_at, consumed_at
          from oauth_states
         where state = $1
        "#,
    )
    .bind(&body.state)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::Invalid("unknown OAuth state".into()))?;

    if row.project_id != pid {
        return Err(AppError::Invalid(
            "OAuth state belongs to another project".into(),
        ));
    }
    if row.consumed_at.is_some() {
        return Err(AppError::Conflict("OAuth state already consumed".into()));
    }
    if Utc::now() - row.created_at > Duration::minutes(15) {
        return Err(AppError::Invalid("OAuth state expired".into()));
    }
    let secret_name = row
        .user_token_secret_name
        .ok_or_else(|| AppError::Invalid("OAuth callback not completed".into()))?;
    let user_token = s.secrets.get(&secret_name).await.map_err(AppError::Other)?;

    let accounts = s
        .meta
        .get(
            "me/accounts",
            &[(
                "fields",
                "id,name,access_token,instagram_business_account{id,username}",
            )],
            &user_token,
        )
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;

    let page = accounts["data"]
        .as_array()
        .and_then(|arr| {
            arr.iter()
                .find(|p| p["id"].as_str() == Some(body.page_id.as_str()))
        })
        .ok_or_else(|| AppError::Invalid("page_id not in authorized accounts".into()))?;

    let page_token = page["access_token"]
        .as_str()
        .ok_or_else(|| AppError::Upstream("page access_token missing".into()))?;
    let page_name = page["name"].as_str().unwrap_or(&body.page_id).to_owned();

    let fb_secret = format!("meta-page-token-{pid}-facebook");
    s.secrets
        .set(&fb_secret, page_token)
        .await
        .map_err(AppError::Other)?;

    let mut tx = s.db.begin().await?;
    upsert_connection(
        &mut tx,
        pid,
        "facebook",
        &body.page_id,
        &page_name,
        &fb_secret,
    )
    .await?;

    if let Some(ig_id) = body.instagram_account_id.as_deref() {
        let ig_ok = page
            .pointer("/instagram_business_account/id")
            .and_then(|v| v.as_str())
            == Some(ig_id);
        if !ig_ok {
            return Err(AppError::Invalid(
                "instagram_account_id does not belong to the selected page".into(),
            ));
        }
        let ig_name = page
            .pointer("/instagram_business_account/username")
            .and_then(|v| v.as_str())
            .unwrap_or(ig_id)
            .to_owned();
        let ig_secret = format!("meta-page-token-{pid}-instagram");
        s.secrets
            .set(&ig_secret, page_token)
            .await
            .map_err(AppError::Other)?;
        upsert_connection(&mut tx, pid, "instagram", ig_id, &ig_name, &ig_secret).await?;
    }

    sqlx::query("update oauth_states set consumed_at = now() where state = $1")
        .bind(&body.state)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    // Forward only: a project already past `meta` keeps its step.
    sqlx::query(
        "update projects set onboarding_step = 'details' where id = $1 and onboarding_step = 'meta'",
    )
    .bind(pid)
    .execute(&s.db)
    .await?;

    if let Err(e) = s.secrets.delete(&secret_name).await {
        tracing::warn!(error = %e, secret = %secret_name, "failed to delete user token secret");
    }

    let connections = list_connections_inner(&s, pid).await?;
    Ok(Json(json!({ "connections": connections })))
}

async fn upsert_connection(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project_id: Uuid,
    platform: &str,
    external_account_id: &str,
    display_name: &str,
    token_secret_name: &str,
) -> AppResult<()> {
    sqlx::query(
        r#"
        insert into platform_connections
            (id, project_id, platform, external_account_id, display_name, token_secret_name, status)
        values ($1, $2, $3, $4, $5, $6, 'connected')
        on conflict (project_id, platform) do update
           set external_account_id = excluded.external_account_id,
               display_name = excluded.display_name,
               token_secret_name = excluded.token_secret_name,
               status = 'connected',
               connected_at = now()
        "#,
    )
    .bind(new_id())
    .bind(project_id)
    .bind(platform)
    .bind(external_account_id)
    .bind(display_name)
    .bind(token_secret_name)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[utoipa::path(
    get,
    path = "/projects/{pid}/connections",
    params(("pid" = Uuid, Path, description = "Project id")),
    responses((status = 200, body = [ConnectionView]))
)]
pub async fn list(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
) -> AppResult<Json<Vec<ConnectionView>>> {
    access::ensure_project_owner(&s.db, &user.id, pid).await?;
    Ok(Json(list_connections_inner(&s, pid).await?))
}

#[utoipa::path(
    get,
    path = "/projects/{pid}/connections/meta/candidates",
    params(
        ("pid" = Uuid, Path, description = "Project id"),
        ("state" = String, Query, description = "OAuth state"),
    ),
    responses((status = 200, description = "Page/IG candidates from callback"))
)]
pub async fn candidates(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
    Query(q): Query<StateQuery>,
) -> AppResult<Json<Value>> {
    access::ensure_project_owner(&s.db, &user.id, pid).await?;
    let row = sqlx::query_as::<_, OAuthStateRow>(
        r#"
        select state, project_id, candidates, user_token_secret_name, created_at, consumed_at
          from oauth_states
         where state = $1
        "#,
    )
    .bind(&q.state)
    .fetch_optional(&s.db)
    .await?
    .ok_or(AppError::NotFound)?;
    if row.project_id != pid {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({
        "state": row.state,
        "candidates": row.candidates.unwrap_or(Value::Array(vec![])),
        "consumed": row.consumed_at.is_some(),
    })))
}

#[derive(Debug, Deserialize)]
pub struct StateQuery {
    pub state: String,
}

#[utoipa::path(
    delete,
    path = "/projects/{pid}/connections/{platform}",
    params(
        ("pid" = Uuid, Path, description = "Project id"),
        ("platform" = String, Path, description = "instagram|facebook"),
    ),
    responses((status = 204, description = "Deleted"))
)]
pub async fn delete(
    State(s): State<AppState>,
    user: AuthUser,
    Path((pid, platform)): Path<(Uuid, String)>,
) -> AppResult<StatusCode> {
    access::ensure_project_owner(&s.db, &user.id, pid).await?;
    if Platform::parse(&platform).is_none() {
        return Err(AppError::Invalid(
            "platform must be instagram|facebook".into(),
        ));
    }
    let secret: Option<(String,)> = sqlx::query_as(
        r#"
        delete from platform_connections
         where project_id = $1 and platform = $2
        returning token_secret_name
        "#,
    )
    .bind(pid)
    .bind(&platform)
    .fetch_optional(&s.db)
    .await?;

    if let Some((name,)) = secret {
        // Demo clones share the template's token secret; only delete when unused.
        let still_used: bool = sqlx::query_scalar(
            "select exists(select 1 from platform_connections where token_secret_name = $1)",
        )
        .bind(&name)
        .fetch_one(&s.db)
        .await?;
        if !still_used {
            let _ = s.secrets.delete(&name).await;
        }
        return Ok(StatusCode::NO_CONTENT);
    }
    Err(AppError::NotFound)
}

async fn list_connections_inner(s: &AppState, pid: Uuid) -> AppResult<Vec<ConnectionView>> {
    let rows = sqlx::query_as::<_, ConnectionView>(
        r#"
        select id, project_id, platform, external_account_id, display_name, status, connected_at
          from platform_connections
         where project_id = $1
         order by platform
        "#,
    )
    .bind(pid)
    .fetch_all(&s.db)
    .await?;
    Ok(rows)
}

#[derive(Debug, FromRow)]
struct OAuthStateRow {
    state: String,
    project_id: Uuid,
    candidates: Option<Value>,
    user_token_secret_name: Option<String>,
    created_at: chrono::DateTime<Utc>,
    consumed_at: Option<chrono::DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn done_url_carries_project_and_errors() {
        let base = url::Url::parse("https://app.example/onboarding/meta/done").unwrap();
        let pid = Uuid::nil();
        let ok = done_url(&base, Some("abc"), Some(pid), None);
        assert_eq!(
            ok.as_str(),
            format!("https://app.example/onboarding/meta/done?state=abc&project_id={pid}")
        );
        let cancelled = done_url(
            &base,
            Some("abc"),
            Some(pid),
            Some(("access_denied", Some("User denied"))),
        );
        let pairs: Vec<(String, String)> = cancelled.query_pairs().into_owned().collect();
        assert!(pairs.contains(&("error".into(), "access_denied".into())));
        assert!(pairs.contains(&("error_description".into(), "User denied".into())));
        assert!(pairs.contains(&("project_id".into(), pid.to_string())));
        let long = "x".repeat(1000);
        let capped = done_url(&base, None, None, Some(("callback_failed", Some(&long))));
        let desc = capped
            .query_pairs()
            .find(|(k, _)| k == "error_description")
            .map(|(_, v)| v.into_owned())
            .unwrap();
        assert_eq!(desc.len(), MAX_ERROR_DESCRIPTION_CHARS);
    }
}
