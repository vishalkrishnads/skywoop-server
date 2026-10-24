pub mod azure;
pub mod local;

use async_trait::async_trait;
use chrono::Duration;
use url::Url;

#[async_trait]
pub trait BlobStore: Send + Sync {
    async fn put(&self, path: &str, bytes: Vec<u8>, content_type: &str) -> anyhow::Result<()>;
    async fn get(&self, path: &str) -> anyhow::Result<Vec<u8>>;
    /// Public, read-only, time-limited URL that Meta's servers can fetch.
    async fn public_read_url(&self, path: &str, ttl: Duration) -> anyhow::Result<Url>;
    /// Time-limited URL the client can `PUT` bytes to (Azure write SAS / local HMAC-signed PUT).
    /// The client must send `Content-Type: {content_type}`; Azure additionally requires
    /// `x-ms-blob-type: BlockBlob`.
    async fn upload_url(
        &self,
        path: &str,
        content_type: &str,
        ttl: Duration,
    ) -> anyhow::Result<Url>;
    /// Delete a blob. Missing blobs are not an error.
    async fn delete(&self, path: &str) -> anyhow::Result<()>;
    /// Size in bytes when the blob exists.
    async fn size(&self, path: &str) -> anyhow::Result<Option<u64>>;
}

pub use azure::AzureBlobStore;
pub use local::LocalBlobStore;
