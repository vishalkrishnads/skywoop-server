use std::sync::Arc;

use sqlx::PgPool;

use crate::ai::AiClient;
use crate::appstore::AscClient;
use crate::auth::JwksCache;
use crate::clock::Clock;
use crate::config::Config;
use crate::jobs::UserJobQueue;
use crate::meta::MetaClient;
use crate::secrets::Secrets;
use crate::storage::{BlobStore, LocalBlobStore};

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub db: PgPool,
    pub clock: Arc<dyn Clock>,
    pub ai: Arc<AiClient>,
    pub blob: Arc<dyn BlobStore>,
    /// Present only when `APP_ENV=local` (HMAC `/dev/assets` serve).
    pub local_blob: Option<Arc<LocalBlobStore>>,
    pub secrets: Arc<dyn Secrets>,
    pub meta: Arc<MetaClient>,
    pub asc: Arc<AscClient>,
    pub user_jobs: UserJobQueue,
    /// Present when `AUTH_MODE=jwt`.
    pub jwks: Option<Arc<JwksCache>>,
}
