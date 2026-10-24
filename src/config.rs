use std::env;
use std::path::PathBuf;

use anyhow::{Context, bail};
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppEnv {
    Local,
    Azure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbAuth {
    Password,
    Entra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    /// Fixed bearer token → synthetic `dev-user` (local + CI).
    DevToken,
    /// Better Auth JWT verified against `AUTH_JWKS_URL`.
    Jwt,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub app_env: AppEnv,
    pub public_base_url: Url,
    pub fallback_url: Url,
    pub database_url: String,
    pub db_auth: DbAuth,
    pub azure_client_id: Option<String>,
    pub auth_mode: AuthMode,
    pub dev_token: String,
    /// Better Auth JWKS endpoint (required when `auth_mode = Jwt`).
    pub auth_jwks_url: Option<Url>,
    pub auth_issuer: Option<String>,
    pub auth_audience: Option<String>,
    /// First `APP_ORIGIN` entry: the frontend used for OAuth done redirects.
    pub app_origin: Url,
    /// Every `APP_ORIGIN` entry (comma-separated), without trailing slashes, for CORS.
    pub app_origins: Vec<String>,
    pub azure_openai_base: Url,
    pub azure_openai_images_api_version: Option<String>,
    /// Optional local Bearer key when managed identity is unavailable.
    pub azure_openai_api_key: Option<String>,
    pub deployment_planner: String,
    pub deployment_vision: String,
    pub deployment_copy: String,
    pub deployment_image_draft: String,
    pub deployment_image_final: String,
    pub image_rpm: u32,
    /// Directory of Apple-marketing iPhone bezel PNGs (gitignored; operator-supplied).
    pub local_asset_dir: PathBuf,
    pub local_secrets_file: PathBuf,
    pub blob_account: Option<String>,
    pub blob_container: String,
    pub keyvault_url: Option<String>,
    pub meta_app_id: String,
    pub meta_app_secret_name: String,
    pub meta_graph_version: String,
    pub meta_graph_base: Url,
    pub asc_issuer_id: String,
    pub asc_key_id: String,
    pub asc_p8_secret_name: String,
    pub asc_base: Url,
    pub link_hash_secret_name: String,
    pub oauth_done_url: Url,
    pub listen_addr: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        // `.env` wins over variables already exported in the shell. `dotenv()`
        // leaves those in place, so a stale `AZURE_OPENAI_API_KEY` kept being
        // sent after the file was updated. No `.env` is shipped in the image,
        // so Azure env vars are unchanged.
        let _ = dotenvy::dotenv_override();

        let app_env = match env_or("APP_ENV", "local")?.as_str() {
            "local" => AppEnv::Local,
            "azure" => AppEnv::Azure,
            other => bail!("APP_ENV must be local|azure, got {other}"),
        };
        let db_auth = match env_or("DB_AUTH", "password")?.as_str() {
            "password" => DbAuth::Password,
            "entra" => DbAuth::Entra,
            other => bail!("DB_AUTH must be password|entra, got {other}"),
        };
        let auth_mode = match env_or("AUTH_MODE", "dev_token")?.as_str() {
            "dev_token" => AuthMode::DevToken,
            "jwt" => AuthMode::Jwt,
            other => bail!("AUTH_MODE must be dev_token|jwt, got {other}"),
        };

        let app_origins = parse_origins(&env_or("APP_ORIGIN", "http://localhost:3000")?)?;
        let app_origin = Url::parse(&app_origins[0]).context("APP_ORIGIN is not a valid URL")?;
        let oauth_done_default = format!(
            "{}/onboarding/meta/done",
            app_origin.as_str().trim_end_matches('/')
        );

        let auth_jwks_url = env::var("AUTH_JWKS_URL")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|s| Url::parse(&s).context("AUTH_JWKS_URL is not a valid URL"))
            .transpose()?;

        if auth_mode == AuthMode::Jwt && auth_jwks_url.is_none() {
            bail!("AUTH_JWKS_URL is required when AUTH_MODE=jwt");
        }

        Ok(Self {
            app_env,
            public_base_url: env_url("PUBLIC_BASE_URL", "http://localhost:8080")?,
            fallback_url: env_url("FALLBACK_URL", "https://apps.apple.com")?,
            database_url: env_or(
                "DATABASE_URL",
                "postgres://skywoop:skywoop@localhost:5432/skywoop",
            )?,
            db_auth,
            azure_client_id: env::var("AZURE_CLIENT_ID").ok().filter(|s| !s.is_empty()),
            auth_mode,
            dev_token: env_or("DEV_TOKEN", "dev-secret-token-change-me")?,
            auth_jwks_url,
            auth_issuer: env::var("AUTH_ISSUER").ok().filter(|s| !s.is_empty()),
            auth_audience: env::var("AUTH_AUDIENCE").ok().filter(|s| !s.is_empty()),
            app_origin,
            app_origins,
            azure_openai_base: env_url(
                "AZURE_OPENAI_BASE",
                "https://example.openai.azure.com/openai/v1",
            )?,
            azure_openai_images_api_version: env::var("AZURE_OPENAI_IMAGES_API_VERSION")
                .ok()
                .filter(|s| !s.is_empty()),
            azure_openai_api_key: env::var("AZURE_OPENAI_API_KEY")
                .ok()
                .filter(|s| !s.is_empty()),
            deployment_planner: env_or("DEPLOYMENT_PLANNER", "gpt-6.1-sol")?,
            deployment_vision: env_or("DEPLOYMENT_VISION", "gpt-6.1-sol")?,
            deployment_copy: env_or("DEPLOYMENT_COPY", "gpt-6-luna")?,
            deployment_image_draft: env_or("DEPLOYMENT_IMAGE_DRAFT", "gpt-image-2.5-flare")?,
            deployment_image_final: env_or("DEPLOYMENT_IMAGE_FINAL", "gpt-image-2.5-sunburst")?,
            image_rpm: env_or("IMAGE_RPM", "5")?
                .parse()
                .context("IMAGE_RPM must be a u32")?,
            local_asset_dir: PathBuf::from(env_or("LOCAL_ASSET_DIR", "./.data/assets")?),
            local_secrets_file: PathBuf::from(env_or(
                "LOCAL_SECRETS_FILE",
                "./.data/secrets.json",
            )?),
            blob_account: env::var("BLOB_ACCOUNT").ok().filter(|s| !s.is_empty()),
            blob_container: env_or("BLOB_CONTAINER", "assets")?,
            keyvault_url: env::var("KEYVAULT_URL").ok().filter(|s| !s.is_empty()),
            meta_app_id: env_or("META_APP_ID", "")?,
            meta_app_secret_name: env_or("META_APP_SECRET_NAME", "meta-app-secret")?,
            meta_graph_version: env_or("META_GRAPH_VERSION", "v26.0")?,
            meta_graph_base: env_url("META_GRAPH_BASE", "https://graph.facebook.com")?,
            asc_issuer_id: env_or("ASC_ISSUER_ID", "")?,
            asc_key_id: env_or("ASC_KEY_ID", "")?,
            asc_p8_secret_name: env_or("ASC_P8_SECRET_NAME", "asc-p8")?,
            asc_base: env_url("ASC_BASE", "https://api.appstoreconnect.apple.com")?,
            link_hash_secret_name: env_or("LINK_HASH_SECRET_NAME", "link-hash-secret")?,
            oauth_done_url: env_url("OAUTH_DONE_URL", &oauth_done_default)?,
            listen_addr: env_or("LISTEN_ADDR", "0.0.0.0:8080")?,
        })
    }

    pub fn is_local(&self) -> bool {
        self.app_env == AppEnv::Local
    }
}

fn env_or(key: &str, default: &str) -> anyhow::Result<String> {
    match env::var(key) {
        Ok(v) if !v.is_empty() => Ok(v),
        _ => Ok(default.to_owned()),
    }
}

fn env_url(key: &str, default: &str) -> anyhow::Result<Url> {
    let raw = env_or(key, default)?;
    Url::parse(&raw).with_context(|| format!("{key} is not a valid URL: {raw}"))
}

/// Comma-separated origins, e.g. `https://app.skywoop.com,http://localhost:3000`.
fn parse_origins(raw: &str) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    for part in raw.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        Url::parse(part).with_context(|| format!("APP_ORIGIN entry is not a valid URL: {part}"))?;
        out.push(part.trim_end_matches('/').to_owned());
    }
    if out.is_empty() {
        bail!("APP_ORIGIN must list at least one origin");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_comma_separated_origins() {
        assert_eq!(
            parse_origins("https://app.skywoop.com/, http://localhost:3000").unwrap(),
            vec!["https://app.skywoop.com", "http://localhost:3000"]
        );
        assert_eq!(
            parse_origins("http://localhost:3000").unwrap(),
            vec!["http://localhost:3000"]
        );
        assert!(parse_origins(" , ").is_err());
        assert!(parse_origins("https://ok.example,not a url").is_err());
    }
}
