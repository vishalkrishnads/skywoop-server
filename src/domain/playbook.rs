//! Database creative-pattern candidates: proposed by diary/analysis agents or users,
//! reviewed by the operator. PlanDay reads the handwritten `prompts/playbook.md` via
//! `read_playbook`, not these rows.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::ai::planner::PlaybookCandidateOut;
use crate::domain::new_id;
use crate::error::{AppError, AppResult};
use crate::scheduling::industries::Industry;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct PlaybookEntry {
    pub id: Uuid,
    pub title: String,
    pub body_md: String,
    pub app_categories: Vec<String>,
    pub tags: Vec<String>,
    pub evidence: String,
    pub source_project_id: Option<Uuid>,
    /// proposed | approved | rejected
    pub status: String,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const COLUMNS: &str = "id, title, body_md, app_categories, tags, evidence, source_project_id, \
     status, reviewed_at, created_at, updated_at";

pub const STATUSES: [&str; 3] = ["proposed", "approved", "rejected"];

fn normalize_list(items: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for i in items {
        let i = i.trim().to_lowercase();
        if !i.is_empty() && i.chars().count() <= 60 && !out.contains(&i) {
            out.push(i);
        }
        if out.len() >= 20 {
            break;
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct NewEntry {
    pub title: String,
    pub body_md: String,
    pub app_categories: Vec<String>,
    pub tags: Vec<String>,
    pub evidence: String,
    pub source_project_id: Option<Uuid>,
}

/// Insert a `proposed` entry.
pub async fn submit(pool: &PgPool, e: NewEntry) -> AppResult<PlaybookEntry> {
    let title = e.title.trim();
    if title.is_empty() || e.body_md.trim().is_empty() {
        return Err(AppError::Invalid("title and body_md are required".into()));
    }
    if title.chars().count() > 160 {
        return Err(AppError::Invalid("title exceeds 160 characters".into()));
    }
    Ok(
        sqlx::query_as::<_, PlaybookEntry>(sqlx::AssertSqlSafe(format!(
            "insert into playbook_entries \
            (id, title, body_md, app_categories, tags, evidence, source_project_id) \
         values ($1,$2,$3,$4,$5,$6,$7) returning {COLUMNS}"
        )))
        .bind(new_id())
        .bind(title)
        .bind(e.body_md.trim())
        .bind(normalize_list(&e.app_categories))
        .bind(normalize_list(&e.tags))
        .bind(e.evidence.trim())
        .bind(e.source_project_id)
        .fetch_one(pool)
        .await?,
    )
}

/// Agent-proposed candidates; skips titles that already exist. Returns how many were inserted.
pub async fn propose_candidates(
    pool: &PgPool,
    project_id: Uuid,
    candidates: &[PlaybookCandidateOut],
) -> anyhow::Result<usize> {
    let mut inserted = 0;
    for c in candidates.iter().take(3) {
        if c.title.trim().is_empty() || c.body_md.trim().is_empty() {
            continue;
        }
        let res = sqlx::query(
            "insert into playbook_entries \
                (id, title, body_md, app_categories, tags, evidence, source_project_id) \
             select $1,$2,$3,$4,$5,$6,$7 \
              where not exists (select 1 from playbook_entries where lower(title) = lower($2))",
        )
        .bind(new_id())
        .bind(c.title.trim())
        .bind(c.body_md.trim())
        .bind(normalize_list(&c.app_categories))
        .bind(normalize_list(&c.tags))
        .bind(c.evidence.trim())
        .bind(project_id)
        .execute(pool)
        .await?;
        inserted += res.rows_affected() as usize;
    }
    Ok(inserted)
}

pub async fn get(pool: &PgPool, id: Uuid) -> AppResult<PlaybookEntry> {
    sqlx::query_as::<_, PlaybookEntry>(sqlx::AssertSqlSafe(format!(
        "select {COLUMNS} from playbook_entries where id = $1"
    )))
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(AppError::from)
}

pub async fn list(
    pool: &PgPool,
    status: Option<&str>,
    category: Option<&str>,
    q: Option<&str>,
    limit: i64,
) -> AppResult<Vec<PlaybookEntry>> {
    let q = q.map(str::trim).filter(|s| !s.is_empty()).map(|s| {
        format!(
            "%{}%",
            s.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        )
    });
    Ok(
        sqlx::query_as::<_, PlaybookEntry>(sqlx::AssertSqlSafe(format!(
            "select {COLUMNS} from playbook_entries \
          where ($1::text is null or status = $1) \
            and ($2::text is null or $2 = any(app_categories)) \
            and ($3::text is null or title ilike $3 escape '\\' or body_md ilike $3 escape '\\' \
                 or exists (select 1 from unnest(tags) t where t ilike $3 escape '\\')) \
          order by updated_at desc limit $4"
        )))
        .bind(status)
        .bind(category.map(|c| c.trim().to_lowercase()))
        .bind(q)
        .bind(limit.clamp(1, 200))
        .fetch_all(pool)
        .await?,
    )
}

#[derive(Debug, Default)]
pub struct ReviewEdit {
    pub title: Option<String>,
    pub body_md: Option<String>,
    pub app_categories: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
}

/// Approve / reject (optionally editing the entry first).
pub async fn review(
    pool: &PgPool,
    id: Uuid,
    status: &str,
    edit: ReviewEdit,
) -> AppResult<PlaybookEntry> {
    if !matches!(status, "approved" | "rejected" | "proposed") {
        return Err(AppError::Invalid(
            "status must be approved, rejected or proposed".into(),
        ));
    }
    if edit.title.as_deref().is_some_and(|t| t.trim().is_empty())
        || edit.body_md.as_deref().is_some_and(|t| t.trim().is_empty())
    {
        return Err(AppError::Invalid(
            "title and body_md cannot be empty".into(),
        ));
    }
    sqlx::query_as::<_, PlaybookEntry>(sqlx::AssertSqlSafe(format!(
        "update playbook_entries set status = $2, \
                reviewed_at = case when $2 = 'proposed' then null else now() end, \
                title = coalesce($3, title), body_md = coalesce($4, body_md), \
                app_categories = coalesce($5, app_categories), tags = coalesce($6, tags), \
                updated_at = now() \
          where id = $1 returning {COLUMNS}"
    )))
    .bind(id)
    .bind(status)
    .bind(edit.title.as_deref().map(str::trim))
    .bind(edit.body_md.as_deref().map(str::trim))
    .bind(edit.app_categories.as_deref().map(normalize_list))
    .bind(edit.tags.as_deref().map(normalize_list))
    .fetch_one(pool)
    .await
    .map_err(AppError::from)
}

/// Category signals for a project: (free-text haystack, explicit category tokens).
pub async fn project_category_signals(
    pool: &PgPool,
    project_id: Uuid,
) -> anyhow::Result<(String, Vec<String>)> {
    let row: (String, String, Option<String>) =
        sqlx::query_as("select name, description, industry from projects where id = $1")
            .bind(project_id)
            .fetch_one(pool)
            .await?;
    let mut tokens: Vec<String> = Vec::new();
    let mut haystack = format!("{} {}", row.0, row.1).to_lowercase();
    if let Some(ind) = row.2.as_deref() {
        tokens.push(ind.to_lowercase());
        if let Some(i) = Industry::parse(ind) {
            let display = i.display_name().to_lowercase();
            haystack.push(' ');
            haystack.push_str(&display);
            tokens.extend(
                display
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| w.len() > 2)
                    .map(str::to_owned),
            );
        }
    }
    Ok((haystack, tokens))
}

/// Approved entries whose categories match the project (or that apply to every category).
pub async fn matching_for_project(
    pool: &PgPool,
    project_id: Uuid,
    limit: i64,
) -> anyhow::Result<Vec<PlaybookEntry>> {
    let (haystack, tokens) = project_category_signals(pool, project_id).await?;
    Ok(
        sqlx::query_as::<_, PlaybookEntry>(sqlx::AssertSqlSafe(format!(
            "select {COLUMNS} from playbook_entries pe \
          where status = 'approved' \
            and (cardinality(app_categories) = 0 \
                 or exists (select 1 from unnest(app_categories) c \
                             where c = any($2) or position(c in $1) > 0)) \
          order by updated_at desc limit $3"
        )))
        .bind(haystack)
        .bind(tokens)
        .bind(limit.clamp(1, 50))
        .fetch_all(pool)
        .await?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_normalization() {
        let v = normalize_list(&[" Health ".into(), "health".into(), "".into(), "UI".into()]);
        assert_eq!(v, vec!["health", "ui"]);
    }
}
