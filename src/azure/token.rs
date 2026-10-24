use std::sync::Arc;

use azure_core::credentials::TokenCredential;
use azure_identity::{
    DeveloperToolsCredential, ManagedIdentityCredential, ManagedIdentityCredentialOptions,
    UserAssignedId,
};
use chrono::{DateTime, Duration, Utc};
use tokio::sync::RwLock;

use crate::config::Config;

pub mod scope {
    pub const AZURE_OPENAI: &str = "https://ai.azure.com/.default";
    pub const POSTGRES: &str = "https://ossrdbms-aad.database.windows.net/.default";
    pub const STORAGE: &str = "https://storage.azure.com/.default";
    pub const KEYVAULT: &str = "https://vault.azure.net/.default";
}

pub fn credential(cfg: &Config) -> anyhow::Result<Arc<dyn TokenCredential>> {
    Ok(match &cfg.azure_client_id {
        Some(id) => ManagedIdentityCredential::new(Some(ManagedIdentityCredentialOptions {
            user_assigned_id: Some(UserAssignedId::ClientId(id.clone())),
            ..Default::default()
        }))?,
        None => DeveloperToolsCredential::new(None)?,
    })
}

pub struct TokenCache {
    cred: Arc<dyn TokenCredential>,
    scope: &'static str,
    cached: RwLock<Option<(String, DateTime<Utc>)>>,
}

impl TokenCache {
    pub fn new(cred: Arc<dyn TokenCredential>, scope: &'static str) -> Arc<Self> {
        Arc::new(Self {
            cred,
            scope,
            cached: RwLock::new(None),
        })
    }

    pub async fn get(&self) -> anyhow::Result<String> {
        if let Some((t, exp)) = &*self.cached.read().await
            && *exp - Utc::now() > Duration::minutes(5)
        {
            return Ok(t.clone());
        }
        let tok = self.cred.get_token(&[self.scope], None).await?;
        let exp = DateTime::<Utc>::from_timestamp(tok.expires_on.unix_timestamp(), 0)
            .ok_or_else(|| anyhow::anyhow!("invalid token expiry"))?;
        let s = tok.token.secret().to_string();
        *self.cached.write().await = Some((s.clone(), exp));
        Ok(s)
    }
}
