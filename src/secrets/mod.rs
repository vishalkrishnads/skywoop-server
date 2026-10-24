pub mod keyvault;
pub mod local;

use async_trait::async_trait;

#[async_trait]
pub trait Secrets: Send + Sync {
    async fn get(&self, name: &str) -> anyhow::Result<String>;
    async fn set(&self, name: &str, value: &str) -> anyhow::Result<()>;
    async fn delete(&self, name: &str) -> anyhow::Result<()>;
}

pub use keyvault::KeyVaultSecrets;
pub use local::LocalSecrets;
