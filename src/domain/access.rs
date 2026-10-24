//! Authorization helpers: every resource is scoped to its project's owner.

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{AppError, AppResult};

/// Returns Ok when `user_id` owns the project. Unknown or foreign projects → NotFound
/// so we never leak existence across tenants.
pub async fn ensure_project_owner(db: &PgPool, user_id: &str, project_id: Uuid) -> AppResult<()> {
    let owner: Option<(String,)> =
        sqlx::query_as("select owner_user_id from projects where id = $1")
            .bind(project_id)
            .fetch_optional(db)
            .await?;
    match owner {
        Some((oid,)) if oid == user_id => Ok(()),
        _ => Err(AppError::NotFound),
    }
}

/// Project must be owned by `user_id` and past onboarding (`complete`).
pub async fn ensure_project_ready(db: &PgPool, user_id: &str, project_id: Uuid) -> AppResult<()> {
    let row: Option<(String, String)> =
        sqlx::query_as("select owner_user_id, onboarding_step from projects where id = $1")
            .bind(project_id)
            .fetch_optional(db)
            .await?;
    match row {
        Some((oid, step)) if oid == user_id && step == "complete" => Ok(()),
        Some((oid, _)) if oid == user_id => Err(AppError::Conflict(
            "project onboarding is not complete".into(),
        )),
        _ => Err(AppError::NotFound),
    }
}

async fn ensure_via_project(
    db: &PgPool,
    user_id: &str,
    sql: &'static str,
    id: Uuid,
) -> AppResult<Uuid> {
    let row: Option<(Uuid, String)> = sqlx::query_as(sql).bind(id).fetch_optional(db).await?;
    match row {
        Some((pid, oid)) if oid == user_id => Ok(pid),
        _ => Err(AppError::NotFound),
    }
}

pub async fn ensure_asset_owner(db: &PgPool, user_id: &str, asset_id: Uuid) -> AppResult<Uuid> {
    ensure_via_project(
        db,
        user_id,
        "select a.project_id, p.owner_user_id from assets a \
         join projects p on p.id = a.project_id where a.id = $1",
        asset_id,
    )
    .await
}

pub async fn ensure_plan_owner(db: &PgPool, user_id: &str, plan_id: Uuid) -> AppResult<Uuid> {
    ensure_via_project(
        db,
        user_id,
        "select d.project_id, p.owner_user_id from daily_plans d \
         join projects p on p.id = d.project_id where d.id = $1",
        plan_id,
    )
    .await
}

pub async fn ensure_video_project_owner(
    db: &PgPool,
    user_id: &str,
    video_project_id: Uuid,
) -> AppResult<Uuid> {
    ensure_via_project(
        db,
        user_id,
        "select v.project_id, p.owner_user_id from video_projects v \
         join projects p on p.id = v.project_id where v.id = $1",
        video_project_id,
    )
    .await
}

pub async fn ensure_job_owner(db: &PgPool, user_id: &str, job_id: Uuid) -> AppResult<()> {
    // Stored payload is the inner job object (no `kind` wrapper), e.g.
    // `{ "project_id": "..." }` or `{ "discovery_id": "..." }`.
    let row: Option<(serde_json::Value,)> =
        sqlx::query_as("select payload from jobs where id = $1")
            .bind(job_id)
            .fetch_optional(db)
            .await?;
    let Some((payload,)) = row else {
        return Err(AppError::NotFound);
    };
    if let Some(pid) = payload
        .get("project_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        return ensure_project_owner(db, user_id, pid).await;
    }
    if let Some(did) = payload
        .get("discovery_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        let owned: Option<(Uuid,)> = sqlx::query_as(
            "select d.project_id from project_discoveries d \
             join projects p on p.id = d.project_id \
             where d.id = $1 and p.owner_user_id = $2",
        )
        .bind(did)
        .bind(user_id)
        .fetch_optional(db)
        .await?;
        return if owned.is_some() {
            Ok(())
        } else {
            Err(AppError::NotFound)
        };
    }
    Err(AppError::NotFound)
}

pub async fn ensure_experiment_owner(
    db: &PgPool,
    user_id: &str,
    experiment_id: Uuid,
) -> AppResult<Uuid> {
    ensure_via_project(
        db,
        user_id,
        "select e.project_id, p.owner_user_id from experiments e \
         join projects p on p.id = e.project_id where e.id = $1",
        experiment_id,
    )
    .await
}

pub async fn ensure_diary_owner(db: &PgPool, user_id: &str, entry_id: Uuid) -> AppResult<Uuid> {
    ensure_via_project(
        db,
        user_id,
        "select d.project_id, p.owner_user_id from diary_entries d \
         join projects p on p.id = d.project_id where d.id = $1",
        entry_id,
    )
    .await
}

/// Playbook rows are optionally tied to a source project via `source_project_id`.
/// - If set: caller must own that project (returns its id).
/// - If null: global catalog entry — any authenticated caller may access (returns `Uuid::nil()`).
pub async fn ensure_playbook_owner(db: &PgPool, user_id: &str, entry_id: Uuid) -> AppResult<Uuid> {
    let row: Option<(Option<Uuid>,)> =
        sqlx::query_as("select source_project_id from playbook_entries where id = $1")
            .bind(entry_id)
            .fetch_optional(db)
            .await?;
    let Some((source_project_id,)) = row else {
        return Err(AppError::NotFound);
    };
    match source_project_id {
        Some(pid) => {
            ensure_project_owner(db, user_id, pid).await?;
            Ok(pid)
        }
        None => Ok(Uuid::nil()),
    }
}

pub async fn ensure_comment_owner(db: &PgPool, user_id: &str, comment_id: Uuid) -> AppResult<Uuid> {
    ensure_via_project(
        db,
        user_id,
        "select d.project_id, p.owner_user_id from plan_comments c \
         join daily_plans d on d.id = c.plan_id \
         join projects p on p.id = d.project_id where c.id = $1",
        comment_id,
    )
    .await
}

pub async fn ensure_connection_owner(
    db: &PgPool,
    user_id: &str,
    connection_id: Uuid,
) -> AppResult<Uuid> {
    ensure_via_project(
        db,
        user_id,
        "select c.project_id, p.owner_user_id from platform_connections c \
         join projects p on p.id = c.project_id where c.id = $1",
        connection_id,
    )
    .await
}

pub async fn ensure_asset_dir_owner(db: &PgPool, user_id: &str, dir_id: Uuid) -> AppResult<Uuid> {
    ensure_via_project(
        db,
        user_id,
        "select d.project_id, p.owner_user_id from asset_dirs d \
         join projects p on p.id = d.project_id where d.id = $1",
        dir_id,
    )
    .await
}
