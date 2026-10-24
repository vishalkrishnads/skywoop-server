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
        let header = decode_header(token).map_err(|e| {
            tracing::warn!(error = %e, "jwt header decode failed");
            AppError::Unauthorized
        })?;
        let kid = header.kid.clone().unwrap_or_default();

        if self.keys.read().await.get(&kid).is_none() {
            self.refresh().await?;
        }

        // Refresh once more if kid still missing (key rotation).
        if !kid.is_empty() && self.keys.read().await.get(&kid).is_none() {
            self.refresh().await?;
        }

        let key_count = self.keys.read().await.len();
        if key_count == 0 {
            tracing::error!(jwks_url = %self.url, "jwks refresh left zero usable keys");
            return Err(AppError::Upstream("auth keys unavailable".into()));
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
        };
        let Some(key) = key else {
            tracing::warn!(
                kid = %kid,
                key_count,
                "jwt kid not found in jwks"
            );
            return Err(AppError::Unauthorized);
        };

        decode_claims(
            token,
            &key,
            header.alg,
            cfg.auth_issuer.as_deref(),
            cfg.auth_audience.as_deref(),
        )
        .map_err(|e| {
            tracing::warn!(
                error = %e,
                kid = %kid,
                key_count,
                alg = ?header.alg,
                issuer = ?cfg.auth_issuer,
                audience = ?cfg.auth_audience,
                "jwt verify failed"
            );
            AppError::Unauthorized
        })
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
        let total = doc.keys.len();
        for jwk in doc.keys {
            let kid = jwk
                .kid
                .clone()
                .unwrap_or_else(|| format!("{}-{}", jwk.kty, map.len()));
            if let Some(key) = decoding_key_from_jwk(&jwk) {
                map.insert(kid, key);
            } else {
                tracing::warn!(
                    kid = %kid,
                    kty = %jwk.kty,
                    crv = ?jwk.crv,
                    "unsupported jwk skipped"
                );
            }
        }
        if map.is_empty() {
            tracing::error!(
                jwks_url = %self.url,
                total_keys = total,
                "jwks document had no usable keys"
            );
            return Err(AppError::Upstream("auth keys unavailable".into()));
        }
        tracing::info!(
            jwks_url = %self.url,
            usable = map.len(),
            total_keys = total,
            "jwks refreshed"
        );
        *self.keys.write().await = map;
        Ok(())
    }
}

/// Better Auth issues EdDSA; RS256/ES256 are kept for future key rotation.
fn header_alg_allowed(alg: Algorithm) -> bool {
    matches!(alg, Algorithm::EdDSA | Algorithm::RS256 | Algorithm::ES256)
}

/// Build validation for a single header algorithm that matches the key family.
///
/// `jsonwebtoken` 11 rejects the whole list when any allowed algorithm is in a
/// different family than the decoding key — so never mix EdDSA with RS/ES here.
fn validation_for(
    alg: Algorithm,
    key: &DecodingKey,
    issuer: Option<&str>,
    audience: Option<&str>,
) -> Result<Validation, String> {
    if !header_alg_allowed(alg) {
        return Err(format!("unsupported jwt alg {alg:?}"));
    }
    if key.family() != alg.family() {
        return Err(format!(
            "jwt alg {alg:?} does not match key family {:?}",
            key.family()
        ));
    }
    let mut validation = Validation::new(alg);
    validation.algorithms = vec![alg];
    validation.validate_exp = true;
    if let Some(iss) = issuer {
        validation.set_issuer(&[iss]);
    } else {
        validation.iss = None;
    }
    if let Some(aud) = audience {
        validation.set_audience(&[aud]);
    } else {
        validation.validate_aud = false;
    }
    Ok(validation)
}

fn decode_claims(
    token: &str,
    key: &DecodingKey,
    alg: Algorithm,
    issuer: Option<&str>,
    audience: Option<&str>,
) -> Result<JwtClaims, String> {
    let validation = validation_for(alg, key, issuer, audience)?;
    decode::<JwtClaims>(token, key, &validation)
        .map(|d| d.claims)
        .map_err(|e| e.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde::Serialize;

    // Fixed Ed25519 key pair from the jsonwebtoken 11 test suite.
    const ED_PRIV_PEM: &str = "-----BEGIN PRIVATE KEY-----\n\
MC4CAQAwBQYDK2VwBCIEIGrD/e7uKYqSY4twDEsRfMMuLSrODf14dpTiTK6K1YI0\n\
-----END PRIVATE KEY-----";
    const ED_PUB_PEM: &str = "-----BEGIN PUBLIC KEY-----\n\
MCowBQYDK2VwAyEA2+Jj2UvNCvQiUPNYRgSi0cJSPiJI6Rs6D0UTeEpQVj8=\n\
-----END PUBLIC KEY-----";

    #[derive(Serialize)]
    struct TestClaims {
        sub: String,
        email: String,
        exp: i64,
        iss: String,
        aud: String,
    }

    fn sign(claims: &TestClaims) -> String {
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = Some("test-kid".into());
        encode(
            &header,
            claims,
            &EncodingKey::from_ed_pem(ED_PRIV_PEM.as_bytes()).unwrap(),
        )
        .unwrap()
    }

    fn decoding_key() -> DecodingKey {
        DecodingKey::from_ed_pem(ED_PUB_PEM.as_bytes()).unwrap()
    }

    #[test]
    fn eddsa_token_verifies() {
        let now = chrono::Utc::now().timestamp();
        let token = sign(&TestClaims {
            sub: "user-1".into(),
            email: "a@b.co".into(),
            exp: now + 600,
            iss: "https://skywoop.com".into(),
            aud: "skywoop-api".into(),
        });
        let claims = decode_claims(
            &token,
            &decoding_key(),
            Algorithm::EdDSA,
            Some("https://skywoop.com"),
            Some("skywoop-api"),
        )
        .unwrap();
        assert_eq!(claims.sub, "user-1");
        assert_eq!(claims.email.as_deref(), Some("a@b.co"));
    }

    #[test]
    fn expired_token_rejected() {
        let now = chrono::Utc::now().timestamp();
        let token = sign(&TestClaims {
            sub: "user-1".into(),
            email: "a@b.co".into(),
            exp: now - 120,
            iss: "https://skywoop.com".into(),
            aud: "skywoop-api".into(),
        });
        let err = decode_claims(
            &token,
            &decoding_key(),
            Algorithm::EdDSA,
            Some("https://skywoop.com"),
            Some("skywoop-api"),
        )
        .unwrap_err();
        assert!(
            err.to_lowercase().contains("expired") || err.contains("ExpiredSignature"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn wrong_audience_rejected() {
        let now = chrono::Utc::now().timestamp();
        let token = sign(&TestClaims {
            sub: "user-1".into(),
            email: "a@b.co".into(),
            exp: now + 600,
            iss: "https://skywoop.com".into(),
            aud: "other-api".into(),
        });
        let err = decode_claims(
            &token,
            &decoding_key(),
            Algorithm::EdDSA,
            Some("https://skywoop.com"),
            Some("skywoop-api"),
        )
        .unwrap_err();
        assert!(
            err.to_lowercase().contains("audience") || err.contains("InvalidAudience"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn mixed_family_algorithms_would_fail_key_check() {
        // Documents the jsonwebtoken 11 pitfall we fixed: mixed families must
        // not appear in validation.algorithms when verifying with an Ed key.
        let key = decoding_key();
        assert!(
            validation_for(
                Algorithm::EdDSA,
                &key,
                Some("https://skywoop.com"),
                Some("skywoop-api"),
            )
            .is_ok()
        );
        assert!(
            validation_for(
                Algorithm::RS256,
                &key,
                Some("https://skywoop.com"),
                Some("skywoop-api"),
            )
            .is_err()
        );
    }
}
