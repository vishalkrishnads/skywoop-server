use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use minijinja::{Environment, context};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::storage::{BlobStore, LocalBlobStore};

const TEMPLATE: &str = include_str!("../../templates/link_page.html.j2");

#[derive(Debug, FromRow)]
struct ProjectName {
    name: String,
}

#[derive(Debug, Clone, Serialize, FromRow)]
struct LinkVideo {
    plan_video_id: Uuid,
    token: String,
    title: String,
}

#[derive(Debug, Deserialize)]
pub struct DevAssetQuery {
    pub exp: i64,
    pub sig: String,
}

/// `/l/{slug}` — HTML link-in-bio page from minijinja template.
///
/// Videos are deleted from storage after posting, so the page lists recent Instagram
/// videos by title only (each entry links through the tracked `/r/{token}` redirect).
pub async fn page(State(s): State<AppState>, Path(slug): Path<String>) -> AppResult<Html<String>> {
    let project = sqlx::query_as::<_, ProjectName>("select name from projects where slug = $1")
        .bind(&slug)
        .fetch_optional(&s.db)
        .await?
        .ok_or(AppError::NotFound)?;

    let videos = sqlx::query_as::<_, LinkVideo>(
        r#"
        select pv.id as plan_video_id, tl.token, pv.title
          from posts po
          join plan_videos pv on pv.id = po.plan_video_id
          join tracked_links tl on tl.plan_video_id = pv.id
          join projects p on p.id = po.project_id
         where p.slug = $1
           and po.platform = 'instagram'
           and po.status = 'published'
           and po.published_at >= now() - interval '14 days'
         order by po.published_at desc
        "#,
    )
    .bind(&slug)
    .fetch_all(&s.db)
    .await?;

    let mut env = Environment::new();
    env.add_template("link_page", TEMPLATE)
        .map_err(|e| AppError::Other(e.into()))?;
    let tmpl = env
        .get_template("link_page")
        .map_err(|e| AppError::Other(e.into()))?;
    let html = tmpl
        .render(context! {
            title => project.name,
            slug => slug,
            videos => videos,
        })
        .map_err(|e| AppError::Other(e.into()))?;

    Ok(Html(html))
}

/// Local-only HMAC-signed asset serve for `LocalBlobStore::public_read_url`.
pub async fn dev_assets(
    State(s): State<AppState>,
    Path(path): Path<String>,
    Query(q): Query<DevAssetQuery>,
) -> Result<Response, AppError> {
    let store: &LocalBlobStore = s.local_blob.as_deref().ok_or(AppError::NotFound)?;

    if !store.verify_sig(&path, q.exp, &q.sig) {
        return Err(AppError::Unauthorized);
    }

    let bytes = store.get(&path).await.map_err(|_| AppError::NotFound)?;
    let mut headers = HeaderMap::new();
    let content_type = match store.content_type(&path).await {
        Some(ct) => HeaderValue::from_str(&ct)
            .unwrap_or_else(|_| HeaderValue::from_static(mime_for_path(&path))),
        None => HeaderValue::from_static(mime_for_path(&path)),
    };
    headers.insert(header::CONTENT_TYPE, content_type);
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=60"),
    );
    Ok((StatusCode::OK, headers, bytes).into_response())
}

/// Local-only HMAC-signed `PUT` target for `LocalBlobStore::upload_url`.
pub async fn dev_upload(
    State(s): State<AppState>,
    Path(path): Path<String>,
    Query(q): Query<DevAssetQuery>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<StatusCode, AppError> {
    let store: &LocalBlobStore = s.local_blob.as_deref().ok_or(AppError::NotFound)?;
    if !store.verify_upload_sig(&path, q.exp, &q.sig) {
        return Err(AppError::Unauthorized);
    }
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream");
    store
        .put(&path, body.to_vec(), ct)
        .await
        .map_err(AppError::Other)?;
    Ok(StatusCode::CREATED)
}

fn mime_for_path(path: &str) -> &'static str {
    match path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    }
}
