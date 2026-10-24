// Foundation modules intentionally expose APIs used by later milestones.
#![allow(dead_code)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]
// sqlx needs `&mut *tx` to coerce Transaction → PgConnection for Executor.
#![allow(clippy::explicit_auto_deref)]
// Inherited lint noise from upstream port; tighten later.
#![allow(clippy::collapsible_if)]
#![allow(clippy::collapsible_match)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::useless_borrows_in_formatting)]
#![allow(clippy::assertions_on_constants)]
#![allow(clippy::vec_init_then_push)]
#![allow(clippy::useless_conversion)]
#![allow(clippy::manual_ignore_case_cmp)]
#![allow(clippy::trim_split_whitespace)]
#![allow(clippy::unused_async)]
#![allow(clippy::manual_async_fn)]

mod agent;
mod ai;
mod analysis;
mod api;
mod appstore;
mod auth;
mod azure;
mod clock;
mod config;
mod crawl;
mod db;
mod domain;
mod error;
mod jobs;
mod meta;
mod plan_format;
mod playbook_doc;
mod router;
mod scheduling;
mod secrets;
mod state;
mod storage;
mod tracking;
mod trends;
mod video_guide_doc;

use std::sync::Arc;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use crate::ai::{AiClient, Effort, RenderedPrompt, TextRole};
use crate::appstore::AscClient;
use crate::auth::JwksCache;
use crate::azure::token::{self, TokenCache, scope};
use crate::clock::SystemClock;
use crate::config::{AppEnv, AuthMode, Config};
use crate::jobs::{DataJobQueue, UserJobQueue, scheduler, worker};
use crate::meta::MetaClient;
use crate::secrets::{KeyVaultSecrets, LocalSecrets};
use crate::state::AppState;
use crate::storage::{AzureBlobStore, LocalBlobStore};

#[derive(Parser, Debug)]
#[command(name = "skywoop", about = "Skywoop server")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run the HTTP API, job worker, and data-job scheduler
    Serve,
    /// Apply sqlx migrations and exit
    Migrate,
    /// Print the OpenAPI document to stdout
    Openapi,
    /// Smoke-test Azure OpenAI structured output
    AiSmoke,
    /// Run a one-shot sync kind (stub)
    Sync { kind: String },
    /// Fetch today's social trends via web search and print them as JSON
    Trends {
        /// ISO 3166-1 alpha-2 country, e.g. US
        #[arg(long, default_value = "US")]
        country: String,
        /// instagram | facebook
        #[arg(long, default_value = "instagram")]
        platform: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_tracing();

    match cli.command {
        Commands::Serve => serve().await,
        Commands::Migrate => migrate().await,
        Commands::Openapi => openapi().await,
        Commands::AiSmoke => ai_smoke().await,
        Commands::Sync { kind } => sync(kind).await,
        Commands::Trends { country, platform } => trends_cmd(country, platform).await,
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn build_state() -> anyhow::Result<AppState> {
    let cfg = Arc::new(Config::from_env()?);
    let cred = token::credential(&cfg)?;

    let pg_tokens = db::postgres_token_cache(&cfg, cred.clone());
    let db = db::connect(&cfg, pg_tokens).await?;

    let openai_tokens = match cfg.app_env {
        AppEnv::Azure => Some(TokenCache::new(cred.clone(), scope::AZURE_OPENAI)),
        AppEnv::Local => None,
    };
    let ai = AiClient::new(cfg.clone(), openai_tokens, Some(db.clone()))?;

    let (blob, local_blob, secrets): (
        Arc<dyn storage::BlobStore>,
        Option<Arc<LocalBlobStore>>,
        Arc<dyn secrets::Secrets>,
    ) = match cfg.app_env {
        AppEnv::Local => {
            let local = LocalBlobStore::new(
                cfg.local_asset_dir.clone(),
                cfg.public_base_url.clone(),
                b"local-dev-signing-key".to_vec(),
            );
            let blob: Arc<dyn storage::BlobStore> = local.clone();
            let secrets = LocalSecrets::new(cfg.local_secrets_file.clone());
            (blob, Some(local), secrets)
        }
        AppEnv::Azure => {
            let account = cfg
                .blob_account
                .clone()
                .ok_or_else(|| anyhow::anyhow!("BLOB_ACCOUNT required when APP_ENV=azure"))?;
            let vault = cfg
                .keyvault_url
                .clone()
                .ok_or_else(|| anyhow::anyhow!("KEYVAULT_URL required when APP_ENV=azure"))?;
            let blob = AzureBlobStore::new(account, cfg.blob_container.clone(), cred.clone())?;
            let secrets = KeyVaultSecrets::new(&vault, cred.clone())?;
            (blob, None, secrets)
        }
    };

    let meta = MetaClient::new(cfg.clone())?;
    let asc = AscClient::new(cfg.clone())?;
    let user_jobs = UserJobQueue::new(db.clone());
    let jwks = match cfg.auth_mode {
        AuthMode::Jwt => Some(Arc::new(JwksCache::new(
            cfg.auth_jwks_url
                .as_ref()
                .map(|u| u.to_string())
                .ok_or_else(|| anyhow::anyhow!("AUTH_JWKS_URL required"))?,
        ))),
        AuthMode::DevToken => None,
    };

    Ok(AppState {
        cfg,
        db,
        clock: Arc::new(SystemClock),
        ai,
        blob,
        local_blob,
        secrets,
        meta,
        asc,
        user_jobs,
        jwks,
    })
}

async fn serve() -> anyhow::Result<()> {
    let state = build_state().await?;
    let data_jobs = DataJobQueue::new(state.db.clone());

    tokio::spawn(worker::run_worker(state.clone()));
    tokio::spawn(scheduler::run_scheduler(
        state.db.clone(),
        data_jobs,
        state.user_jobs.clone(),
        state.clock.clone(),
    ));

    let addr = state.cfg.listen_addr.clone();
    let app = router::router(state);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(%addr, "listening");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn migrate() -> anyhow::Result<()> {
    let cfg = Config::from_env()?;
    let cred = token::credential(&cfg)?;
    let pg_tokens = db::postgres_token_cache(&cfg, cred);
    let _pool = db::connect(&cfg, pg_tokens).await?;
    tracing::info!("migrations applied");
    Ok(())
}

async fn openapi() -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(&router::openapi())?);
    Ok(())
}

async fn ai_smoke() -> anyhow::Result<()> {
    let cfg = Arc::new(Config::from_env()?);
    let openai_tokens = if cfg.azure_openai_api_key.is_some() {
        None
    } else {
        match token::credential(&cfg) {
            Ok(cred) => Some(TokenCache::new(cred, scope::AZURE_OPENAI)),
            Err(e) => {
                tracing::warn!("ai-smoke: credential setup failed ({e}); skipping");
                return Ok(());
            }
        }
    };
    let ai = AiClient::new(cfg, openai_tokens, None)?;

    match ai.bearer().await {
        Ok(_) => tracing::info!("ai-smoke: obtained Azure OpenAI bearer credentials"),
        Err(e) => {
            tracing::warn!("ai-smoke: no credentials ({e}); skipping structured/image calls");
            return Ok(());
        }
    }

    let prompt = RenderedPrompt {
        system: "You list a few social trends as structured JSON. Prefer short ids.".into(),
        user: "List 3 trends that a consumer app could hitch onto for Instagram in the US.".into(),
        version: "smoke".into(),
    };
    match ai
        .structured::<ai::planner::SocialTrendsOut>(
            TextRole::Planner,
            "ai_smoke_trends",
            &prompt,
            &[],
            Effort::Low,
        )
        .await
    {
        Ok(out) => tracing::info!(items = out.items.len(), "ai-smoke: structured output ok"),
        Err(e) => tracing::warn!("ai-smoke: structured output failed: {e}"),
    }

    Ok(())
}

async fn trends_cmd(country: String, platform: String) -> anyhow::Result<()> {
    let cfg = Arc::new(Config::from_env()?);
    let openai_tokens = if cfg.azure_openai_api_key.is_some() {
        None
    } else {
        Some(TokenCache::new(
            token::credential(&cfg)?,
            scope::AZURE_OPENAI,
        ))
    };
    let ai = AiClient::new(cfg, openai_tokens, None)?;
    let out = ai::planner::social_trends_via_web(&ai, &country.to_ascii_uppercase(), &platform)
        .await
        .map_err(|e| anyhow::anyhow!("social trends failed: {e}"))?;
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

async fn sync(kind: String) -> anyhow::Result<()> {
    tracing::info!(%kind, "sync stub — not implemented yet");
    Ok(())
}
