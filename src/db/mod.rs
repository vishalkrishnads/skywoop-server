use std::str::FromStr;
use std::sync::Arc;

use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use crate::azure::token::{TokenCache, scope};
use crate::config::{Config, DbAuth};

pub async fn connect(cfg: &Config, pg_tokens: Option<Arc<TokenCache>>) -> anyhow::Result<PgPool> {
    let base = PgConnectOptions::from_str(&cfg.database_url)?;
    let opts = match cfg.db_auth {
        DbAuth::Password => base.clone(),
        DbAuth::Entra => {
            let tokens = pg_tokens
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Entra DB auth requires a postgres TokenCache"))?;
            base.clone().password(&tokens.get().await?)
        }
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .max_lifetime(std::time::Duration::from_secs(30 * 60))
        .connect_with(opts)
        .await?;

    if cfg.db_auth == DbAuth::Entra {
        let tokens = pg_tokens.expect("checked above");
        let pool2 = pool.clone();
        let base2 = base.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(15 * 60)).await;
                match tokens.get().await {
                    Ok(t) => pool2.set_connect_options(base2.clone().password(&t)),
                    Err(e) => tracing::warn!(error = ?e, "postgres token refresh failed"),
                }
            }
        });
    }

    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

pub fn postgres_token_cache(
    cfg: &Config,
    cred: Arc<dyn azure_core::credentials::TokenCredential>,
) -> Option<Arc<TokenCache>> {
    match cfg.db_auth {
        DbAuth::Entra => Some(TokenCache::new(cred, scope::POSTGRES)),
        DbAuth::Password => None,
    }
}
