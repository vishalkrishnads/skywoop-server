use rand::Rng;

use crate::domain::Platform;

const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789"; // no 0/o/1/l

/// Generate a short opaque campaign token (≤ 30 chars), e.g. `haserupt-ig-k7m2qx9a`.
pub fn campaign_token(slug: &str, platform: Platform) -> String {
    let prefix: String = slug
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(10)
        .collect();
    let mut rng = rand::rng();
    let id: String = (0..8)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect();
    format!("{prefix}-{}-{id}", platform.short())
}

/// Backward-compatible alias used by older call sites.
pub fn new_campaign_token() -> String {
    campaign_token("camp", Platform::Instagram)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_shape_and_length() {
        let t = campaign_token("haserupt!", Platform::Instagram);
        assert!(t.starts_with("haserupt-ig-"));
        assert!(t.len() <= 30);
        assert_eq!(t.len(), "haserupt-ig-".len() + 8);
    }
}
