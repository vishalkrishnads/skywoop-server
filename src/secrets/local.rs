use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use tokio::sync::RwLock;

use super::Secrets;

struct CacheEntry {
    value: String,
    fetched_at: DateTime<Utc>,
}

pub struct LocalSecrets {
    path: PathBuf,
    cache: RwLock<HashMap<String, CacheEntry>>,
}

impl LocalSecrets {
    pub fn new(path: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            path,
            cache: RwLock::new(HashMap::new()),
        })
    }

    async fn load_file(&self) -> anyhow::Result<HashMap<String, String>> {
        match tokio::fs::read_to_string(&self.path).await {
            Ok(s) => Ok(serde_json::from_str(&s)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
            Err(e) => Err(e.into()),
        }
    }

    async fn save_file(&self, map: &HashMap<String, String>) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let data = serde_json::to_string_pretty(map)?;
        tokio::fs::write(&self.path, data).await?;
        Ok(())
    }
}

#[async_trait]
impl Secrets for LocalSecrets {
    async fn get(&self, name: &str) -> anyhow::Result<String> {
        if let Some(entry) = self.cache.read().await.get(name)
            && Utc::now() - entry.fetched_at < Duration::minutes(10)
        {
            return Ok(entry.value.clone());
        }
        let map = self.load_file().await?;
        let value = map
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("secret not found: {name}"))?;
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
        let mut map = self.load_file().await?;
        map.insert(name.to_owned(), value.to_owned());
        self.save_file(&map).await?;
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
        let mut map = self.load_file().await?;
        map.remove(name);
        self.save_file(&map).await?;
        self.cache.write().await.remove(name);
        Ok(())
    }
}
