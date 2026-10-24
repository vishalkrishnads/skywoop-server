use axum::http::{HeaderValue, Method};
use axum::routing::{get, put};
use axum::{Json, Router};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api;
use crate::auth;
use crate::config::AppEnv;
use crate::meta;
use crate::state::AppState;
use crate::tracking;

#[derive(OpenApi)]
#[openapi(info(title = "Skywoop API", version = "0.1.0"))]
struct ApiDoc;

/// All user-authenticated API routes (also the source of the OpenAPI document).
fn api_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        // Current user
        .routes(routes!(api::me::me))
        // Onboarding
        .routes(routes!(api::onboarding::start))
        .routes(routes!(
            api::onboarding::get_details,
            api::onboarding::put_details
        ))
        .routes(routes!(api::onboarding::post_context))
        .routes(routes!(api::onboarding::complete))
        // Experiments
        .routes(routes!(api::experiments::list))
        .routes(routes!(api::experiments::get))
        .routes(routes!(api::experiments::refresh))
        .routes(routes!(api::experiments::app_store_status))
        .routes(routes!(api::experiments::app_store_report_request))
        // Meta OAuth / connections
        .routes(routes!(meta::oauth::start))
        .routes(routes!(meta::oauth::select))
        .routes(routes!(meta::oauth::list))
        .routes(routes!(meta::oauth::candidates))
        .routes(routes!(meta::oauth::delete))
        // Diary / history
        .routes(routes!(api::diary::list))
        .routes(routes!(api::diary::create))
        .routes(routes!(api::diary::patch))
        .routes(routes!(api::history::get))
        // Home
        .routes(routes!(api::overview::get))
        // Plans (register `…/plans/latest` before `…/plans/{date}`)
        .routes(routes!(api::plans::list_plans))
        .routes(routes!(api::plans::get_latest))
        .routes(routes!(api::plans::draft_plan))
        .routes(routes!(api::plans::get_for_date))
        .routes(routes!(api::plans::get_plan, api::plans::put_plan))
        .routes(routes!(api::plans::list_versions))
        .routes(routes!(api::plans::set_status))
        .routes(routes!(api::plans::resume))
        .routes(routes!(api::plans::schedule_plan))
        .routes(routes!(api::plans::put_checklist))
        .routes(routes!(api::plans::list_comments, api::plans::post_comment))
        .routes(routes!(api::plans::delete_comment))
        .routes(routes!(api::plans::list_messages, api::plans::post_message))
        .routes(routes!(
            api::plans::list_audiences,
            api::plans::put_audiences
        ))
        // Video projects
        .routes(routes!(
            api::videos::create_for_plan_video,
            api::videos::get_for_plan_video
        ))
        .routes(routes!(
            api::videos::get_project,
            api::videos::put_project,
            api::videos::delete_project
        ))
        .routes(routes!(api::videos::apply))
        .routes(routes!(api::videos::list_revisions))
        .routes(routes!(api::videos::get_revision))
        .routes(routes!(api::videos::undo))
        .routes(routes!(api::videos::render_upload_url))
        .routes(routes!(api::videos::render_finalize))
        .routes(routes!(api::videos::render_fail))
        .routes(routes!(api::videos::get_render))
        // Asset manager
        .routes(routes!(api::assets::list_dirs, api::assets::create_dir))
        .routes(routes!(api::assets::patch_dir, api::assets::delete_dir))
        .routes(routes!(
            api::assets::list_assets,
            api::assets::finalize_upload
        ))
        .routes(routes!(api::assets::search_assets))
        .routes(routes!(api::assets::resolve_one, api::assets::resolve_many))
        .routes(routes!(api::assets::request_upload))
        .routes(routes!(
            api::assets::get_asset,
            api::assets::patch_asset,
            api::assets::delete_asset
        ))
        .routes(routes!(api::audio_library::search))
        .routes(routes!(api::audio_library::attach))
        .routes(routes!(api::audio_library::ingest))
        // Playbook, budgets, AI usage ledger
        .routes(routes!(
            api::text_templates::list,
            api::text_templates::upsert
        ))
        .routes(routes!(api::text_templates::delete))
        .routes(routes!(api::playbook::list, api::playbook::submit))
        .routes(routes!(api::playbook::read_doc))
        .routes(routes!(api::playbook::get))
        .routes(routes!(api::playbook::review))
        .routes(routes!(api::video_guide::read_doc))
        .routes(routes!(api::budget::get_budget, api::budget::put_budget))
        .routes(routes!(api::budget::get_usage, api::budget::post_usage))
        // Projects (list / get / patch — create & discover replaced by onboarding)
        .routes(routes!(api::projects::list))
        .routes(routes!(api::projects::list_timezones))
        .routes(routes!(api::projects::list_font_families))
        .routes(routes!(api::projects::list_countries))
        .routes(routes!(api::projects::list_industries))
        .routes(routes!(api::projects::get, api::projects::patch))
        .routes(routes!(api::projects::put_brand))
        .routes(routes!(api::projects::bio_link))
        .routes(routes!(
            api::projects::get_schedule,
            api::projects::put_schedule
        ))
        .routes(routes!(api::projects::put_app_store))
        .routes(routes!(api::trends::today))
        .routes(routes!(api::jobs::get))
}

/// The OpenAPI document (used by `skywoop openapi` and `/openapi.json`).
pub fn openapi() -> utoipa::openapi::OpenApi {
    api_routes().split_for_parts().1
}

pub fn router(s: AppState) -> Router {
    let (api, openapi) = api_routes()
        .layer(axum::middleware::from_fn_with_state(
            s.clone(),
            auth::require_user,
        ))
        .split_for_parts();

    let mut public = Router::new()
        .route("/r/{token}", get(tracking::redirect::handle))
        .route("/l/{slug}", get(tracking::link_page::page))
        .route("/oauth/meta/callback", get(meta::oauth::callback))
        .route("/healthz", get(api::health::healthz))
        .route("/readyz", get(api::health::ready));

    if s.cfg.app_env == AppEnv::Local {
        public = public
            .route("/dev/assets/{*path}", get(tracking::link_page::dev_assets))
            .route(
                "/dev/upload/{*path}",
                put(tracking::link_page::dev_upload)
                    .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024 * 1024)),
            );
    }

    let origins: Vec<HeaderValue> = s
        .cfg
        .app_origins
        .iter()
        .filter_map(|o| HeaderValue::from_str(o).ok())
        .collect();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            axum::http::header::AUTHORIZATION,
            axum::http::header::CONTENT_TYPE,
            axum::http::header::ACCEPT,
        ])
        .allow_credentials(true);

    let openapi_doc = openapi.clone();
    Router::new()
        .merge(api)
        .merge(public)
        .route(
            "/openapi.json",
            get(move || async move { Json(openapi_doc.clone()) }),
        )
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(s)
}

#[cfg(test)]
mod route_conflict_tests {
    use axum::Router;
    use axum::routing::{get, post};

    #[test]
    fn trends_and_project_routes_do_not_conflict() {
        let _: Router<()> = Router::new()
            .route("/projects", get(|| async {}))
            .route("/onboarding/projects", post(|| async {}))
            .route(
                "/projects/{pid}/onboarding/details",
                get(|| async {}).put(|| async {}),
            )
            .route("/projects/{id}", get(|| async {}).patch(|| async {}))
            .route("/projects/{pid}/overview", get(|| async {}))
            .route("/projects/{pid}/platforms/{p}/plans", get(|| async {}))
            .route(
                "/projects/{pid}/platforms/{p}/plans/latest",
                get(|| async {}),
            )
            .route("/plans/{id}", get(|| async {}).put(|| async {}))
            .route("/plans/{id}/status", post(|| async {}))
            .route("/plans/{id}/resume", post(|| async {}))
            .route(
                "/projects/{pid}/platforms/{p}/plans/{date}",
                get(|| async {}),
            )
            .route(
                "/projects/{pid}/platforms/{p}/plans/draft",
                post(|| async {}),
            )
            .route("/projects/{pid}/assets", get(|| async {}).post(|| async {}))
            .route("/projects/{pid}/assets/search", get(|| async {}))
            .route(
                "/projects/{pid}/assets/resolve",
                get(|| async {}).post(|| async {}),
            )
            .route("/projects/{pid}/assets/uploads", post(|| async {}))
            .route("/assets/{id}", get(|| async {}))
            .route("/projects/{id}/{platform}/trends/today", get(|| async {}));
    }
}
