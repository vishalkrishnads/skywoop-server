//! Multi-step onboarding: App Store URL → Meta → confirm details → context → done.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{Timelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::api::projects::{
    default_timezone_for_country, existing_app_store_asset, import_discovery_assets,
    validate_country,
};
use crate::auth::AuthUser;
use crate::domain::Platform;
use crate::domain::access::ensure_project_owner;
use crate::domain::onboarding::{app_store_url_parts, slugify};
use crate::domain::types::{BrandProfile, new_id, normalize_brand};
use crate::error::{AppError, AppResult};
use crate::jobs::{DataJob, DataJobQueue, UserJob};
use crate::state::AppState;

const FIRST_PLAN_CUTOFF_HOUR: u32 = 16;

#[derive(Debug, Deserialize, ToSchema)]
pub struct StartOnboardingRequest {
    pub app_store_url: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct StartOnboardingResponse {
    pub project_id: Uuid,
    pub discovery_id: Uuid,
    pub onboarding_step: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OnboardingDetailsView {
    pub project_id: Uuid,
    pub discovery_id: Option<Uuid>,
    /// pending | running | ready | failed
    pub status: String,
    pub draft: Option<Value>,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MediaRef {
    /// Keep a discovery preview blob (path under `discoveries/…`).
    Discovery { blob_path: String },
    /// Use an already-finalized uploaded asset.
    Asset { asset_id: Uuid },
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ConfirmDetailsRequest {
    pub name: String,
    pub description: String,
    pub icon: Option<MediaRef>,
    #[serde(default)]
    pub screenshots: Vec<MediaRef>,
    /// Brand colors as `#rrggbb`; omitted colors keep the current (or discovered) value.
    pub primary_color: Option<String>,
    pub secondary_color: Option<String>,
    pub tertiary_color: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ContextRequest {
    #[serde(default)]
    pub notes: String,
    /// Assets already uploaded into Inbox/ (via the normal upload flow).
    #[serde(default)]
    pub asset_ids: Vec<Uuid>,
    /// Optional poster frames keyed by video asset id.
    #[serde(default)]
    pub posters: Vec<PosterRef>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PosterRef {
    pub asset_id: Uuid,
    pub poster_asset_id: Uuid,
}

/// `app-{app_id}`, or `app-{app_id}-{short id}` when another project already holds it.
async fn unique_app_slug(
    tx: &mut sqlx::PgConnection,
    app_id: &str,
    project_id: Uuid,
) -> AppResult<String> {
    let base = format!("app-{app_id}");
    let simple = project_id.simple().to_string();
    for candidate in [
        base.clone(),
        format!("{base}-{}", &simple[simple.len() - 6..]),
        format!("{base}-{simple}"),
    ] {
        let taken: bool = sqlx::query_scalar(
            "select exists(select 1 from projects where slug = $1 and id <> $2)",
        )
        .bind(&candidate)
        .bind(project_id)
        .fetch_one(&mut *tx)
        .await?;
        if !taken {
            return Ok(candidate);
        }
    }
    Err(AppError::Conflict(format!("slug {base} is taken")))
}

/// Insert a discovery row for the project and queue `DiscoverProduct`.
async fn queue_discovery(
    s: &AppState,
    tx: &mut sqlx::PgConnection,
    project_id: Uuid,
    url: &str,
) -> AppResult<Uuid> {
    let discovery_id = new_id();
    sqlx::query(
        "insert into project_discoveries (id, url, status, project_id) values ($1, $2, 'pending', $3)",
    )
    .bind(discovery_id)
    .bind(url)
    .bind(project_id)
    .execute(&mut *tx)
    .await?;
    let job_id = s
        .user_jobs
        .enqueue(
            &mut *tx,
            &UserJob::DiscoverProduct { discovery_id },
            Some(format!("discover:{discovery_id}")),
        )
        .await?
        .ok_or_else(|| AppError::Other(anyhow::anyhow!("failed to enqueue discovery")))?;
    sqlx::query("update project_discoveries set job_id = $2 where id = $1")
        .bind(discovery_id)
        .bind(job_id)
        .execute(&mut *tx)
        .await?;
    Ok(discovery_id)
}

#[derive(sqlx::FromRow)]
struct OnboardingProject {
    id: Uuid,
    app_store_app_id: Option<String>,
    onboarding_step: String,
    slug: String,
    name: String,
}

#[utoipa::path(
    post,
    path = "/onboarding/projects",
    request_body = StartOnboardingRequest,
    responses(
        (status = 201, description = "Project created; discovery queued", body = StartOnboardingResponse),
        (status = 200, description = "The caller's project still in onboarding (discovery re-queued when the App Store id changed)", body = StartOnboardingResponse)
    )
)]
pub async fn start(
    State(s): State<AppState>,
    user: AuthUser,
    Json(body): Json<StartOnboardingRequest>,
) -> Result<(StatusCode, Json<StartOnboardingResponse>), AppError> {
    let raw = body.app_store_url.trim();
    let candidate = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    };
    let (app_id, country) = app_store_url_parts(&candidate).ok_or_else(|| {
        AppError::Invalid("app_store_url must be an apps.apple.com listing URL".into())
    })?;
    let market_country = validate_country(country.as_deref().unwrap_or("US"))?;
    let timezone = default_timezone_for_country(&market_country)
        .unwrap_or("UTC")
        .to_string();
    let placeholder_name = format!("App {app_id}");

    let mut tx = s.db.begin().await?;
    // One onboarding project per user: serialize concurrent starts.
    sqlx::query("select pg_advisory_xact_lock(hashtext($1))")
        .bind(format!("onboarding:{}", user.id))
        .execute(&mut *tx)
        .await?;

    let existing = sqlx::query_as::<_, OnboardingProject>(
        "select id, app_store_app_id, onboarding_step, slug, name from projects \
          where owner_user_id = $1 and onboarding_step <> 'complete' \
          order by created_at desc limit 1 for update",
    )
    .bind(&user.id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(p) = existing {
        let latest: Option<Uuid> = sqlx::query_scalar(
            "select id from project_discoveries where project_id = $1 \
              order by created_at desc limit 1",
        )
        .bind(p.id)
        .fetch_optional(&mut *tx)
        .await?;
        let discovery_id = match (
            p.app_store_app_id.as_deref() == Some(app_id.as_str()),
            latest,
        ) {
            (true, Some(d)) => d,
            (true, None) => queue_discovery(&s, &mut tx, p.id, &candidate).await?,
            (false, _) => {
                let old_app = p.app_store_app_id.clone().unwrap_or_default();
                let slug = if p.slug.starts_with(&format!("app-{old_app}")) {
                    unique_app_slug(&mut tx, &app_id, p.id).await?
                } else {
                    p.slug.clone()
                };
                let name = if p.name == format!("App {old_app}") {
                    placeholder_name.clone()
                } else {
                    p.name.clone()
                };
                sqlx::query(
                    "update projects set app_store_app_id = $2, slug = $3, name = $4, \
                            market_country = $5, timezone = $6, updated_at = now() \
                      where id = $1",
                )
                .bind(p.id)
                .bind(&app_id)
                .bind(&slug)
                .bind(&name)
                .bind(&market_country)
                .bind(&timezone)
                .execute(&mut *tx)
                .await?;
                queue_discovery(&s, &mut tx, p.id, &candidate).await?
            }
        };
        tx.commit().await?;
        return Ok((
            StatusCode::OK,
            Json(StartOnboardingResponse {
                project_id: p.id,
                discovery_id,
                onboarding_step: p.onboarding_step,
            }),
        ));
    }

    let project_id = new_id();
    let slug = unique_app_slug(&mut tx, &app_id, project_id).await?;

    sqlx::query(
        r#"
        insert into projects
            (id, slug, name, description, website, timezone, market_country,
             app_store_app_id, owner_user_id, onboarding_step)
        values ($1, $2, $3, '', null, $4, $5, $6, $7, 'meta')
        "#,
    )
    .bind(project_id)
    .bind(&slug)
    .bind(&placeholder_name)
    .bind(&timezone)
    .bind(&market_country)
    .bind(&app_id)
    .bind(&user.id)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        if let sqlx::Error::Database(db) = &e
            && db.constraint() == Some("projects_slug_key")
        {
            return AppError::Conflict("project slug was taken concurrently; retry".into());
        }
        AppError::from(e)
    })?;

    for platform in ["instagram", "facebook"] {
        sqlx::query("insert into platform_workspaces (project_id, platform) values ($1, $2)")
            .bind(project_id)
            .bind(platform)
            .execute(&mut *tx)
            .await?;
    }

    crate::domain::assets::seed_dirs(&mut tx, project_id).await?;

    let discovery_id = queue_discovery(&s, &mut tx, project_id, &candidate).await?;

    tx.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(StartOnboardingResponse {
            project_id,
            discovery_id,
            onboarding_step: "meta".into(),
        }),
    ))
}

#[utoipa::path(
    get,
    path = "/projects/{pid}/onboarding/details",
    params(("pid" = Uuid, Path, description = "Project id")),
    responses((status = 200, body = OnboardingDetailsView))
)]
pub async fn get_details(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
) -> AppResult<Json<OnboardingDetailsView>> {
    ensure_project_owner(&s.db, &user.id, pid).await?;

    let row: Option<(Uuid, String, String, Option<Value>, Option<String>)> = sqlx::query_as(
        "select id, url, status, draft, error from project_discoveries \
         where project_id = $1 order by created_at desc limit 1",
    )
    .bind(pid)
    .fetch_optional(&s.db)
    .await?;

    let Some((discovery_id, _url, status, mut draft, error)) = row else {
        return Ok(Json(OnboardingDetailsView {
            project_id: pid,
            discovery_id: None,
            status: "pending".into(),
            draft: None,
            error: None,
        }));
    };

    if let Some(assets) = draft
        .as_mut()
        .and_then(|d| d.get_mut("preview_assets"))
        .and_then(Value::as_array_mut)
    {
        for a in assets {
            let Some(path) = a
                .get("blob_path")
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                continue;
            };
            if let Ok(u) = s
                .blob
                .public_read_url(&path, chrono::Duration::hours(1))
                .await
                && let Some(obj) = a.as_object_mut()
            {
                obj.insert("preview_url".into(), Value::String(u.to_string()));
            }
        }
    }

    Ok(Json(OnboardingDetailsView {
        project_id: pid,
        discovery_id: Some(discovery_id),
        status,
        draft,
        error,
    }))
}

#[utoipa::path(
    put,
    path = "/projects/{pid}/onboarding/details",
    params(("pid" = Uuid, Path, description = "Project id")),
    request_body = ConfirmDetailsRequest,
    responses(
        (status = 200, description = "Details saved; step moves forward to context (never backward)"),
        (status = 409, description = "Onboarding already complete")
    )
)]
pub async fn put_details(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
    Json(body): Json<ConfirmDetailsRequest>,
) -> AppResult<Json<Value>> {
    ensure_project_owner(&s.db, &user.id, pid).await?;

    let (step, current_brand): (String, sqlx::types::Json<BrandProfile>) =
        sqlx::query_as("select onboarding_step, brand from projects where id = $1")
            .bind(pid)
            .fetch_one(&s.db)
            .await?;
    if !matches!(step.as_str(), "meta" | "details" | "context") {
        return Err(AppError::Conflict(format!(
            "details can only be edited during onboarding (step is {step})"
        )));
    }
    let current_brand = current_brand.0;

    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::Invalid("name is required".into()));
    }
    let description = body.description.trim().to_string();
    let mut slug = slugify(name);
    // Ensure slug uniqueness for this owner by appending short id on conflict.
    let slug_taken: bool =
        sqlx::query_scalar("select exists(select 1 from projects where slug = $1 and id <> $2)")
            .bind(&slug)
            .bind(pid)
            .fetch_one(&s.db)
            .await?;
    if slug_taken {
        slug = format!("{slug}-{}", &pid.to_string()[..8]);
    }

    let draft: Option<Value> = sqlx::query_scalar(
        "select draft from project_discoveries where project_id = $1 \
         and status = 'ready' order by created_at desc limit 1",
    )
    .bind(pid)
    .fetch_optional(&s.db)
    .await?
    .flatten();

    let mut tx = s.db.begin().await?;

    // Materialize discovery assets (icon/screenshots) on the first save; later saves keep
    // the brand chosen before. Imports reuse identical App Store files, so nothing duplicates.
    let mut brand = if current_brand.logo_asset_id.is_some() {
        current_brand
    } else if let Some(ref d) = draft {
        let imported = import_discovery_assets(&s, &mut tx, pid, d).await?;
        BrandProfile {
            primary_color: current_brand.primary_color.or(imported.primary_color),
            secondary_color: current_brand.secondary_color.or(imported.secondary_color),
            tertiary_color: current_brand.tertiary_color.or(imported.tertiary_color),
            font_family: current_brand.font_family.or(imported.font_family),
            ..imported
        }
    } else {
        current_brand
    };
    for (slot, color) in [
        (&mut brand.primary_color, &body.primary_color),
        (&mut brand.secondary_color, &body.secondary_color),
        (&mut brand.tertiary_color, &body.tertiary_color),
    ] {
        if let Some(c) = color {
            *slot = Some(c.clone());
        }
    }

    // Apply explicit icon / screenshot overrides from the client.
    if let Some(icon) = &body.icon {
        if let Some(id) = resolve_media_to_app_store(&s, &mut tx, pid, icon, "app-icon").await? {
            brand.logo_asset_id = Some(id);
        }
    }
    if !body.screenshots.is_empty() {
        let mut ids = Vec::new();
        for (i, shot) in body.screenshots.iter().enumerate() {
            if let Some(id) =
                resolve_media_to_app_store(&s, &mut tx, pid, shot, &format!("screenshot-{}", i + 1))
                    .await?
            {
                ids.push(id);
            }
        }
        if !ids.is_empty() {
            brand.screenshot_asset_ids = ids;
        }
    }

    let brand = normalize_brand(brand).map_err(AppError::Invalid)?;

    let new_step: String = sqlx::query_scalar(
        r#"
        update projects
           set name = $2,
               slug = $3,
               description = $4,
               brand = $5,
               onboarding_step = case
                 when onboarding_step in ('meta', 'details') then 'context'
                 else onboarding_step
               end,
               updated_at = now()
         where id = $1
        returning onboarding_step
        "#,
    )
    .bind(pid)
    .bind(name)
    .bind(&slug)
    .bind(&description)
    .bind(sqlx::types::Json(&brand))
    .fetch_one(&mut *tx)
    .await?;

    // Seed audiences from discovery suggestions when none exist yet.
    let existing: i64 = sqlx::query_scalar(
        "select count(*) from project_audiences where project_id = $1 and archived_at is null",
    )
    .bind(pid)
    .fetch_one(&mut *tx)
    .await?;
    if existing == 0 {
        let suggested = draft
            .as_ref()
            .and_then(|d| d.get("suggested_audiences"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for i in 0..5 {
            let (aname, adesc) = suggested
                .get(i)
                .map(|a| {
                    (
                        a.get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("Audience")
                            .to_string(),
                        a.get("description")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    )
                })
                .unwrap_or_else(|| {
                    (
                        format!("Audience segment {}", i + 1),
                        "Potential customers interested in this app.".into(),
                    )
                });
            sqlx::query(
                "insert into project_audiences (id, project_id, position, name, description) \
                 values ($1, $2, $3, $4, $5)",
            )
            .bind(new_id())
            .bind(pid)
            .bind((i as i32) + 1)
            .bind(aname.trim())
            .bind(adesc.trim())
            .execute(&mut *tx)
            .await?;
        }
    }

    s.user_jobs
        .enqueue(
            &mut *tx,
            &UserJob::ClassifyIndustry { project_id: pid },
            Some(format!("classify_industry:{pid}")),
        )
        .await?;

    tx.commit().await?;

    Ok(Json(serde_json::json!({
        "project_id": pid,
        "onboarding_step": new_step,
        "slug": slug,
    })))
}

async fn resolve_media_to_app_store(
    s: &AppState,
    tx: &mut sqlx::PgConnection,
    project_id: Uuid,
    media: &MediaRef,
    base_name: &str,
) -> AppResult<Option<Uuid>> {
    match media {
        MediaRef::Asset { asset_id } => {
            let owned: Option<(String, String)> =
                sqlx::query_as("select name, path from assets where id = $1 and project_id = $2")
                    .bind(asset_id)
                    .bind(project_id)
                    .fetch_optional(&mut *tx)
                    .await?;
            let Some((name, path)) = owned else {
                return Err(AppError::NotFound);
            };
            if path.starts_with("App Store/") {
                return Ok(Some(*asset_id));
            }
            // Move into App Store/ by updating dir_path via domain helper on pool —
            // within the transaction, update path directly.
            let new_path = format!("App Store/{name}");
            let dir_id: Option<Uuid> = sqlx::query_scalar(
                "select id from asset_dirs where project_id = $1 and path = 'App Store'",
            )
            .bind(project_id)
            .fetch_optional(&mut *tx)
            .await?;
            sqlx::query(
                "update assets set path = $2, dir_id = $3, updated_at = now() where id = $1",
            )
            .bind(asset_id)
            .bind(&new_path)
            .bind(dir_id)
            .execute(&mut *tx)
            .await?;
            Ok(Some(*asset_id))
        }
        MediaRef::Discovery { blob_path } => {
            if !blob_path.starts_with("discoveries/") || blob_path.contains("..") {
                return Err(AppError::Invalid("invalid discovery blob_path".into()));
            }
            let bytes = s.blob.get(blob_path).await.map_err(AppError::Other)?;
            if bytes.is_empty() {
                return Ok(None);
            }
            if let Some(id) = existing_app_store_asset(&mut *tx, project_id, &bytes).await? {
                return Ok(Some(id));
            }
            let mime = if blob_path.ends_with(".png") {
                "image/png"
            } else if blob_path.ends_with(".webp") {
                "image/webp"
            } else {
                "image/jpeg"
            };
            let ext = mime.rsplit('/').next().unwrap_or("jpg");
            let name = format!("{base_name}.{ext}");
            let asset = crate::api::assets::store_bytes(
                s,
                &mut *tx,
                project_id,
                "App Store",
                &name,
                mime,
                bytes,
                None,
                vec!["app store".into()],
                "",
            )
            .await?;
            Ok(Some(asset.id))
        }
    }
}

#[utoipa::path(
    post,
    path = "/projects/{pid}/onboarding/context",
    params(("pid" = Uuid, Path, description = "Project id")),
    request_body = ContextRequest,
    responses((status = 200, description = "Context saved; categorization queued"))
)]
pub async fn post_context(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
    Json(body): Json<ContextRequest>,
) -> AppResult<Json<Value>> {
    ensure_project_owner(&s.db, &user.id, pid).await?;
    let step: String = sqlx::query_scalar("select onboarding_step from projects where id = $1")
        .bind(pid)
        .fetch_one(&s.db)
        .await?;
    if step != "context" {
        return Err(AppError::Conflict(format!(
            "expected onboarding step context, got {step}"
        )));
    }

    let mut tx = s.db.begin().await?;
    sqlx::query("update projects set context_notes = $2, updated_at = now() where id = $1")
        .bind(pid)
        .bind(body.notes.trim())
        .execute(&mut *tx)
        .await?;

    let poster_map: std::collections::HashMap<Uuid, Uuid> = body
        .posters
        .iter()
        .map(|p| (p.asset_id, p.poster_asset_id))
        .collect();

    for asset_id in &body.asset_ids {
        let owned: Option<(Uuid,)> =
            sqlx::query_as("select id from assets where id = $1 and project_id = $2")
                .bind(asset_id)
                .bind(pid)
                .fetch_optional(&mut *tx)
                .await?;
        if owned.is_none() {
            return Err(AppError::NotFound);
        }
        let poster = poster_map.get(asset_id).copied();
        s.user_jobs
            .enqueue(
                &mut *tx,
                &UserJob::CategorizeAsset {
                    asset_id: *asset_id,
                    poster_asset_id: poster,
                },
                Some(format!("categorize_asset:{asset_id}")),
            )
            .await?;
    }

    tx.commit().await?;
    Ok(Json(serde_json::json!({
        "project_id": pid,
        "queued": body.asset_ids.len(),
    })))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DemoOnboardingResponse {
    pub project_id: Uuid,
    pub onboarding_step: String,
}

#[utoipa::path(
    post,
    path = "/onboarding/demo",
    responses(
        (status = 200, description = "Existing or newly cloned Haserupt demo project", body = DemoOnboardingResponse),
        (status = 409, description = "Demo template unavailable")
    )
)]
pub async fn start_demo(
    State(s): State<AppState>,
    user: AuthUser,
) -> AppResult<Json<DemoOnboardingResponse>> {
    let template_id = s.cfg.demo_template_project_id.ok_or_else(|| {
        AppError::Conflict("demo unavailable: DEMO_TEMPLATE_PROJECT_ID is not configured".into())
    })?;

    if let Some(existing) =
        crate::domain::demo::existing_demo(&s.db, &user.id, template_id).await?
    {
        return Ok(Json(DemoOnboardingResponse {
            project_id: existing,
            onboarding_step: "complete".into(),
        }));
    }

    let ig = crate::domain::demo::template_instagram(&s.db, template_id).await?;

    let mut tx = s.db.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtext($1))")
        .bind(format!("onboarding:{}", user.id))
        .execute(&mut *tx)
        .await?;

    // Another request may have cloned while we waited for the lock.
    if let Some(existing) = sqlx::query_scalar::<_, Uuid>(
        "select id from projects where owner_user_id = $1 and demo_of = $2 limit 1",
    )
    .bind(&user.id)
    .bind(template_id)
    .fetch_optional(&mut *tx)
    .await?
    {
        tx.commit().await?;
        return Ok(Json(DemoOnboardingResponse {
            project_id: existing,
            onboarding_step: "complete".into(),
        }));
    }

    let project_id = crate::domain::demo::clone_template(
        &mut tx,
        s.blob.as_ref(),
        template_id,
        &user.id,
        &ig,
    )
    .await?;
    tx.commit().await?;

    Ok(Json(DemoOnboardingResponse {
        project_id,
        onboarding_step: "complete".into(),
    }))
}

#[utoipa::path(
    post,
    path = "/projects/{pid}/onboarding/complete",
    params(("pid" = Uuid, Path, description = "Project id")),
    responses((status = 200, description = "Onboarding complete; first plans queued"))
)]
pub async fn complete(
    State(s): State<AppState>,
    user: AuthUser,
    Path(pid): Path<Uuid>,
) -> AppResult<Json<Value>> {
    ensure_project_owner(&s.db, &user.id, pid).await?;
    let (step, timezone): (String, String) =
        sqlx::query_as("select onboarding_step, timezone from projects where id = $1")
            .bind(pid)
            .fetch_one(&s.db)
            .await?;
    if step != "context" && step != "details" {
        // Allow complete from context (normal) or details if they skip extras.
        if step == "complete" {
            return Ok(Json(serde_json::json!({
                "project_id": pid,
                "onboarding_step": "complete",
            })));
        }
        if step == "meta" {
            return Err(AppError::Conflict(
                "finish Meta connection and details before completing".into(),
            ));
        }
    }

    let mut tx = s.db.begin().await?;
    sqlx::query(
        "update projects set onboarding_step = 'complete', updated_at = now() where id = $1",
    )
    .bind(pid)
    .execute(&mut *tx)
    .await?;

    let now_local = Utc::now().with_timezone(
        &timezone
            .parse::<chrono_tz::Tz>()
            .map_err(|_| AppError::Invalid(format!("unknown timezone {timezone}")))?,
    );
    let today = now_local.date_naive();
    let first_plan_date = if now_local.hour() < FIRST_PLAN_CUTOFF_HOUR {
        today
    } else {
        today + chrono::Duration::days(1)
    };
    let data_jobs = DataJobQueue::new(s.db.clone());
    for platform in [Platform::Instagram, Platform::Facebook] {
        data_jobs
            .enqueue(
                &mut *tx,
                &DataJob::DraftPlan {
                    project_id: pid,
                    platform,
                    for_date: first_plan_date,
                    allow_no_op: false,
                },
                Some(format!(
                    "draft_plan:{pid}:{}:{first_plan_date}",
                    platform.as_str()
                )),
                Utc::now(),
            )
            .await?;
    }

    tx.commit().await?;
    Ok(Json(serde_json::json!({
        "project_id": pid,
        "onboarding_step": "complete",
        "first_plan_date": first_plan_date.to_string(),
    })))
}
