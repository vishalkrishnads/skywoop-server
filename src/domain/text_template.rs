//! Shared text-template catalog. Seeds ship in the migration; the editor adds the rest.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use sqlx::types::Json;
use utoipa::ToSchema;

use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TemplateBox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TemplateElement {
    pub name: String,
    pub font_family: String,
    pub font_size: f64,
    pub font_weight: i32,
    #[serde(rename = "box")]
    pub rect: TemplateBox,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct TextTemplateRow {
    id: String,
    name: String,
    description: String,
    elements: Json<Vec<TemplateElement>>,
    seeded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TextTemplate {
    pub id: String,
    pub name: String,
    pub description: String,
    pub elements: Vec<TemplateElement>,
    pub seeded: bool,
}

fn into_template(row: TextTemplateRow) -> TextTemplate {
    TextTemplate {
        id: row.id,
        name: row.name,
        description: row.description,
        elements: row.elements.0,
        seeded: row.seeded,
    }
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewTextTemplate {
    pub id: String,
    pub name: String,
    pub description: String,
    pub elements: Vec<TemplateElement>,
}

const SEED_IDS: [&str; 6] = [
    "hook",
    "hook-subline",
    "lower-third",
    "three-stack",
    "quote",
    "call-to-action",
];

fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
}

fn validate(t: &NewTextTemplate) -> AppResult<()> {
    let id = t.id.trim();
    let name = t.name.trim();
    if !valid_id(id) {
        return Err(AppError::Invalid(
            "id must start with a letter or digit and use only letters, digits, _, ., -".into(),
        ));
    }
    if name.is_empty() || name.chars().count() > 80 {
        return Err(AppError::Invalid("name must be 1-80 characters".into()));
    }
    if t.description.chars().count() > 280 {
        return Err(AppError::Invalid(
            "description must be 280 characters or fewer".into(),
        ));
    }
    if t.elements.is_empty() || t.elements.len() > 12 {
        return Err(AppError::Invalid(
            "a template needs 1-12 text elements".into(),
        ));
    }
    let mut names = Vec::new();
    for el in &t.elements {
        let n = el.name.trim();
        if n.is_empty() || n.chars().count() > 64 {
            return Err(AppError::Invalid(
                "each text name must be 1-64 characters".into(),
            ));
        }
        if names.iter().any(|e: &String| e.eq_ignore_ascii_case(n)) {
            return Err(AppError::Invalid(format!("duplicate text name \"{n}\"")));
        }
        names.push(n.to_string());
        if el.font_family.trim().is_empty() || el.font_family.chars().count() > 80 {
            return Err(AppError::Invalid("fontFamily is required".into()));
        }
        if !el.font_size.is_finite() || el.font_size <= 0.0 || el.font_size > 400.0 {
            return Err(AppError::Invalid(
                "fontSize must be between 0 and 400".into(),
            ));
        }
        if !(100..=900).contains(&el.font_weight) {
            return Err(AppError::Invalid(
                "fontWeight must be between 100 and 900".into(),
            ));
        }
        if !el.rect.width.is_finite()
            || el.rect.width <= 0.0
            || !el.rect.height.is_finite()
            || el.rect.height <= 0.0
        {
            return Err(AppError::Invalid(
                "box width and height must be positive".into(),
            ));
        }
    }
    Ok(())
}

pub async fn list(pool: &PgPool) -> AppResult<Vec<TextTemplate>> {
    let rows = sqlx::query_as::<_, TextTemplateRow>(
        "select id, name, description, elements, seeded \
         from text_templates order by position, name",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(into_template).collect())
}

pub async fn search(pool: &PgPool, query: Option<&str>) -> AppResult<Vec<TextTemplate>> {
    let rows = list(pool).await?;
    let q = query.unwrap_or("").trim().to_lowercase();
    if q.is_empty() {
        return Ok(rows);
    }
    Ok(rows
        .into_iter()
        .filter(|t| {
            t.name.to_lowercase().contains(&q)
                || t.description.to_lowercase().contains(&q)
                || t.id.to_lowercase().contains(&q)
                || serde_json::to_string(&t.elements)
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&q)
        })
        .collect())
}

pub async fn upsert_custom(pool: &PgPool, raw: NewTextTemplate) -> AppResult<TextTemplate> {
    validate(&raw)?;
    let id = raw.id.trim().to_string();
    if SEED_IDS.contains(&id.as_str()) {
        return Err(AppError::Conflict(format!(
            "cannot overwrite seeded template \"{id}\""
        )));
    }
    if let Some(existing) = sqlx::query_as::<_, TextTemplateRow>(
        "select id, name, description, elements, seeded from text_templates where id = $1",
    )
    .bind(&id)
    .fetch_optional(pool)
    .await?
    {
        if existing.seeded {
            return Err(AppError::Conflict(format!(
                "cannot overwrite seeded template \"{id}\""
            )));
        }
        let row = sqlx::query_as::<_, TextTemplateRow>(
            "update text_templates set name = $2, description = $3, elements = $4 \
             where id = $1 returning id, name, description, elements, seeded",
        )
        .bind(&id)
        .bind(raw.name.trim())
        .bind(raw.description.trim())
        .bind(Json(raw.elements))
        .fetch_one(pool)
        .await?;
        return Ok(into_template(row));
    }
    let row = sqlx::query_as::<_, TextTemplateRow>(
        "insert into text_templates (id, name, description, elements, seeded, position) \
         values ($1, $2, $3, $4, false, \
           (select coalesce(max(position), 0) + 1 from text_templates)) \
         returning id, name, description, elements, seeded",
    )
    .bind(&id)
    .bind(raw.name.trim())
    .bind(raw.description.trim())
    .bind(Json(raw.elements))
    .fetch_one(pool)
    .await?;
    Ok(into_template(row))
}

pub async fn delete_custom(pool: &PgPool, id: &str) -> AppResult<()> {
    let existing = sqlx::query_as::<_, TextTemplateRow>(
        "select id, name, description, elements, seeded from text_templates where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    let Some(existing) = existing else {
        return Err(AppError::NotFound);
    };
    if existing.seeded || SEED_IDS.contains(&id) {
        return Err(AppError::Conflict(format!(
            "cannot delete seeded template \"{id}\""
        )));
    }
    sqlx::query("delete from text_templates where id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
