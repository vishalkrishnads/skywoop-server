//! Global royalty-free music and sound-effect library shared across projects.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::domain::assets::{self, Asset, AssetKind, NewAsset};
use crate::domain::new_id;
use crate::error::{AppError, AppResult};

pub const COLUMNS: &str = "id, kind, title, description, tags, duration_ms, blob_path, mime, \
     bytes, sha256, source, source_id, license, attribution, created_at";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AudioLibraryKind {
    SoundEffect,
    Music,
}

impl AudioLibraryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SoundEffect => "sound_effect",
            Self::Music => "music",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "sound_effect" | "sound effects" | "sfx" => Some(Self::SoundEffect),
            "music" => Some(Self::Music),
            _ => None,
        }
    }

    pub fn dir_path(self) -> &'static str {
        match self {
            Self::SoundEffect => "Library/sfx",
            Self::Music => "Library/music",
        }
    }

    pub fn dir_description(self) -> &'static str {
        match self {
            Self::SoundEffect => "Royalty-free sound effects from the shared library.",
            Self::Music => "Royalty-free music from the shared library.",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct AudioLibraryEntry {
    pub id: Uuid,
    pub kind: String,
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
    pub duration_ms: Option<i32>,
    pub blob_path: String,
    pub mime: String,
    pub bytes: Option<i64>,
    pub sha256: String,
    pub source: String,
    pub source_id: String,
    pub license: String,
    pub attribution: String,
    pub created_at: DateTime<Utc>,
}

pub fn library_blob_path(id: Uuid) -> String {
    format!("library/audio/{id}")
}

pub fn is_library_blob(path: &str) -> bool {
    path.starts_with("library/audio/")
}

#[derive(Debug, Clone)]
pub struct NewLibraryEntry {
    pub kind: AudioLibraryKind,
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
    pub duration_ms: Option<i32>,
    pub mime: String,
    pub bytes: i64,
    pub sha256: String,
    pub source: String,
    pub source_id: String,
    pub license: String,
    pub attribution: String,
}

/// Insert a library row. Caller has already written `blob_path`.
pub async fn insert(
    conn: &mut PgConnection,
    id: Uuid,
    blob_path: &str,
    entry: NewLibraryEntry,
) -> AppResult<AudioLibraryEntry> {
    let row = sqlx::query_as::<_, AudioLibraryEntry>(sqlx::AssertSqlSafe(format!(
        "insert into audio_library (id, kind, title, description, tags, duration_ms, blob_path, \
            mime, bytes, sha256, source, source_id, license, attribution) \
         values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14) \
         returning {COLUMNS}"
    )))
    .bind(id)
    .bind(entry.kind.as_str())
    .bind(entry.title.trim())
    .bind(entry.description.trim())
    .bind(&entry.tags)
    .bind(entry.duration_ms)
    .bind(blob_path)
    .bind(&entry.mime)
    .bind(entry.bytes)
    .bind(&entry.sha256)
    .bind(entry.source.trim())
    .bind(entry.source_id.trim())
    .bind(entry.license.trim())
    .bind(entry.attribution.trim())
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| {
        if let sqlx::Error::Database(db) = &e
            && db.constraint() == Some("audio_library_source_source_id_key")
        {
            return AppError::Conflict("that source file is already in the library".into());
        }
        AppError::from(e)
    })?;
    Ok(row)
}

pub async fn get(pool: &PgPool, id: Uuid) -> AppResult<AudioLibraryEntry> {
    sqlx::query_as::<_, AudioLibraryEntry>(sqlx::AssertSqlSafe(format!(
        "select {COLUMNS} from audio_library where id = $1"
    )))
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(AppError::from)
}

pub async fn find_by_source(
    pool: &PgPool,
    source: &str,
    source_id: &str,
) -> AppResult<Option<AudioLibraryEntry>> {
    Ok(
        sqlx::query_as::<_, AudioLibraryEntry>(sqlx::AssertSqlSafe(format!(
            "select {COLUMNS} from audio_library where source = $1 and source_id = $2"
        )))
        .bind(source)
        .bind(source_id)
        .fetch_optional(pool)
        .await?,
    )
}

/// Ranked text search within one kind. Empty query returns newest first.
pub async fn search(
    pool: &PgPool,
    kind: AudioLibraryKind,
    query: &str,
    limit: i64,
) -> AppResult<Vec<AudioLibraryEntry>> {
    let limit = limit.clamp(1, 25);
    let q = query.trim();
    if q.is_empty() {
        return Ok(
            sqlx::query_as::<_, AudioLibraryEntry>(sqlx::AssertSqlSafe(format!(
                "select {COLUMNS} from audio_library where kind = $1 \
             order by created_at desc limit $2"
            )))
            .bind(kind.as_str())
            .bind(limit)
            .fetch_all(pool)
            .await?,
        );
    }
    let like = format!("%{}%", escape_like(q));
    Ok(sqlx::query_as::<_, AudioLibraryEntry>(sqlx::AssertSqlSafe(format!(
        "select {COLUMNS} from audio_library a \
          where a.kind = $1 \
            and ( \
                to_tsvector('english', coalesce(a.title,'') || ' ' || coalesce(a.description,'')) \
                  @@ websearch_to_tsquery('english', $2) \
                or a.title ilike $3 escape '\\' \
                or a.description ilike $3 escape '\\' \
                or a.title % $2 \
                or $2 <% a.description \
                or exists (select 1 from unnest(a.tags) t where t ilike $3 escape '\\') \
            ) \
          order by greatest( \
                ts_rank(to_tsvector('english', coalesce(a.title,'') || ' ' || coalesce(a.description,'')), \
                        websearch_to_tsquery('english', $2)), \
                similarity(a.title, $2), \
                word_similarity($2, a.description) \
            ) desc, a.created_at desc \
          limit $4"
    )))
    .bind(kind.as_str())
    .bind(q)
    .bind(&like)
    .bind(limit)
    .fetch_all(pool)
    .await?)
}

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Create `Library` and `Library/music` or `Library/sfx` when missing.
pub async fn ensure_project_dirs(
    conn: &mut PgConnection,
    project_id: Uuid,
    kind: AudioLibraryKind,
) -> AppResult<()> {
    ensure_one_dir(
        conn,
        project_id,
        "Library",
        "Shared royalty-free music and sound effects used across videos.",
    )
    .await?;
    ensure_one_dir(conn, project_id, kind.dir_path(), kind.dir_description()).await?;
    Ok(())
}

async fn ensure_one_dir(
    conn: &mut PgConnection,
    project_id: Uuid,
    path: &str,
    description: &str,
) -> AppResult<()> {
    if assets::find_dir(conn, project_id, path).await?.is_some() {
        return Ok(());
    }
    let (parent_path, name) = match path.rsplit_once('/') {
        Some((p, n)) => (p, n),
        None => ("", path),
    };
    let parent_id = if parent_path.is_empty() {
        None
    } else {
        Some(
            assets::find_dir(conn, project_id, parent_path)
                .await?
                .ok_or_else(|| {
                    AppError::Invalid(format!("parent directory \"{parent_path}\" does not exist"))
                })?
                .id,
        )
    };
    sqlx::query(
        "insert into asset_dirs (id, project_id, parent_id, name, path, description) \
         values ($1,$2,$3,$4,$5,$6) on conflict (project_id, path) do nothing",
    )
    .bind(new_id())
    .bind(project_id)
    .bind(parent_id)
    .bind(name)
    .bind(path)
    .bind(description)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Attach a library entry as a project asset sharing the same blob.
pub async fn attach_to_project(
    conn: &mut PgConnection,
    project_id: Uuid,
    entry: &AudioLibraryEntry,
) -> AppResult<Asset> {
    let kind = AudioLibraryKind::parse(&entry.kind)
        .ok_or_else(|| AppError::Invalid(format!("unknown library kind {}", entry.kind)))?;
    ensure_project_dirs(conn, project_id, kind).await?;

    let ext = extension_for_mime(&entry.mime);
    let stem = sanitize_filename(&entry.title);
    let name = format!("{stem}{ext}");
    let description = if entry.description.trim().is_empty() {
        format!(
            "{} ({}, {}). {}",
            entry.title,
            kind.as_str().replace('_', " "),
            entry.source,
            entry.attribution
        )
        .trim()
        .to_owned()
    } else {
        entry.description.clone()
    };
    let mut tags = entry.tags.clone();
    tags.push(kind.as_str().to_owned());
    tags.push(entry.source.clone());

    assets::insert_asset(
        conn,
        NewAsset {
            id: new_id(),
            project_id,
            dir_path: kind.dir_path().to_owned(),
            name,
            kind: AssetKind::Audio,
            blob_path: entry.blob_path.clone(),
            mime: entry.mime.clone(),
            width: None,
            height: None,
            duration_ms: entry.duration_ms,
            sha256: entry.sha256.clone(),
            description,
            tags,
            transcript: None,
            bytes: entry.bytes,
        },
        true,
    )
    .await
}

fn sanitize_filename(title: &str) -> String {
    let s: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ' ' {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .take(8)
        .collect::<Vec<_>>()
        .join(" ");
    if s.is_empty() { "audio".into() } else { s }
}

fn extension_for_mime(mime: &str) -> &'static str {
    match mime {
        "audio/mpeg" | "audio/mp3" => ".mp3",
        "audio/wav" | "audio/x-wav" | "audio/wave" => ".wav",
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" => ".m4a",
        "audio/ogg" | "audio/opus" => ".ogg",
        "audio/flac" => ".flac",
        _ => ".mp3",
    }
}

/// True when any library row still points at this blob.
pub async fn blob_still_referenced(pool: &PgPool, blob_path: &str) -> AppResult<bool> {
    let n: (i64,) = sqlx::query_as("select count(*) from audio_library where blob_path = $1")
        .bind(blob_path)
        .fetch_one(pool)
        .await?;
    Ok(n.0 > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kinds_and_dirs() {
        assert_eq!(
            AudioLibraryKind::parse("sound effects"),
            Some(AudioLibraryKind::SoundEffect)
        );
        assert_eq!(
            AudioLibraryKind::parse("music"),
            Some(AudioLibraryKind::Music)
        );
        assert_eq!(AudioLibraryKind::Music.dir_path(), "Library/music");
        assert!(is_library_blob("library/audio/01a1"));
        assert!(!is_library_blob("assets/01a1/x"));
    }

    #[test]
    fn sanitizes_titles() {
        assert_eq!(sanitize_filename("Soft Phone Ping!"), "Soft Phone Ping");
        assert_eq!(sanitize_filename("!!!"), "audio");
    }
}
