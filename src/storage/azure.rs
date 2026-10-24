use std::sync::Arc;

use async_trait::async_trait;
use azure_core::credentials::TokenCredential;
use azure_core::http::{RequestContent, Url as AzureUrl, XmlFormat};
use azure_core::time::{Duration as AzureDuration, OffsetDateTime};
use azure_storage_blob::BlobServiceClient;
use azure_storage_blob::models::{BlobClientUploadOptions, KeyInfo};
use azure_storage_common::models::UserDelegationKey;
use azure_storage_sas::SasBuilder;
use chrono::Duration;
use tokio::sync::RwLock;
use url::Url;

use super::BlobStore;

pub struct AzureBlobStore {
    account: String,
    container: String,
    service: BlobServiceClient,
    udk: RwLock<Option<(UserDelegationKey, OffsetDateTime)>>,
}

impl AzureBlobStore {
    pub fn new(
        account: String,
        container: String,
        cred: Arc<dyn TokenCredential>,
    ) -> anyhow::Result<Arc<Self>> {
        let endpoint = AzureUrl::parse(&format!("https://{account}.blob.core.windows.net"))?;
        let service = BlobServiceClient::new(endpoint, Some(cred), None)?;
        Ok(Arc::new(Self {
            account,
            container,
            service,
            udk: RwLock::new(None),
        }))
    }

    async fn delegation_key(&self) -> anyhow::Result<UserDelegationKey> {
        if let Some((key, exp)) = &*self.udk.read().await
            && *exp - OffsetDateTime::now_utc() > AzureDuration::days(1)
        {
            return Ok(key.clone());
        }
        let now = OffsetDateTime::now_utc();
        let key_expiry = now + AzureDuration::days(7);
        let key_info = KeyInfo {
            start: Some(now),
            expiry: Some(key_expiry),
            ..Default::default()
        };
        let request_content: RequestContent<KeyInfo, XmlFormat> = key_info.try_into()?;
        let key = self
            .service
            .get_user_delegation_key(request_content, None)
            .await?
            .into_model()?;
        let cache_until = now + AzureDuration::days(6);
        *self.udk.write().await = Some((key.clone(), cache_until));
        Ok(key)
    }
}

#[async_trait]
impl BlobStore for AzureBlobStore {
    async fn put(&self, path: &str, bytes: Vec<u8>, content_type: &str) -> anyhow::Result<()> {
        let client = self.service.blob_client(&self.container, path);
        let opts = BlobClientUploadOptions {
            blob_content_type: Some(content_type.to_owned()),
            ..Default::default()
        };
        client
            .upload(RequestContent::from(bytes), Some(opts))
            .await?;
        Ok(())
    }

    async fn get(&self, path: &str) -> anyhow::Result<Vec<u8>> {
        let client = self.service.blob_client(&self.container, path);
        let result = client.download(None).await?;
        let body = result.body.collect().await?;
        Ok(body.to_vec())
    }

    async fn public_read_url(&self, path: &str, ttl: Duration) -> anyhow::Result<Url> {
        let udk = self.delegation_key().await?;
        let secs = ttl.num_seconds().max(1);
        let expiry = OffsetDateTime::now_utc() + AzureDuration::seconds(secs);
        let sas = SasBuilder::new(&self.account, &udk, expiry)?
            .blob(&self.container, path)
            .read()
            .build();
        Ok(Url::parse(&format!(
            "https://{}.blob.core.windows.net/{}/{}?{}",
            self.account, self.container, path, sas
        ))?)
    }

    async fn upload_url(
        &self,
        path: &str,
        _content_type: &str,
        ttl: Duration,
    ) -> anyhow::Result<Url> {
        let udk = self.delegation_key().await?;
        let secs = ttl.num_seconds().max(1);
        let expiry = OffsetDateTime::now_utc() + AzureDuration::seconds(secs);
        let sas = SasBuilder::new(&self.account, &udk, expiry)?
            .blob(&self.container, path)
            .create()
            .write()
            .build();
        Ok(Url::parse(&format!(
            "https://{}.blob.core.windows.net/{}/{}?{}",
            self.account, self.container, path, sas
        ))?)
    }

    async fn delete(&self, path: &str) -> anyhow::Result<()> {
        let client = self.service.blob_client(&self.container, path);
        match client.delete(None).await {
            Ok(_) => Ok(()),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("404") || msg.contains("BlobNotFound") {
                    Ok(())
                } else {
                    Err(e.into())
                }
            }
        }
    }

    async fn size(&self, path: &str) -> anyhow::Result<Option<u64>> {
        let client = self.service.blob_client(&self.container, path);
        match client.get_properties(None).await {
            Ok(resp) => {
                let len = resp
                    .headers()
                    .get_optional_str(&azure_core::http::headers::CONTENT_LENGTH)
                    .and_then(|s| s.parse::<u64>().ok());
                Ok(Some(len.unwrap_or(0)))
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("404") || msg.contains("BlobNotFound") {
                    Ok(None)
                } else {
                    Err(e.into())
                }
            }
        }
    }
}
