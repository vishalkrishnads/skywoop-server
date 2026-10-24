//! Clone a template project for the Haserupt onboarding demo.

use std::collections::HashMap;

use chrono::{Timelike, Utc};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::domain::assets::{self, ASSET_COLUMNS, Asset};
use crate::domain::types::{BrandProfile, new_id, normalize_brand};
use crate::domain::Platform;
use crate::error::{AppError, AppResult};
use crate::jobs::DataJob;
use crate::storage::BlobStore;

#[derive(Debug, sqlx::FromRow)]
struct TemplateProject {
    id: Uuid,
    name: String,
    description: String,
    website: Option<String>,
    timezone: String,
    market_country: String,
    industry: Option<String>,
    app_store_app_id: Option<String>,
    brand: sqlx::types::Json<BrandProfile>,
    context_notes: String,
}

#[derive(Debug, sqlx::FromRow)]
struct TemplateAudience {
    id: Uuid,
    position: i32,
    name: String,
    description: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TemplateConnection {
    pub external_account_id: String,
    pub display_name: String,
    pub token_secret_name: String,
    pub status: String,
}

#[derive(Debug, sqlx::FromRow)]
struct TemplatePlan {
    id: Uuid,
    platform: String,
    for_date: chrono::NaiveDate,
    status: String,
    version: i32,
    markdown: String,
    greeting: String,
    findings: sqlx::types::Json<serde_json::Value>,
    title: String,
    summary: String,
    no_op_reason: Option<String>,
    no_op_explanation: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct TemplatePlanVideo {
    id: Uuid,
    plan_id: Uuid,
    anchor: String,
    title: String,
    track: String,
    audience_id: Option<Uuid>,
    trend: Option<sqlx::types::Json<serde_json::Value>>,
    caption: String,
    daypart: Option<String>,
    position: i32,
}

#[derive(Debug, sqlx::FromRow)]
struct TemplatePlanVersion {
    plan_id: Uuid,
    version: i32,
    markdown: String,
    greeting: String,
    reason: String,
}

#[derive(Debug, sqlx::FromRow)]
struct TemplatePlanMessage {
    plan_id: Uuid,
    role: String,
    parts: sqlx::types::Json<serde_json::Value>,
}

#[derive(Debug, sqlx::FromRow)]
struct TemplateDir {
    id: Uuid,
    parent_id: Option<Uuid>,
    name: String,
    path: String,
    description: String,
}

/// Return an existing demo clone for this user, if any.
pub async fn existing_demo(
    pool: &PgPool,
    user_id: &str,
    template_id: Uuid,
) -> AppResult<Option<Uuid>> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "select id from projects where owner_user_id = $1 and demo_of = $2 limit 1",
    )
    .bind(user_id)
    .bind(template_id)
    .fetch_optional(pool)
    .await?;
    Ok(id)
}

/// Load the Instagram connection that must exist on the template.
pub async fn template_instagram(
    pool: &PgPool,
    template_id: Uuid,
) -> AppResult<TemplateConnection> {
    let row = sqlx::query_as::<_, TemplateConnection>(
        r#"
        select external_account_id, display_name, token_secret_name, status
          from platform_connections
         where project_id = $1 and platform = 'instagram'
        "#,
    )
    .bind(template_id)
    .fetch_optional(pool)
    .await?;
    match row {
        Some(c) if c.status == "connected" => Ok(c),
        Some(_) => Err(AppError::Conflict(
            "demo unavailable: template Instagram needs reconnecting".into(),
        )),
        None => Err(AppError::Conflict(
            "demo unavailable: template Instagram is not connected".into(),
        )),
    }
}

/// Clone the template into a new complete project owned by `user_id`.
///
/// Copies brand, audiences, App Store assets (blob bytes duplicated), upcoming
/// review/no_op plans, and the Instagram connection (shared token secret name).
/// Returns the new project id. Caller must hold the per-user advisory lock.
pub async fn clone_template(
    tx: &mut PgConnection,
    blob: &dyn BlobStore,
    template_id: Uuid,
    user_id: &str,
    ig: &TemplateConnection,
) -> AppResult<Uuid> {
    let template = sqlx::query_as::<_, TemplateProject>(
        r#"
        select id, name, description, website, timezone, market_country, industry,
               app_store_app_id, brand, context_notes
          from projects
         where id = $1
        "#,
    )
    .bind(template_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::Conflict("demo unavailable: template project missing".into()))?;

    let project_id = new_id();
    let simple = project_id.simple().to_string();
    let slug = format!("haserupt-{}", &simple[simple.len().saturating_sub(8)..]);

    sqlx::query(
        r#"
        insert into projects
            (id, slug, name, description, website, timezone, market_country, industry,
             app_store_app_id, brand, owner_user_id, onboarding_step, context_notes, demo_of)
        values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'complete', $12, $13)
        "#,
    )
    .bind(project_id)
    .bind(&slug)
    .bind(&template.name)
    .bind(&template.description)
    .bind(&template.website)
    .bind(&template.timezone)
    .bind(&template.market_country)
    .bind(&template.industry)
    .bind(&template.app_store_app_id)
    .bind(sqlx::types::Json(template.brand.0.clone()))
    .bind(user_id)
    .bind(&template.context_notes)
    .bind(template_id)
    .execute(&mut *tx)
    .await?;

    for platform in ["instagram", "facebook"] {
        sqlx::query("insert into platform_workspaces (project_id, platform) values ($1, $2)")
            .bind(project_id)
            .bind(platform)
            .execute(&mut *tx)
            .await?;
    }

    assets::seed_dirs(&mut *tx, project_id).await?;

    let audiences = sqlx::query_as::<_, TemplateAudience>(
        r#"
        select id, position, name, description
          from project_audiences
         where project_id = $1 and archived_at is null
         order by position
        "#,
    )
    .bind(template_id)
    .fetch_all(&mut *tx)
    .await?;

    let mut audience_map: HashMap<Uuid, Uuid> = HashMap::new();
    for a in &audiences {
        let new_aid = new_id();
        audience_map.insert(a.id, new_aid);
        sqlx::query(
            r#"
            insert into project_audiences (id, project_id, position, name, description)
            values ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(new_aid)
        .bind(project_id)
        .bind(a.position)
        .bind(&a.name)
        .bind(&a.description)
        .execute(&mut *tx)
        .await?;
    }

    let asset_map = copy_app_store_assets(tx, blob, template_id, project_id).await?;

    let mut brand = template.brand.0;
    if let Some(logo) = brand.logo_asset_id {
        brand.logo_asset_id = asset_map.get(&logo).copied();
    }
    brand.screenshot_asset_ids = brand
        .screenshot_asset_ids
        .into_iter()
        .filter_map(|id| asset_map.get(&id).copied())
        .collect();
    let brand = normalize_brand(brand).map_err(AppError::Invalid)?;
    sqlx::query("update projects set brand = $2 where id = $1")
        .bind(project_id)
        .bind(sqlx::types::Json(brand))
        .execute(&mut *tx)
        .await?;

    let today = local_today(&template.timezone)?;
    let plans = sqlx::query_as::<_, TemplatePlan>(
        r#"
        select id, platform, for_date, status, version, markdown, greeting, findings,
               title, summary, no_op_reason, no_op_explanation
          from daily_plans
         where project_id = $1
           and for_date >= $2
           and status in ('review', 'no_op')
         order by for_date, platform
        "#,
    )
    .bind(template_id)
    .bind(today)
    .fetch_all(&mut *tx)
    .await?;

    let plan_ids: Vec<Uuid> = plans.iter().map(|p| p.id).collect();
    let mut plan_map: HashMap<Uuid, Uuid> = HashMap::new();

    for p in &plans {
        let new_pid = new_id();
        plan_map.insert(p.id, new_pid);
        sqlx::query(
            r#"
            insert into daily_plans
                (id, project_id, platform, for_date, status, version, markdown, greeting,
                 findings, title, summary, no_op_reason, no_op_explanation)
            values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)
            "#,
        )
        .bind(new_pid)
        .bind(project_id)
        .bind(&p.platform)
        .bind(p.for_date)
        .bind(&p.status)
        .bind(p.version)
        .bind(&p.markdown)
        .bind(&p.greeting)
        .bind(&p.findings)
        .bind(&p.title)
        .bind(&p.summary)
        .bind(&p.no_op_reason)
        .bind(&p.no_op_explanation)
        .execute(&mut *tx)
        .await?;
    }

    if !plan_ids.is_empty() {
        let videos = sqlx::query_as::<_, TemplatePlanVideo>(
            r#"
            select id, plan_id, anchor, title, track, audience_id, trend, caption, daypart, position
              from plan_videos
             where plan_id = any($1)
            "#,
        )
        .bind(&plan_ids)
        .fetch_all(&mut *tx)
        .await?;

        for v in &videos {
            let Some(&new_plan_id) = plan_map.get(&v.plan_id) else {
                continue;
            };
            let new_audience = v.audience_id.and_then(|id| audience_map.get(&id).copied());
            sqlx::query(
                r#"
                insert into plan_videos
                    (id, plan_id, anchor, title, track, audience_id, trend, caption, daypart, position)
                values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
                "#,
            )
            .bind(new_id())
            .bind(new_plan_id)
            .bind(&v.anchor)
            .bind(&v.title)
            .bind(&v.track)
            .bind(new_audience)
            .bind(&v.trend)
            .bind(&v.caption)
            .bind(&v.daypart)
            .bind(v.position)
            .execute(&mut *tx)
            .await?;
        }

        let versions = sqlx::query_as::<_, TemplatePlanVersion>(
            r#"
            select plan_id, version, markdown, greeting, reason
              from plan_versions
             where plan_id = any($1)
            "#,
        )
        .bind(&plan_ids)
        .fetch_all(&mut *tx)
        .await?;
        for v in &versions {
            let Some(&new_plan_id) = plan_map.get(&v.plan_id) else {
                continue;
            };
            sqlx::query(
                r#"
                insert into plan_versions (plan_id, version, markdown, greeting, reason)
                values ($1, $2, $3, $4, $5)
                "#,
            )
            .bind(new_plan_id)
            .bind(v.version)
            .bind(&v.markdown)
            .bind(&v.greeting)
            .bind(&v.reason)
            .execute(&mut *tx)
            .await?;
        }

        let messages = sqlx::query_as::<_, TemplatePlanMessage>(
            r#"
            select plan_id, role, parts
              from plan_messages
             where plan_id = any($1)
             order by created_at
            "#,
        )
        .bind(&plan_ids)
        .fetch_all(&mut *tx)
        .await?;
        for m in &messages {
            let Some(&new_plan_id) = plan_map.get(&m.plan_id) else {
                continue;
            };
            sqlx::query(
                r#"
                insert into plan_messages (id, plan_id, role, parts)
                values ($1, $2, $3, $4)
                "#,
            )
            .bind(new_id())
            .bind(new_plan_id)
            .bind(&m.role)
            .bind(&m.parts)
            .execute(&mut *tx)
            .await?;
        }
    }

    sqlx::query(
        r#"
        insert into platform_connections
            (id, project_id, platform, external_account_id, display_name, token_secret_name, status)
        values ($1, $2, 'instagram', $3, $4, $5, 'connected')
        "#,
    )
    .bind(new_id())
    .bind(project_id)
    .bind(&ig.external_account_id)
    .bind(&ig.display_name)
    .bind(&ig.token_secret_name)
    .execute(&mut *tx)
    .await?;

    if plans.is_empty() {
        let first_date = first_plan_date(&template.timezone)?;
        enqueue_draft_plan(tx, project_id, Platform::Instagram, first_date).await?;
    }

    Ok(project_id)
}

async fn enqueue_draft_plan(
    tx: &mut PgConnection,
    project_id: Uuid,
    platform: Platform,
    for_date: chrono::NaiveDate,
) -> AppResult<()> {
    let job = DataJob::DraftPlan {
        project_id,
        platform,
        for_date,
        allow_no_op: false,
    };
    let v = serde_json::to_value(&job).map_err(|e| AppError::Other(e.into()))?;
    let kind = v["kind"].as_str().unwrap_or("DraftPlan").to_owned();
    let payload = v.get("payload").cloned().unwrap_or(serde_json::Value::Null);
    let dedupe = format!("draft_plan:{project_id}:{}:{for_date}", platform.as_str());
    sqlx::query(
        r#"
        insert into jobs (id, kind, payload, dedupe_key, run_at)
        values ($1, $2, $3, $4, now())
        on conflict (dedupe_key) do nothing
        "#,
    )
    .bind(new_id())
    .bind(kind)
    .bind(payload)
    .bind(dedupe)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

fn local_today(timezone: &str) -> AppResult<chrono::NaiveDate> {
    let tz: chrono_tz::Tz = timezone
        .parse()
        .map_err(|_| AppError::Invalid(format!("unknown timezone {timezone}")))?;
    Ok(Utc::now().with_timezone(&tz).date_naive())
}

fn first_plan_date(timezone: &str) -> AppResult<chrono::NaiveDate> {
    let tz: chrono_tz::Tz = timezone
        .parse()
        .map_err(|_| AppError::Invalid(format!("unknown timezone {timezone}")))?;
    let now_local = Utc::now().with_timezone(&tz);
    let today = now_local.date_naive();
    Ok(if now_local.hour() < 16 {
        today
    } else {
        today + chrono::Duration::days(1)
    })
}

/// Copy `App Store` dirs (and nested) plus their assets; return old→new asset id map.
async fn copy_app_store_assets(
    tx: &mut PgConnection,
    blob: &dyn BlobStore,
    template_id: Uuid,
    project_id: Uuid,
) -> AppResult<HashMap<Uuid, Uuid>> {
    let dirs = sqlx::query_as::<_, TemplateDir>(
        r#"
        select id, parent_id, name, path, description
          from asset_dirs
         where project_id = $1
           and (path = 'App Store' or path like 'App Store/%')
         order by length(path)
        "#,
    )
    .bind(template_id)
    .fetch_all(&mut *tx)
    .await?;

    // Seed already created top-level App Store; map template App Store id → clone App Store id.
    let clone_app_store: Uuid = sqlx::query_scalar(
        "select id from asset_dirs where project_id = $1 and path = 'App Store'",
    )
    .bind(project_id)
    .fetch_one(&mut *tx)
    .await?;

    let mut dir_map: HashMap<Uuid, Uuid> = HashMap::new();
    for d in &dirs {
        if d.path == "App Store" {
            dir_map.insert(d.id, clone_app_store);
            continue;
        }
        let new_dir_id = new_id();
        let parent_id = d
            .parent_id
            .and_then(|pid| dir_map.get(&pid).copied())
            .unwrap_or(clone_app_store);
        sqlx::query(
            r#"
            insert into asset_dirs (id, project_id, parent_id, name, path, description)
            values ($1, $2, $3, $4, $5, $6)
            on conflict (project_id, path) do nothing
            "#,
        )
        .bind(new_dir_id)
        .bind(project_id)
        .bind(parent_id)
        .bind(&d.name)
        .bind(&d.path)
        .bind(&d.description)
        .execute(&mut *tx)
        .await?;
        // If conflict, look up existing
        let resolved: Uuid = sqlx::query_scalar(
            "select id from asset_dirs where project_id = $1 and path = $2",
        )
        .bind(project_id)
        .bind(&d.path)
        .fetch_one(&mut *tx)
        .await?;
        dir_map.insert(d.id, resolved);
    }

    let assets = sqlx::query_as::<_, Asset>(sqlx::AssertSqlSafe(format!(
        "select {ASSET_COLUMNS} from assets \
          where project_id = $1 and (path = 'App Store' or path like 'App Store/%')"
    )))
    .bind(template_id)
    .fetch_all(&mut *tx)
    .await?;

    let mut asset_map: HashMap<Uuid, Uuid> = HashMap::new();
    for a in &assets {
        let new_id = new_id();
        let new_blob = assets::upload_blob_path(project_id, new_id);
        match blob.get(&a.blob_path).await {
            Ok(bytes) => {
                if let Err(e) = blob.put(&new_blob, bytes, &a.mime).await {
                    tracing::warn!(
                        error = %e,
                        from = %a.blob_path,
                        to = %new_blob,
                        "demo asset blob copy failed; skipping"
                    );
                    continue;
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    blob = %a.blob_path,
                    "demo asset blob missing; skipping"
                );
                continue;
            }
        }
        let new_dir = a.dir_id.and_then(|d| dir_map.get(&d).copied());
        sqlx::query(
            r#"
            insert into assets
                (id, project_id, dir_id, name, path, kind, blob_path, mime,
                 width, height, duration_ms, sha256, description, tags, transcript, bytes)
            values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)
            "#,
        )
        .bind(new_id)
        .bind(project_id)
        .bind(new_dir)
        .bind(&a.name)
        .bind(&a.path)
        .bind(&a.kind)
        .bind(&new_blob)
        .bind(&a.mime)
        .bind(a.width)
        .bind(a.height)
        .bind(a.duration_ms)
        .bind(&a.sha256)
        .bind(&a.description)
        .bind(&a.tags)
        .bind(&a.transcript)
        .bind(a.bytes)
        .execute(&mut *tx)
        .await?;
        asset_map.insert(a.id, new_id);
    }
    Ok(asset_map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::LocalBlobStore;
    use std::sync::Arc;
    use tempfile::TempDir;

    async fn insert_user(pool: &PgPool, id: &str) {
        sqlx::query("insert into app_users (id, email) values ($1, $2)")
            .bind(id)
            .bind(format!("{id}@test.local"))
            .execute(pool)
            .await
            .unwrap();
    }

    async fn insert_template(pool: &PgPool, owner: &str) -> (Uuid, String) {
        let tid = new_id();
        sqlx::query(
            "insert into projects (id, slug, name, description, timezone, market_country, \
             app_store_app_id, owner_user_id, onboarding_step, brand) \
             values ($1,$2,'Haserupt','demo','America/Los_Angeles','US','6763058568',$3,'complete','{}')",
        )
        .bind(tid)
        .bind(format!("tmpl-{tid}"))
        .bind(owner)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("insert into platform_workspaces (project_id, platform) values ($1,'instagram')")
            .bind(tid)
            .execute(pool)
            .await
            .unwrap();
        let secret = format!("meta-page-token-{tid}-instagram");
        sqlx::query(
            "insert into platform_connections \
             (id, project_id, platform, external_account_id, display_name, token_secret_name, status) \
             values ($1,$2,'instagram','ig-1','@haserupt',$3,'connected')",
        )
        .bind(new_id())
        .bind(tid)
        .bind(&secret)
        .execute(pool)
        .await
        .unwrap();
        let aud = new_id();
        sqlx::query(
            "insert into project_audiences (id, project_id, position, name, description) \
             values ($1,$2,1,'Builders','iOS builders')",
        )
        .bind(aud)
        .bind(tid)
        .execute(pool)
        .await
        .unwrap();
        let plan = new_id();
        let today = chrono::Utc::now().date_naive();
        sqlx::query(
            "insert into daily_plans (id, project_id, platform, for_date, status, markdown, title, summary) \
             values ($1,$2,'instagram',$3,'review','# Plan','Day 1','summary')",
        )
        .bind(plan)
        .bind(tid)
        .bind(today)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "insert into plan_videos (id, plan_id, anchor, title, track, audience_id, position) \
             values ($1,$2,'v1','Clip','audience',$3,0)",
        )
        .bind(new_id())
        .bind(plan)
        .bind(aud)
        .execute(pool)
        .await
        .unwrap();
        (tid, secret)
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn clone_is_idempotent_and_remaps_audiences(pool: PgPool) {
        insert_user(&pool, "tmpl-owner").await;
        insert_user(&pool, "demo-user").await;
        let (tid, secret) = insert_template(&pool, "tmpl-owner").await;
        let ig = template_instagram(&pool, tid).await.unwrap();
        assert_eq!(ig.token_secret_name, secret);

        let tmp = TempDir::new().unwrap();
        let blob: Arc<dyn BlobStore> = LocalBlobStore::new(
            tmp.path().to_path_buf(),
            url::Url::parse("http://localhost/").unwrap(),
            b"test-signing-key",
        );

        let mut tx = pool.begin().await.unwrap();
        let a = clone_template(&mut tx, blob.as_ref(), tid, "demo-user", &ig)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let existing = existing_demo(&pool, "demo-user", tid).await.unwrap();
        assert_eq!(existing, Some(a));

        let remapped: Option<Uuid> = sqlx::query_scalar(
            "select pv.audience_id from plan_videos pv \
              join daily_plans dp on dp.id = pv.plan_id \
             where dp.project_id = $1 limit 1",
        )
        .bind(a)
        .fetch_one(&pool)
        .await
        .unwrap();
        let template_aud: Uuid = sqlx::query_scalar(
            "select id from project_audiences where project_id = $1 and position = 1",
        )
        .bind(tid)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_ne!(remapped, Some(template_aud));

        // Shared secret must survive clone disconnect when template still references it.
        sqlx::query(
            "delete from platform_connections where project_id = $1 and platform = 'instagram'",
        )
        .bind(a)
        .execute(&pool)
        .await
        .unwrap();
        let still: i64 = sqlx::query_scalar(
            "select count(*) from platform_connections where token_secret_name = $1",
        )
        .bind(&secret)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(still, 1);
    }
}
