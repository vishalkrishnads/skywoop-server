//! Shared onboarding helpers (URL parsing, slugify, step transitions).

use regex::Regex;
use std::sync::LazyLock;
use url::Url;

static APP_ID_IN_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/id(\d+)").expect("app id in url regex"));

/// Parse an App Store listing URL into `(app_id, optional country code)`.
pub fn app_store_url_parts(url: &str) -> Option<(String, Option<String>)> {
    let parsed = Url::parse(url).ok()?;
    if parsed.host_str()? != "apps.apple.com" {
        return None;
    }
    let id = APP_ID_IN_URL.captures(parsed.path())?[1].to_string();
    let country = parsed
        .path_segments()
        .and_then(|mut s| s.next())
        .filter(|c| c.len() == 2 && c.bytes().all(|b| b.is_ascii_alphabetic()))
        .map(|c| c.to_ascii_uppercase());
    Some((id, country))
}

/// Slug from a display name: lowercase, hyphenated, ASCII-ish, max 48 chars.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut prev_hyphen = true;
    for c in name.chars() {
        let mapped = if c.is_ascii_alphanumeric() {
            Some(c.to_ascii_lowercase())
        } else if c.is_whitespace() || c == '-' || c == '_' {
            Some('-')
        } else {
            None
        };
        if let Some(ch) = mapped {
            if ch == '-' {
                if prev_hyphen {
                    continue;
                }
                prev_hyphen = true;
            } else {
                prev_hyphen = false;
            }
            out.push(ch);
            if out.len() >= 48 {
                break;
            }
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() { "app".into() } else { out }
}

/// Map vision category labels to `Context/<Dir>` directory names.
pub fn context_dir_for_category(category: &str) -> &'static str {
    match category {
        "Brand" => "Context/Brand",
        "Product/Screenshots" => "Context/Product/Screenshots",
        "Product/Screen recordings" => "Context/Product/Screen recordings",
        "People" => "Context/People",
        "Lifestyle" => "Context/Lifestyle",
        "Testimonials" => "Context/Testimonials",
        "Press" => "Context/Press",
        "Videos" => "Context/Videos",
        _ => "Context/Other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_app_store_url() {
        let (id, country) =
            app_store_url_parts("https://apps.apple.com/us/app/test/id1234567890").unwrap();
        assert_eq!(id, "1234567890");
        assert_eq!(country.as_deref(), Some("US"));
    }

    #[test]
    fn slugifies_names() {
        assert_eq!(slugify("My Cool App!"), "my-cool-app");
        assert_eq!(slugify("  ---  "), "app");
    }
}
