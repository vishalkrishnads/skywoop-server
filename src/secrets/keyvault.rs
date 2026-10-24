use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use azure_core::credentials::TokenCredential;
use azure_core::http::RequestContent;
use azure_security_keyvault_secrets::SecretClient;
use azure_security_keyvault_secrets::models::SetSecretParameters;
use chrono::{DateTime, Duration, Utc};
use tokio::sync::RwLock;

use super::Secrets;

struct CacheEntry {
    value: String,
    fetched_at: DateTime<Utc>,
}

pub struct KeyVaultSecrets {
    client: SecretClient,
    cache: RwLock<HashMap<String, CacheEntry>>,
}

impl KeyVaultSecrets {
    pub fn new(vault_url: &str, cred: Arc<dyn TokenCredential>) -> anyhow::Result<Arc<Self>> {
        let client = SecretClient::new(vault_url, cred, None)?;
        Ok(Arc::new(Self {
            client,
            cache: RwLock::new(HashMap::new()),
        }))
    }
}

#[async_trait]
impl Secrets for KeyVaultSecrets {
    async fn get(&self, name: &str) -> anyhow::Result<String> {
        if let Some(entry) = self.cache.read().await.get(name)
            && Utc::now() - entry.fetched_at < Duration::minutes(10)
        {
            return Ok(entry.value.clone());
        }
        let secret = self.client.get_secret(name, None).await?.into_model()?;
        let value = secret
            .value
            .ok_or_else(|| anyhow::anyhow!("secret {name} has no value"))?;
        self.cache.write().await.insert(
            name.to_owned(),
            CacheEntry {
                value: value.clone(),
                fetched_at: Utc::now(),
            },
        );
        Ok(value)
    }

    async fn set(&self, name: &str, value: &str) -> anyhow::Result<()> {
        let params = SetSecretParameters {
            value: Some(value.to_owned()),
            ..Default::default()
        };
        let content: RequestContent<SetSecretParameters> = params.try_into()?;
        self.client.set_secret(name, content, None).await?;
        self.cache.write().await.insert(
            name.to_owned(),
            CacheEntry {
                value: value.to_owned(),
                fetched_at: Utc::now(),
            },
        );
        Ok(())
    }

    async fn delete(&self, name: &str) -> anyhow::Result<()> {
        // Soft-delete; purge is not required for MVP token cleanup.
        self.client.delete_secret(name, None).await?;
        self.cache.write().await.remove(name);
        Ok(())
    }
}
