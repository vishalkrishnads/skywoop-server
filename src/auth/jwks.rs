//! JWKS fetch + cache for Better Auth EdDSA JWTs.

use std::collections::HashMap;

use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use tokio::sync::RwLock;

use crate::config::Config;
use crate::error::{AppError, AppResult};

use super::JwtClaims;

#[derive(Debug, Deserialize)]
struct JwksDoc {
    keys: Vec<Jwk>,
}

#[derive(Debug, Clone, Deserialize)]
struct Jwk {
    kid: Option<String>,
    kty: String,
    #[serde(default)]
    crv: Option<String>,
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
}

pub struct JwksCache {
    url: String,
    http: reqwest::Client,
    keys: RwLock<HashMap<String, DecodingKey>>,
}

impl JwksCache {
    pub fn new(url: String) -> Self {
        Self {
            url,
            http: reqwest::Client::new(),
            keys: RwLock::new(HashMap::new()),
        }
    }

    pub async fn verify(&self, token: &str, cfg: &Config) -> AppResult<JwtClaims> {
        let header = decode_header(token).map_err(|_| AppError::Unauthorized)?;
        let kid = header.kid.clone().unwrap_or_default();

        if self.keys.read().await.get(&kid).is_none() {
            self.refresh().await?;
        }

        // Refresh once more if kid still missing (key rotation).
        if !kid.is_empty() && self.keys.read().await.get(&kid).is_none() {
            self.refresh().await?;
        }

        let key = {
            let guard = self.keys.read().await;
            guard
                .get(&kid)
                .cloned()
                .or_else(|| {
                    if guard.len() == 1 {
                        guard.values().next().cloned()
                    } else {
                        None
                    }
                })
                .ok_or(AppError::Unauthorized)?
        };

        let mut validation = Validation::new(header.alg);
        validation.validate_exp = true;
        if let Some(iss) = cfg.auth_issuer.as_deref() {
            validation.set_issuer(&[iss]);
        } else {
            validation.iss = None;
        }
        if let Some(aud) = cfg.auth_audience.as_deref() {
            validation.set_audience(&[aud]);
        } else {
            validation.validate_aud = false;
        }
        validation.algorithms = vec![
            Algorithm::EdDSA,
            Algorithm::RS256,
            Algorithm::ES256,
            header.alg,
        ];

        let data = decode::<JwtClaims>(token, &key, &validation).map_err(|e| {
            tracing::debug!(error = %e, "jwt verify failed");
            AppError::Unauthorized
        })?;
        Ok(data.claims)
    }

    async fn refresh(&self) -> AppResult<()> {
        let doc: JwksDoc = self
            .http
            .get(&self.url)
            .send()
            .await
            .map_err(|e| AppError::Upstream(format!("jwks fetch: {e}")))?
            .error_for_status()
            .map_err(|e| AppError::Upstream(format!("jwks status: {e}")))?
            .json()
            .await
            .map_err(|e| AppError::Upstream(format!("jwks json: {e}")))?;

        let mut map = HashMap::new();
        for jwk in doc.keys {
            let kid = jwk
                .kid
                .clone()
                .unwrap_or_else(|| format!("{}-{}", jwk.kty, map.len()));
            if let Some(key) = decoding_key_from_jwk(&jwk) {
                map.insert(kid, key);
            } else {
                tracing::warn!(kid = %kid, kty = %jwk.kty, "unsupported jwk skipped");
            }
        }
        *self.keys.write().await = map;
        Ok(())
    }
}

fn decoding_key_from_jwk(jwk: &Jwk) -> Option<DecodingKey> {
    match jwk.kty.as_str() {
        "OKP" if jwk.crv.as_deref() == Some("Ed25519") => {
            let x = jwk.x.as_deref()?;
            DecodingKey::from_ed_components(x).ok()
        }
        "RSA" => {
            let n = jwk.n.as_deref()?;
            let e = jwk.e.as_deref()?;
            DecodingKey::from_rsa_components(n, e).ok()
        }
        _ => None,
    }
}
