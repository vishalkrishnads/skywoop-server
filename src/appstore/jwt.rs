use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::Serialize;

#[derive(Serialize)]
struct AscClaims<'a> {
    iss: &'a str,
    iat: i64,
    exp: i64,
    aud: &'static str,
}

pub fn asc_jwt(key_id: &str, issuer_id: &str, p8_pem: &[u8], now: i64) -> anyhow::Result<String> {
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some(key_id.to_owned());
    header.typ = Some("JWT".to_owned());
    let claims = AscClaims {
        iss: issuer_id,
        iat: now,
        exp: now + 19 * 60,
        aud: "appstoreconnect-v1",
    };
    Ok(encode(
        &header,
        &claims,
        &EncodingKey::from_ec_pem(p8_pem)?,
    )?)
}
