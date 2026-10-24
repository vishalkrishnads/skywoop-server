use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, Timelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::api::plans::AudienceInput;
use crate::auth::AuthUser;
use crate::domain::Platform;
use crate::domain::access;
use crate::domain::plan::{ProjectAudience, list_active_audiences};
use crate::domain::types::{BrandProfile, FONT_FAMILIES, new_id, normalize_brand};
use crate::error::{AppError, AppResult};
use crate::jobs::{DataJob, DataJobQueue, UserJob};
use crate::state::AppState;

/// Before this local hour a new project gets a plan for today; afterwards for tomorrow.
const FIRST_PLAN_CUTOFF_HOUR: u32 = 16;

#[derive(Debug, Serialize, FromRow, ToSchema)]
pub struct ProjectSummary {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Project {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub website: Option<String>,
    pub timezone: String,
    pub market_country: String,
    pub industry: Option<String>,
    pub app_store_app_id: Option<String>,
    pub asc_provider_token: Option<String>,
    pub brand: BrandProfile,
    pub audiences: Vec<ProjectAudience>,
    pub onboarding_step: String,
    pub owner_user_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, FromRow)]
struct ProjectRow {
    id: Uuid,
    slug: String,
    name: String,
    description: String,
    website: Option<String>,
    timezone: String,
    market_country: String,
    industry: Option<String>,
    app_store_app_id: Option<String>,
    asc_provider_token: Option<String>,
    brand: sqlx::types::Json<BrandProfile>,
    onboarding_step: String,
    owner_user_id: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<ProjectRow> for Project {
    fn from(r: ProjectRow) -> Self {
        Self {
            id: r.id,
            slug: r.slug,
            name: r.name,
            description: r.description,
            website: r.website,
            timezone: r.timezone,
            market_country: r.market_country,
            industry: r.industry,
            app_store_app_id: r.app_store_app_id,
            asc_provider_token: r.asc_provider_token,
            brand: r.brand.0,
            audiences: Vec::new(),
            onboarding_step: r.onboarding_step,
            owner_user_id: r.owner_user_id,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateProjectRequest {
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub website: Option<String>,
    /// IANA timezone. When omitted, the server picks the country's `default_timezone`.
    pub timezone: Option<String>,
    /// ISO 3166-1 alpha-2 market (trends, App Store lookups). Defaults to `US`.
    pub market_country: Option<String>,
    /// A `ready` discovery (see `POST /projects/discover`) to seed brand, assets and metadata.
    pub discovery_id: Option<Uuid>,
    /// Exactly five target audiences for daily planning.
    pub audiences: Vec<AudienceInput>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PatchProjectRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub website: Option<Option<String>>,
    pub timezone: Option<String>,
    pub market_country: Option<String>,
    /// Override industry id (see docs/times.md). Reseeds schedule unless agent/manual.
    pub industry: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AppStoreRequest {
    pub app_store_app_id: String,
    pub asc_provider_token: String,
}

#[utoipa::path(
    get,
    path = "/timezones",
    responses((status = 200, description = "IANA timezone names", body = [String]))
)]
pub async fn list_timezones() -> Json<Vec<&'static str>> {
    let mut names: Vec<&str> = chrono_tz::TZ_VARIANTS.iter().map(|tz| tz.name()).collect();
    names.sort_unstable();
    names.dedup();
    Json(names)
}

#[utoipa::path(
    get,
    path = "/font-families",
    responses((status = 200, description = "Font families the brand form may select", body = [String]))
)]
pub async fn list_font_families() -> Json<&'static [&'static str]> {
    Json(FONT_FAMILIES)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct DiscoverRequest {
    pub url: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DiscoverResponse {
    pub discovery_id: Uuid,
    pub job_id: Uuid,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DiscoveryView {
    pub id: Uuid,
    pub url: String,
    /// pending | running | ready | failed
    pub status: String,
    /// Project draft: name, description, website, app_store_app_id, colors, font_family,
    /// preview_assets (each with a short-lived `preview_url` when available).
    pub draft: Option<Value>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Country {
    pub code: &'static str,
    pub name: &'static str,
    /// Primary IANA timezone for this market. UI should auto-select this when the country changes.
    pub default_timezone: &'static str,
}

#[utoipa::path(
    post,
    path = "/projects/discover",
    request_body = DiscoverRequest,
    responses((status = 202, description = "Discovery queued", body = DiscoverResponse))
)]
pub async fn discover(
    State(s): State<AppState>,
    _user: AuthUser,
    Json(body): Json<DiscoverRequest>,
) -> Result<(StatusCode, Json<DiscoverResponse>), AppError> {
    let raw = body.url.trim();
    let candidate = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    };
    let parsed = url::Url::parse(&candidate)
        .map_err(|_| AppError::Invalid(format!("invalid url: {}", body.url)))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(AppError::Invalid("url must be http(s)".into()));
    }

    let discovery_id = new_id();
    let mut tx = s.db.begin().await?;
    sqlx::query("insert into project_discoveries (id, url, status) values ($1, $2, 'pending')")
        .bind(discovery_id)
        .bind(parsed.as_str())
        .execute(&mut *tx)
        .await?;
    let job_id = s
        .user_jobs
        .enqueue(&mut *tx, &UserJob::DiscoverProduct { discovery_id }, None)
        .await?
        .ok_or_else(|| AppError::Other(anyhow::anyhow!("failed to enqueue job")))?;
    sqlx::query("update project_discoveries set job_id = $2 where id = $1")
        .bind(discovery_id)
        .bind(job_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    Ok((
        StatusCode::ACCEPTED,
        Json(DiscoverResponse {
            discovery_id,
            job_id,
        }),
    ))
}

#[utoipa::path(
    get,
    path = "/projects/discoveries/{id}",
    params(("id" = Uuid, Path, description = "Discovery id")),
    responses((status = 200, description = "Discovery", body = DiscoveryView), (status = 404, description = "Not found"))
)]
pub async fn get_discovery(
    State(s): State<AppState>,
    _user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<DiscoveryView>> {
    let (url, status, mut draft, error): (String, String, Option<Value>, Option<String>) =
        sqlx::query_as("select url, status, draft, error from project_discoveries where id = $1")
            .bind(id)
            .fetch_one(&s.db)
            .await?;

    // Best-effort short-lived URLs so clients can preview downloaded assets.
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

    Ok(Json(DiscoveryView {
        id,
        url,
        status,
        draft,
        error,
    }))
}

const COUNTRIES: &[Country] = &[
    Country {
        code: "US",
        name: "United States",
        default_timezone: "America/New_York",
    },
    Country {
        code: "GB",
        name: "United Kingdom",
        default_timezone: "Europe/London",
    },
    Country {
        code: "CA",
        name: "Canada",
        default_timezone: "America/Toronto",
    },
    Country {
        code: "AU",
        name: "Australia",
        default_timezone: "Australia/Sydney",
    },
    Country {
        code: "NZ",
        name: "New Zealand",
        default_timezone: "Pacific/Auckland",
    },
    Country {
        code: "IE",
        name: "Ireland",
        default_timezone: "Europe/Dublin",
    },
    Country {
        code: "DE",
        name: "Germany",
        default_timezone: "Europe/Berlin",
    },
    Country {
        code: "FR",
        name: "France",
        default_timezone: "Europe/Paris",
    },
    Country {
        code: "ES",
        name: "Spain",
        default_timezone: "Europe/Madrid",
    },
    Country {
        code: "IT",
        name: "Italy",
        default_timezone: "Europe/Rome",
    },
    Country {
        code: "NL",
        name: "Netherlands",
        default_timezone: "Europe/Amsterdam",
    },
    Country {
        code: "SE",
        name: "Sweden",
        default_timezone: "Europe/Stockholm",
    },
    Country {
        code: "NO",
        name: "Norway",
        default_timezone: "Europe/Oslo",
    },
    Country {
        code: "DK",
        name: "Denmark",
        default_timezone: "Europe/Copenhagen",
    },
    Country {
        code: "FI",
        name: "Finland",
        default_timezone: "Europe/Helsinki",
    },
    Country {
        code: "PL",
        name: "Poland",
        default_timezone: "Europe/Warsaw",
    },
    Country {
        code: "PT",
        name: "Portugal",
        default_timezone: "Europe/Lisbon",
    },
    Country {
        code: "CH",
        name: "Switzerland",
        default_timezone: "Europe/Zurich",
    },
    Country {
        code: "AT",
        name: "Austria",
        default_timezone: "Europe/Vienna",
    },
    Country {
        code: "BE",
        name: "Belgium",
        default_timezone: "Europe/Brussels",
    },
    Country {
        code: "IN",
        name: "India",
        default_timezone: "Asia/Kolkata",
    },
    Country {
        code: "JP",
        name: "Japan",
        default_timezone: "Asia/Tokyo",
    },
    Country {
        code: "KR",
        name: "South Korea",
        default_timezone: "Asia/Seoul",
    },
    Country {
        code: "SG",
        name: "Singapore",
        default_timezone: "Asia/Singapore",
    },
    Country {
        code: "ID",
        name: "Indonesia",
        default_timezone: "Asia/Jakarta",
    },
    Country {
        code: "PH",
        name: "Philippines",
        default_timezone: "Asia/Manila",
    },
    Country {
        code: "AE",
        name: "United Arab Emirates",
        default_timezone: "Asia/Dubai",
    },
    Country {
        code: "SA",
        name: "Saudi Arabia",
        default_timezone: "Asia/Riyadh",
    },
    Country {
        code: "TR",
        name: "Turkey",
        default_timezone: "Europe/Istanbul",
    },
    Country {
        code: "ZA",
        name: "South Africa",
        default_timezone: "Africa/Johannesburg",
    },
    Country {
        code: "NG",
        name: "Nigeria",
        default_timezone: "Africa/Lagos",
    },
    Country {
        code: "BR",
        name: "Brazil",
        default_timezone: "America/Sao_Paulo",
    },
    Country {
        code: "MX",
        name: "Mexico",
        default_timezone: "America/Mexico_City",
    },
    Country {
        code: "AR",
        name: "Argentina",
        default_timezone: "America/Argentina/Buenos_Aires",
    },
    Country {
        code: "CO",
        name: "Colombia",
        default_timezone: "America/Bogota",
    },
    Country {
        code: "CL",
        name: "Chile",
        default_timezone: "America/Santiago",
    },
];

/// Primary timezone for a market country code, if it is in [`COUNTRIES`].
pub fn default_timezone_for_country(code: &str) -> Option<&'static str> {
    let upper = code.trim().to_ascii_uppercase();
    COUNTRIES
        .iter()
        .find(|c| c.code == upper)
        .map(|c| c.default_timezone)
}

#[utoipa::path(
    get,
    path = "/countries",
    responses((status = 200, description = "Common ISO 3166-1 alpha-2 market countries", body = [Country]))
)]
pub async fn list_countries() -> Json<&'static [Country]> {
    Json(COUNTRIES)
}

fn validate_timezone(tz: &str) -> AppResult<()> {
    tz.parse::<chrono_tz::Tz>()
        .map(|_| ())
        .map_err(|_| AppError::Invalid(format!("invalid timezone: {tz}")))
}

/// ISO 3166-1 alpha-2 shape check; returns the upper-cased code.
pub fn validate_country(code: &str) -> AppResult<String> {
    let c = code.trim().to_ascii_uppercase();
    if COUNTRIES.iter().any(|country| country.code == c) {
        Ok(c)
    } else {
        Err(AppError::Invalid(format!(
            "unknown market_country: {code} (see GET /countries)"
        )))
    }
}

fn map_db_err(e: sqlx::Error) -> AppError {
    if let sqlx::Error::Database(db) = &e
        && db.constraint() == Some("projects_slug_key")
    {
        return AppError::Conflict("slug already exists".into());
    }
    AppError::from(e)
}

async fn fetch_project(db: &sqlx::PgPool, id: Uuid) -> AppResult<Project> {
    let row = sqlx::query_as::<_, ProjectRow>(
        r#"
        select id, slug, name, description, website, timezone, market_country, industry,
               app_store_app_id, asc_provider_token, brand, onboarding_step, owner_user_id,
               created_at, updated_at
          from projects
         where id = $1
        "#,
    )
    .bind(id)
    .fetch_one(db)
    .await?;
    let mut project: Project = row.into();
    project.audiences = list_active_audiences(db, id)
        .await
        .map_err(AppError::Other)?;
    Ok(project)
}

#[utoipa::path(
    get,
    path = "/projects",
    responses((status = 200, description = "List projects", body = [ProjectSummary]))
)]
pub async fn list(
    State(s): State<AppState>,
    user: AuthUser,
) -> AppResult<Json<Vec<ProjectSummary>>> {
    let rows = sqlx::query_as::<_, ProjectSummary>(
        "select id, slug, name from projects where owner_user_id = $1 order by created_at desc",
    )
    .bind(&user.id)
    .fetch_all(&s.db)
    .await?;
    Ok(Json(rows))
}

#[utoipa::path(
    post,
    path = "/projects",
    request_body = CreateProjectRequest,
    responses((status = 201, description = "Created", body = Project))
)]
pub async fn create(
    State(s): State<AppState>,
    user: AuthUser,
    Json(body): Json<CreateProjectRequest>,
) -> Result<(StatusCode, Json<Project>), AppError> {
    if body.slug.trim().is_empty() || body.name.trim().is_empty() {
        return Err(AppError::Invalid("slug and name are required".into()));
    }
    if body.audiences.len() != 5 {
        return Err(AppError::Invalid("exactly 5 audiences are required".into()));
    }
    for aud in &body.audiences {
        if aud.name.trim().is_empty() {
            return Err(AppError::Invalid("each audience needs a name".into()));
        }
    }

    // Discovery draft (must be ready) seeds defaults the request doesn't override.
    let draft: Option<Value> = match body.discovery_id {
        Some(did) => {
            let row: Option<(String, Option<Value>)> =
                sqlx::query_as("select status, draft from project_discoveries where id = $1")
                    .bind(did)
                    .fetch_optional(&s.db)
                    .await?;
            let (status, draft) = row.ok_or(AppError::NotFound)?;
            if status != "ready" {
                return Err(AppError::Conflict(format!(
                    "discovery is {status}; it must be ready before creating a project"
                )));
            }
            Some(draft.ok_or_else(|| AppError::Conflict("discovery has no draft".into()))?)
        }
        None => None,
    };
    let draft_str = |key: &str| {
        draft
            .as_ref()
            .and_then(|d| d.get(key))
            .and_then(Value::as_str)
            .filter(|v| !v.trim().is_empty())
            .map(str::to_string)
    };

    let market_country = validate_country(
        &body
            .market_country
            .clone()
            .or_else(|| draft_str("market_country"))
            .unwrap_or_else(|| "US".into()),
    )?;
    let timezone = match body
        .timezone
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(tz) => {
            validate_timezone(tz)?;
            tz.to_string()
        }
        None => default_timezone_for_country(&market_country)
            .unwrap_or("UTC")
            .to_string(),
    };
    let description = if body.description.trim().is_empty() {
        draft_str("description").unwrap_or_default()
    } else {
        body.description.clone()
    };
    let website = body.website.clone().or_else(|| draft_str("website"));
    let app_store_app_id = draft_str("app_store_app_id");

    let id = new_id();
    let mut tx = s.db.begin().await?;

    sqlx::query(
        r#"
        insert into projects
            (id, slug, name, description, website, timezone, market_country, app_store_app_id, owner_user_id)
        values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        "#,
    )
    .bind(id)
    .bind(body.slug.trim())
    .bind(body.name.trim())
    .bind(&description)
    .bind(&website)
    .bind(&timezone)
    .bind(&market_country)
    .bind(&app_store_app_id)
    .bind(&user.id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_err)?;

    for platform in ["instagram", "facebook"] {
        sqlx::query(
            r#"
            insert into platform_workspaces (project_id, platform)
            values ($1, $2)
            "#,
        )
        .bind(id)
        .bind(platform)
        .execute(&mut *tx)
        .await?;
    }

    for (i, aud) in body.audiences.iter().enumerate() {
        let aid = new_id();
        let position = (i as i32) + 1;
        sqlx::query(
            r#"
            insert into project_audiences (id, project_id, position, name, description)
            values ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(aid)
        .bind(id)
        .bind(position)
        .bind(aud.name.trim())
        .bind(aud.description.trim())
        .execute(&mut *tx)
        .await?;
    }

    crate::domain::assets::seed_dirs(&mut tx, id).await?;

    if let Some(draft) = &draft {
        let brand = import_discovery_assets(&s, &mut tx, id, draft).await?;
        let brand = normalize_brand(brand).map_err(AppError::Invalid)?;
        sqlx::query("update projects set brand = $2 where id = $1")
            .bind(id)
            .bind(sqlx::types::Json(&brand))
            .execute(&mut *tx)
            .await?;
    }

    s.user_jobs
        .enqueue(
            &mut *tx,
            &UserJob::ClassifyIndustry { project_id: id },
            Some(format!("classify_industry:{id}")),
        )
        .await?;

    // First plan: today when it is still early in the project's day, otherwise tomorrow.
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
                    project_id: id,
                    platform,
                    for_date: first_plan_date,
                    allow_no_op: false,
                },
                Some(format!(
                    "draft_plan:{id}:{}:{first_plan_date}",
                    platform.as_str()
                )),
                Utc::now(),
            )
            .await?;
    }

    tx.commit().await?;

    let project = fetch_project(&s.db, id).await?;
    Ok((StatusCode::CREATED, Json(project)))
}

/// An asset under `App Store/` with exactly these bytes, so re-imports reuse it.
pub(crate) async fn existing_app_store_asset(
    conn: &mut sqlx::PgConnection,
    project_id: Uuid,
    bytes: &[u8],
) -> AppResult<Option<Uuid>> {
    use sha2::{Digest, Sha256};
    let sha = hex::encode(Sha256::digest(bytes));
    Ok(sqlx::query_scalar(
        "select id from assets \
          where project_id = $1 and sha256 = $2 and path like 'App Store/%' \
          order by created_at limit 1",
    )
    .bind(project_id)
    .bind(sha)
    .fetch_optional(conn)
    .await?)
}

/// Copy a discovery's preview blobs into the project's `App Store/` directory (with the
/// descriptions generated at discovery time) and build the brand profile
/// (colors, font, logo, screenshots) from its draft.
pub(crate) async fn import_discovery_assets(
    s: &AppState,
    tx: &mut sqlx::PgConnection,
    project_id: Uuid,
    draft: &Value,
) -> AppResult<BrandProfile> {
    let color = |k: &str| draft.get(k).and_then(Value::as_str).map(str::to_string);
    let mut brand = BrandProfile {
        primary_color: color("primary_color"),
        secondary_color: color("secondary_color"),
        tertiary_color: color("tertiary_color"),
        font_family: color("font_family"),
        ..Default::default()
    };

    let previews = draft
        .get("preview_assets")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // (draft kind, asset id) for every preview we managed to copy.
    let mut copied: Vec<(String, Uuid)> = Vec::new();
    let mut shot_n = 0;
    for p in &previews {
        let (Some(kind), Some(src_path), Some(mime)) = (
            p.get("kind").and_then(Value::as_str),
            p.get("blob_path").and_then(Value::as_str),
            p.get("mime").and_then(Value::as_str),
        ) else {
            continue;
        };
        // Only ever read from the discovery namespace.
        if !src_path.starts_with("discoveries/") || src_path.contains("..") {
            continue;
        }
        let bytes = match s.blob.get(src_path).await {
            Ok(b) if !b.is_empty() => b,
            Ok(_) => continue,
            Err(e) => {
                tracing::warn!(error = %e, src_path, "discovery preview missing; skipping");
                continue;
            }
        };
        let ext = ext_for_mime(mime, Some(src_path));
        let (name, base_tags) = if kind == "screenshot" {
            shot_n += 1;
            (
                format!("screenshot-{shot_n}.{ext}"),
                vec!["screenshot", "app store"],
            )
        } else {
            (format!("app-icon.{ext}"), vec!["icon", "logo", "app store"])
        };
        let description = p
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .filter(|d| !d.trim().is_empty())
            .or_else(|| {
                Some(if kind == "screenshot" {
                    "App Store listing screenshot.".to_owned()
                } else {
                    "App icon from the App Store listing.".to_owned()
                })
            });
        if let Some(id) = existing_app_store_asset(&mut *tx, project_id, &bytes).await? {
            copied.push((kind.to_string(), id));
            continue;
        }
        let mut tags: Vec<String> = base_tags.into_iter().map(str::to_owned).collect();
        if let Some(extra) = p.get("tags").and_then(Value::as_array) {
            tags.extend(extra.iter().filter_map(Value::as_str).map(str::to_owned));
        }
        let asset = crate::api::assets::store_bytes(
            s,
            &mut *tx,
            project_id,
            "App Store",
            &name,
            mime,
            bytes,
            description,
            tags,
            "",
        )
        .await?;
        copied.push((kind.to_string(), asset.id));
    }

    // Logo: icon / app_icon / logo, else the first copied asset.
    let logo = copied
        .iter()
        .find(|(k, _)| matches!(k.as_str(), "icon" | "app_icon" | "logo"))
        .or_else(|| copied.first())
        .map(|(_, id)| *id);
    brand.logo_asset_id = logo;
    brand.screenshot_asset_ids = copied
        .iter()
        .filter(|(k, id)| k == "screenshot" && Some(*id) != logo)
        .map(|(_, id)| *id)
        .collect();
    Ok(brand)
}

#[utoipa::path(
    get,
    path = "/projects/{id}",
    params(("id" = Uuid, Path, description = "Project id")),
    responses((status = 200, description = "Project", body = Project), (status = 404, description = "Not found"))
)]
pub async fn get(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Project>> {
    access::ensure_project_owner(&s.db, &user.id, id).await?;
    Ok(Json(fetch_project(&s.db, id).await?))
}

#[utoipa::path(
    patch,
    path = "/projects/{id}",
    params(("id" = Uuid, Path, description = "Project id")),
    request_body = PatchProjectRequest,
    responses((status = 200, description = "Updated", body = Project), (status = 404, description = "Not found"))
)]
pub async fn patch(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<PatchProjectRequest>,
) -> AppResult<Json<Project>> {
    access::ensure_project_owner(&s.db, &user.id, id).await?;
    let existing = fetch_project(&s.db, id).await?;
    let name = body.name.as_deref().unwrap_or(&existing.name);
    let description = body.description.as_deref().unwrap_or(&existing.description);
    let website = match &body.website {
        Some(w) => w.clone(),
        None => existing.website.clone(),
    };
    let market_country = match body.market_country.as_deref() {
        Some(c) => validate_country(c)?,
        None => existing.market_country.clone(),
    };
    // Country change without an explicit timezone → pick that country's default.
    let timezone = match body
        .timezone
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(tz) => {
            validate_timezone(tz)?;
            tz.to_string()
        }
        None if body.market_country.is_some() => default_timezone_for_country(&market_country)
            .unwrap_or(existing.timezone.as_str())
            .to_string(),
        None => existing.timezone.clone(),
    };
    validate_timezone(&timezone)?;

    let industry = match body
        .industry
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(raw) => {
            let ind = crate::scheduling::industries::Industry::parse(raw).ok_or_else(|| {
                AppError::Invalid(format!("unknown industry: {raw} (see GET /industries)"))
            })?;
            Some(ind.as_str().to_owned())
        }
        None => existing.industry.clone(),
    };

    sqlx::query(
        r#"
        update projects
           set name = $2,
               description = $3,
               website = $4,
               timezone = $5,
               market_country = $6,
               industry = $7,
               updated_at = now()
         where id = $1
        "#,
    )
    .bind(id)
    .bind(name)
    .bind(description)
    .bind(&website)
    .bind(&timezone)
    .bind(&market_country)
    .bind(&industry)
    .execute(&s.db)
    .await?;

    if let Some(ref ind_s) = body.industry
        && let Some(ind) = crate::scheduling::industries::Industry::parse(ind_s)
    {
        crate::scheduling::seed_from_industry(&s.db, id, ind)
            .await
            .map_err(AppError::Other)?;
    }

    Ok(Json(fetch_project(&s.db, id).await?))
}

#[utoipa::path(
    put,
    path = "/projects/{id}/brand",
    params(("id" = Uuid, Path, description = "Project id")),
    request_body = BrandProfile,
    responses((status = 200, description = "Brand updated", body = Project), (status = 404, description = "Not found"))
)]
pub async fn put_brand(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(brand): Json<BrandProfile>,
) -> AppResult<Json<Project>> {
    access::ensure_project_owner(&s.db, &user.id, id).await?;
    let brand = normalize_brand(brand).map_err(AppError::Invalid)?;
    sqlx::query(
        r#"
        update projects
           set brand = $2, updated_at = now()
         where id = $1
        "#,
    )
    .bind(id)
    .bind(sqlx::types::Json(&brand))
    .execute(&s.db)
    .await?;
    Ok(Json(fetch_project(&s.db, id).await?))
}

#[utoipa::path(
    put,
    path = "/projects/{id}/app-store",
    params(("id" = Uuid, Path, description = "Project id")),
    request_body = AppStoreRequest,
    responses((status = 200, description = "App Store settings updated", body = Project))
)]
pub async fn put_app_store(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<AppStoreRequest>,
) -> AppResult<Json<Project>> {
    access::ensure_project_owner(&s.db, &user.id, id).await?;
    sqlx::query(
        r#"
        update projects
           set app_store_app_id = $2,
               asc_provider_token = $3,
               updated_at = now()
         where id = $1
        "#,
    )
    .bind(id)
    .bind(body.app_store_app_id.trim())
    .bind(body.asc_provider_token.trim())
    .execute(&s.db)
    .await?;
    Ok(Json(fetch_project(&s.db, id).await?))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BioLinkView {
    pub campaign_token: String,
    pub app_store_url: String,
    pub instructions: String,
}

/// Instagram link-in-bio App Store URL (one per project; paste into IG bio).
#[utoipa::path(
    get,
    path = "/projects/{id}/bio-link",
    params(("id" = Uuid, Path, description = "Project id")),
    responses(
        (status = 200, description = "Bio link", body = BioLinkView),
        (status = 409, description = "App Store not configured"),
    )
)]
pub async fn bio_link(
    State(s): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> AppResult<Json<BioLinkView>> {
    access::ensure_project_owner(&s.db, &user.id, id).await?;
    let p = fetch_project(&s.db, id).await?;
    let app_id = p.app_store_app_id.as_deref().ok_or_else(|| {
        AppError::Conflict("Set App Store app id before using the bio link.".into())
    })?;
    let pt = p.asc_provider_token.as_deref().unwrap_or("000000");
    let token = crate::analysis::rollups::ig_bio_campaign_token(&p.slug);
    let url = crate::domain::links::app_store_destination(app_id, pt, &token);
    Ok(Json(BioLinkView {
        campaign_token: token,
        app_store_url: url,
        instructions: "Paste this App Store URL into your Instagram profile website/bio link. \
Organic posts cannot carry per-post links; App Store Connect attributes installs to this \
campaign token, and Skywoop apportions them across posts by profile visits / bio-link taps."
            .into(),
    }))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct IndustryView {
    pub id: String,
    pub name: String,
}

#[utoipa::path(
    get,
    path = "/industries",
    responses((status = 200, description = "Industry catalog", body = [IndustryView]))
)]
pub async fn list_industries() -> Json<Vec<IndustryView>> {
    Json(
        crate::scheduling::industries::Industry::ALL
            .iter()
            .map(|i| IndustryView {
                id: i.as_str().into(),
                name: i.display_name().into(),
            })
            .collect(),
    )
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ScheduleView {
    pub project_id: Uuid,
    pub platform: String,
    pub slots: Vec<crate::scheduling::Slot>,
    pub prime_windows: Vec<crate::scheduling::PrimeWindow>,
    pub dayparts: crate::scheduling::DaypartWindows,
    pub source: String,
    pub rationale: String,
    pub version: i32,
    pub upcoming: Vec<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PutScheduleRequest {
    pub slots: Option<Vec<crate::scheduling::Slot>>,
    pub prime_windows: Option<Vec<crate::scheduling::PrimeWindow>>,
    pub dayparts: Option<crate::scheduling::DaypartWindows>,
    #[serde(default)]
    pub rationale: String,
}

#[utoipa::path(
    get,
    path = "/projects/{id}/platforms/{platform}/schedule",
    params(
        ("id" = Uuid, Path, description = "Project id"),
        ("platform" = String, Path, description = "instagram|facebook"),
    ),
    responses((status = 200, description = "Posting schedule", body = ScheduleView))
)]
pub async fn get_schedule(
    State(s): State<AppState>,
    user: AuthUser,
    Path((id, platform)): Path<(Uuid, String)>,
) -> AppResult<Json<ScheduleView>> {
    access::ensure_project_ready(&s.db, &user.id, id).await?;
    let p = fetch_project(&s.db, id).await?;
    let platform = crate::domain::Platform::parse(&platform)
        .ok_or_else(|| AppError::Invalid("platform must be instagram or facebook".into()))?;
    let schedule = crate::scheduling::load_schedule_with_dayparts(&s.db, id, platform.as_str())
        .await
        .map_err(AppError::Other)?
        .ok_or_else(|| {
            AppError::Conflict(
                "No schedule yet — wait for industry classification or PUT one.".into(),
            )
        })?;
    let upcoming = crate::scheduling::upcoming_slots(&schedule.slots, &p.timezone, Utc::now(), 7)
        .map_err(AppError::Other)?;
    Ok(Json(ScheduleView {
        project_id: id,
        platform: platform.as_str().into(),
        slots: schedule.slots,
        prime_windows: schedule.prime_windows,
        dayparts: schedule.dayparts,
        source: schedule.source.as_str().into(),
        rationale: schedule.rationale,
        version: schedule.version,
        upcoming,
    }))
}

#[utoipa::path(
    put,
    path = "/projects/{id}/platforms/{platform}/schedule",
    params(
        ("id" = Uuid, Path, description = "Project id"),
        ("platform" = String, Path, description = "instagram|facebook"),
    ),
    request_body = PutScheduleRequest,
    responses((status = 200, description = "Schedule updated", body = ScheduleView))
)]
pub async fn put_schedule(
    State(s): State<AppState>,
    user: AuthUser,
    Path((id, platform)): Path<(Uuid, String)>,
    Json(body): Json<PutScheduleRequest>,
) -> AppResult<Json<ScheduleView>> {
    access::ensure_project_ready(&s.db, &user.id, id).await?;
    let p = fetch_project(&s.db, id).await?;
    let platform = crate::domain::Platform::parse(&platform)
        .ok_or_else(|| AppError::Invalid("platform must be instagram or facebook".into()))?;
    let max_daily: i32 = sqlx::query_scalar(
        r#"
        select max_daily_posts from platform_workspaces
         where project_id = $1 and platform = $2
        "#,
    )
    .bind(id)
    .bind(platform.as_str())
    .fetch_optional(&s.db)
    .await?
    .unwrap_or(4);

    let existing = crate::scheduling::load_schedule_with_dayparts(&s.db, id, platform.as_str())
        .await
        .map_err(AppError::Other)?;

    let slots = if let Some(slots) = &body.slots {
        crate::scheduling::validate_slots(slots, max_daily).map_err(AppError::Invalid)?;
        slots.clone()
    } else {
        existing
            .as_ref()
            .map(|e| e.slots.clone())
            .ok_or_else(|| AppError::Invalid("slots required when no schedule exists yet".into()))?
    };

    let prime_windows = body
        .prime_windows
        .clone()
        .or_else(|| existing.as_ref().map(|e| e.prime_windows.clone()))
        .unwrap_or_default();
    if !prime_windows.is_empty() {
        crate::scheduling::validate_prime_windows(&prime_windows).map_err(AppError::Invalid)?;
    }

    let dayparts = body
        .dayparts
        .clone()
        .or_else(|| existing.as_ref().map(|e| e.dayparts.clone()))
        .unwrap_or_default();
    crate::scheduling::validate_dayparts(&dayparts).map_err(AppError::Invalid)?;

    let rationale = if body.rationale.trim().is_empty() {
        "Manual schedule edit".to_owned()
    } else {
        body.rationale.clone()
    };

    let schedule = crate::scheduling::upsert_schedule_full(
        &s.db,
        id,
        platform.as_str(),
        &slots,
        &prime_windows,
        &dayparts,
        crate::scheduling::ScheduleSource::Manual,
        &rationale,
    )
    .await
    .map_err(AppError::Other)?;
    let upcoming = crate::scheduling::upcoming_slots(&schedule.slots, &p.timezone, Utc::now(), 7)
        .map_err(AppError::Other)?;
    Ok(Json(ScheduleView {
        project_id: id,
        platform: platform.as_str().into(),
        slots: schedule.slots,
        prime_windows: schedule.prime_windows,
        dayparts: schedule.dayparts,
        source: schedule.source.as_str().into(),
        rationale: schedule.rationale,
        version: schedule.version,
        upcoming,
    }))
}

fn guess_mime(filename: Option<&str>) -> Option<&'static str> {
    let name = filename?.to_ascii_lowercase();
    if name.ends_with(".png") {
        Some("image/png")
    } else if name.ends_with(".jpg") || name.ends_with(".jpeg") {
        Some("image/jpeg")
    } else if name.ends_with(".webp") {
        Some("image/webp")
    } else if name.ends_with(".gif") {
        Some("image/gif")
    } else {
        None
    }
}

fn ext_for_mime(mime: &str, filename: Option<&str>) -> String {
    match mime {
        "image/png" => "png".into(),
        "image/jpeg" => "jpg".into(),
        "image/webp" => "webp".into(),
        "image/gif" => "gif".into(),
        _ => filename
            .and_then(|f| f.rsplit('.').next())
            .filter(|e| e.len() <= 8 && e.chars().all(|c| c.is_ascii_alphanumeric()))
            .unwrap_or("bin")
            .to_ascii_lowercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn country_validation() {
        assert_eq!(validate_country(" gb ").unwrap(), "GB");
        assert!(validate_country("USA").is_err());
        assert!(validate_country("1A").is_err());
        assert!(validate_country("ZZ").is_err());
        assert!(COUNTRIES.len() >= 30);
        assert!(COUNTRIES.iter().all(|c| validate_country(c.code).is_ok()));
    }

    #[test]
    fn every_country_has_a_valid_default_timezone() {
        for c in COUNTRIES {
            assert!(
                c.default_timezone.parse::<chrono_tz::Tz>().is_ok(),
                "{} → {}",
                c.code,
                c.default_timezone
            );
        }
        assert_eq!(default_timezone_for_country("in"), Some("Asia/Kolkata"));
        assert_eq!(default_timezone_for_country("ZZ"), None);
    }
}
