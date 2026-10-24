//! Current-user profile and project list.

use axum::Json;
use axum::extract::State;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::AppResult;
use crate::state::AppState;

#[derive(Debug, Serialize, ToSchema)]
pub struct MeResponse {
    pub id: String,
    pub email: String,
    /// When the user first signed in.
    pub created_at: DateTime<Utc>,
    pub projects: Vec<MeProject>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MeProject {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub onboarding_step: String,
    pub created_at: DateTime<Utc>,
    /// Signed read URL of the brand logo (app icon), when set.
    pub icon_url: Option<String>,
}

#[derive(FromRow)]
struct MeProjectRow {
    id: Uuid,
    slug: String,
    name: String,
    onboarding_step: String,
    created_at: DateTime<Utc>,
    icon_blob_path: Option<String>,
}

const ICON_URL_TTL_MINUTES: i64 = 60;

#[utoipa::path(
    get,
    path = "/me",
    responses((status = 200, description = "Current user", body = MeResponse))
)]
pub async fn me(State(s): State<AppState>, user: AuthUser) -> AppResult<Json<MeResponse>> {
    let created_at: DateTime<Utc> =
        sqlx::query_scalar("select created_at from app_users where id = $1")
            .bind(&user.id)
            .fetch_one(&s.db)
            .await?;
    let rows = sqlx::query_as::<_, MeProjectRow>(
        r#"
        select p.id, p.slug, p.name, p.onboarding_step, p.created_at,
               icon.blob_path as icon_blob_path
          from projects p
          left join lateral (
              select a.blob_path from assets a
               where a.project_id = p.id
                 and a.id = case
                   when p.brand->>'logo_asset_id' ~* '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
                   then (p.brand->>'logo_asset_id')::uuid
                 end
          ) icon on true
         where p.owner_user_id = $1
         order by p.created_at desc
        "#,
    )
    .bind(&user.id)
    .fetch_all(&s.db)
    .await?;

    let mut projects = Vec::with_capacity(rows.len());
    for r in rows {
        let icon_url = match &r.icon_blob_path {
            Some(b) => match s
                .blob
                .public_read_url(b, Duration::minutes(ICON_URL_TTL_MINUTES))
                .await
            {
                Ok(u) => Some(u.to_string()),
                Err(e) => {
                    tracing::warn!(error = %e, project = %r.id, "icon url signing failed");
                    None
                }
            },
            None => None,
        };
        projects.push(MeProject {
            id: r.id,
            slug: r.slug,
            name: r.name,
            onboarding_step: r.onboarding_step,
            created_at: r.created_at,
            icon_url,
        });
    }

    Ok(Json(MeResponse {
        id: user.id,
        email: user.email,
        created_at,
        projects,
    }))
}
