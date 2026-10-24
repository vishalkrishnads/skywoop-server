use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tokio::fs;
use url::Url;

use super::BlobStore;

type HmacSha256 = Hmac<Sha256>;

pub struct LocalBlobStore {
    root: PathBuf,
    public_base: Url,
    signing_key: Vec<u8>,
}

impl LocalBlobStore {
    pub fn new(root: PathBuf, public_base: Url, signing_key: impl Into<Vec<u8>>) -> Arc<Self> {
        Arc::new(Self {
            root,
            public_base,
            signing_key: signing_key.into(),
        })
    }

    fn full_path(&self, path: &str) -> PathBuf {
        // Never allow traversal out of the store root.
        let safe: PathBuf = Path::new(path)
            .components()
            .filter(|c| matches!(c, std::path::Component::Normal(_)))
            .collect();
        self.root.join(safe)
    }

    pub fn verify_sig(&self, path: &str, exp: i64, sig: &str) -> bool {
        if exp < Utc::now().timestamp() {
            return false;
        }
        let expected = self.sign(path, exp);
        expected == sig
    }

    pub fn verify_upload_sig(&self, path: &str, exp: i64, sig: &str) -> bool {
        if exp < Utc::now().timestamp() {
            return false;
        }
        self.sign_scoped("put", path, exp) == sig
    }

    fn sign_scoped(&self, scope: &str, path: &str, exp: i64) -> String {
        let mut mac =
            HmacSha256::new_from_slice(&self.signing_key).expect("HMAC key length is valid");
        mac.update(format!("{scope}:{path}:{exp}").as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    fn sign(&self, path: &str, exp: i64) -> String {
        let mut mac =
            HmacSha256::new_from_slice(&self.signing_key).expect("HMAC key length is valid");
        mac.update(format!("{path}:{exp}").as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }
}

#[async_trait]
impl BlobStore for LocalBlobStore {
    async fn put(&self, path: &str, bytes: Vec<u8>, content_type: &str) -> anyhow::Result<()> {
        let full = self.full_path(path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(&full, bytes).await?;
        // Sidecar so extension-less blob paths still serve with the right Content-Type.
        if !content_type.is_empty() {
            fs::write(sidecar(&full), content_type).await?;
        }
        Ok(())
    }

    async fn get(&self, path: &str) -> anyhow::Result<Vec<u8>> {
        let full = self.full_path(path);
        Ok(fs::read(full).await?)
    }

    async fn public_read_url(&self, path: &str, ttl: Duration) -> anyhow::Result<Url> {
        let exp = (Utc::now() + ttl).timestamp();
        let sig = self.sign(path, exp);
        let mut url = self.public_base.clone();
        let joined = format!(
            "dev/assets/{}?exp={exp}&sig={sig}",
            path.trim_start_matches('/')
        );
        url = url.join(&joined)?;
        Ok(url)
    }

    async fn upload_url(
        &self,
        path: &str,
        _content_type: &str,
        ttl: Duration,
    ) -> anyhow::Result<Url> {
        let exp = (Utc::now() + ttl).timestamp();
        let sig = self.sign_scoped("put", path, exp);
        let joined = format!(
            "dev/upload/{}?exp={exp}&sig={sig}",
            path.trim_start_matches('/')
        );
        Ok(self.public_base.join(&joined)?)
    }

    async fn delete(&self, path: &str) -> anyhow::Result<()> {
        let _ = fs::remove_file(sidecar(&self.full_path(path))).await;
        match fs::remove_file(self.full_path(path)).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    async fn size(&self, path: &str) -> anyhow::Result<Option<u64>> {
        match fs::metadata(self.full_path(path)).await {
            Ok(m) => Ok(Some(m.len())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

fn sidecar(full: &Path) -> PathBuf {
    let mut name = full.file_name().map(|n| n.to_owned()).unwrap_or_default();
    name.push(".ct");
    full.with_file_name(name)
}

impl LocalBlobStore {
    /// Content type recorded at upload time, when known.
    pub async fn content_type(&self, path: &str) -> Option<String> {
        fs::read_to_string(sidecar(&self.full_path(path)))
            .await
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    }
}

pub fn ensure_parent(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}
