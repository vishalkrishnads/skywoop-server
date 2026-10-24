use serde::de::Deserializer;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Generate a new UUIDv7 primary key.
pub fn new_id() -> Uuid {
    Uuid::now_v7()
}

/// Families the settings dropdown may select. Image prompts use the name as given.
pub const FONT_FAMILIES: &[&str] = &[
    "Anton",
    "Arial",
    "Bebas Neue",
    "Courier New",
    "DM Sans",
    "Fraunces",
    "Georgia",
    "Helvetica",
    "IBM Plex Sans",
    "IBM Plex Serif",
    "Inter",
    "Lato",
    "Libre Baskerville",
    "Manrope",
    "Merriweather",
    "Montserrat",
    "Nunito",
    "Open Sans",
    "Oswald",
    "Playfair Display",
    "Poppins",
    "Roboto",
    "Source Sans 3",
    "Space Grotesk",
    "Times New Roman",
];

#[derive(Debug, Clone, Default, Serialize, ToSchema, PartialEq)]
pub struct BrandProfile {
    pub primary_color: Option<String>,
    pub secondary_color: Option<String>,
    pub tertiary_color: Option<String>,
    pub font_family: Option<String>,
    pub logo_asset_id: Option<Uuid>,
    #[serde(default)]
    pub screenshot_asset_ids: Vec<Uuid>,
}

#[derive(Deserialize)]
struct BrandProfileIn {
    /// Legacy; dropped on deserialize.
    #[serde(default)]
    #[allow(dead_code)]
    themes: Vec<String>,
    #[serde(default)]
    accent_colors: Vec<String>,
    primary_color: Option<String>,
    secondary_color: Option<String>,
    tertiary_color: Option<String>,
    #[serde(default)]
    fonts: Vec<String>,
    font_family: Option<String>,
    /// Legacy; dropped on deserialize.
    #[serde(default)]
    #[allow(dead_code)]
    do_rules: Vec<String>,
    /// Legacy; dropped on deserialize.
    #[serde(default)]
    #[allow(dead_code)]
    dont_rules: Vec<String>,
    logo_asset_id: Option<Uuid>,
    #[serde(default)]
    screenshot_asset_ids: Vec<Uuid>,
}

impl<'de> Deserialize<'de> for BrandProfile {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = BrandProfileIn::deserialize(deserializer)?;
        let accent = |i: usize| raw.accent_colors.get(i).cloned();
        Ok(Self {
            primary_color: raw.primary_color.or_else(|| accent(0)),
            secondary_color: raw.secondary_color.or_else(|| accent(1)),
            tertiary_color: raw.tertiary_color.or_else(|| accent(2)),
            font_family: raw.font_family.or_else(|| raw.fonts.into_iter().next()),
            logo_asset_id: raw.logo_asset_id,
            screenshot_asset_ids: raw.screenshot_asset_ids,
        })
    }
}

/// Empty becomes `None`. Hex and `rgb`/`rgba` are stored as `#rrggbb` or `#rrggbbaa`.
pub fn normalize_color(raw: Option<String>) -> Result<Option<String>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let s = raw.trim();
    if s.is_empty() {
        return Ok(None);
    }
    parse_color(s).map(Some)
}

pub fn parse_color(raw: &str) -> Result<String, String> {
    let s = raw.trim();
    if let Some(hex) = s.strip_prefix('#') {
        return normalize_hex(hex);
    }
    let lower = s.to_ascii_lowercase();
    let inner = if let Some(rest) = lower.strip_prefix("rgba(") {
        rest
    } else if let Some(rest) = lower.strip_prefix("rgb(") {
        rest
    } else {
        return Err("use #rrggbb or rgb(r, g, b)".into());
    };
    let inner = inner
        .strip_suffix(')')
        .ok_or_else(|| "use #rrggbb or rgb(r, g, b)".to_string())?;
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
    if parts.len() != 3 && parts.len() != 4 {
        return Err("rgb needs 3 channels, rgba needs 4".into());
    }
    let mut channels = [0u8; 3];
    for (i, part) in parts.iter().take(3).enumerate() {
        let n: u16 = part
            .parse()
            .map_err(|_| format!("invalid channel {part}"))?;
        if n > 255 {
            return Err(format!("channel {part} is above 255"));
        }
        channels[i] = n as u8;
    }
    let alpha = if parts.len() == 4 {
        let a: f32 = parts[3]
            .parse()
            .map_err(|_| format!("invalid alpha {}", parts[3]))?;
        if !(0.0..=1.0).contains(&a) {
            return Err("alpha must be between 0 and 1".into());
        }
        Some((a * 255.0).round() as u8)
    } else {
        None
    };
    Ok(format_hex(&channels, alpha))
}

fn normalize_hex(hex: &str) -> Result<String, String> {
    let hex = hex.trim();
    let expanded = match hex.len() {
        3 | 4 => hex.chars().flat_map(|c| [c, c]).collect::<String>(),
        6 | 8 => hex.to_string(),
        _ => return Err("hex must be #rgb, #rgba, #rrggbb, or #rrggbbaa".into()),
    };
    if !expanded.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("hex contains a non-hex character".into());
    }
    let channels = [
        u8::from_str_radix(&expanded[0..2], 16).unwrap(),
        u8::from_str_radix(&expanded[2..4], 16).unwrap(),
        u8::from_str_radix(&expanded[4..6], 16).unwrap(),
    ];
    let alpha = if expanded.len() == 8 {
        Some(u8::from_str_radix(&expanded[6..8], 16).unwrap())
    } else {
        None
    };
    Ok(format_hex(&channels, alpha))
}

fn format_hex(channels: &[u8; 3], alpha: Option<u8>) -> String {
    match alpha {
        Some(a) if a < 255 => format!(
            "#{:02x}{:02x}{:02x}{:02x}",
            channels[0], channels[1], channels[2], a
        ),
        _ => format!("#{:02x}{:02x}{:02x}", channels[0], channels[1], channels[2]),
    }
}

pub fn normalize_font_family(raw: Option<String>) -> Result<Option<String>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let s = raw.trim();
    if s.is_empty() {
        return Ok(None);
    }
    FONT_FAMILIES
        .iter()
        .find(|name| name.eq_ignore_ascii_case(s))
        .map(|name| Some((*name).to_string()))
        .ok_or_else(|| format!("unknown font family: {s}"))
}

pub fn normalize_brand(brand: BrandProfile) -> Result<BrandProfile, String> {
    Ok(BrandProfile {
        primary_color: normalize_color(brand.primary_color)?,
        secondary_color: normalize_color(brand.secondary_color)?,
        tertiary_color: normalize_color(brand.tertiary_color)?,
        font_family: normalize_font_family(brand.font_family)?,
        ..brand
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Maturity {
    Collecting,
    Mature,
    Inconclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStatus {
    Connected,
    NeedsReauth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
}

impl Maturity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Collecting => "collecting",
            Self::Mature => "mature",
            Self::Inconclusive => "inconclusive",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_normalize_to_hex() {
        assert_eq!(parse_color("#abc").unwrap(), "#aabbcc");
        assert_eq!(parse_color("#AbCDef").unwrap(), "#abcdef");
        assert_eq!(parse_color("rgb(255, 128, 0)").unwrap(), "#ff8000");
        assert_eq!(parse_color("rgba(0, 0, 0, 0.5)").unwrap(), "#00000080");
        assert_eq!(parse_color("#ffffffff").unwrap(), "#ffffff");
        assert!(parse_color("blue").is_err());
    }

    #[test]
    fn old_brand_json_maps_onto_the_new_fields() {
        let brand: BrandProfile = serde_json::from_str(
            r##"{"accent_colors":["#111","#222","#333"],"fonts":["Inter"],"tone":"warm"}"##,
        )
        .unwrap();
        assert_eq!(brand.primary_color.as_deref(), Some("#111"));
        assert_eq!(brand.secondary_color.as_deref(), Some("#222"));
        assert_eq!(brand.tertiary_color.as_deref(), Some("#333"));
        assert_eq!(brand.font_family.as_deref(), Some("Inter"));
        let saved = serde_json::to_value(&brand).unwrap();
        assert!(saved.get("tone").is_none());
        assert!(saved.get("accent_colors").is_none());
    }

    #[test]
    fn font_family_must_be_in_the_list() {
        assert_eq!(
            normalize_font_family(Some("inter".into()))
                .unwrap()
                .as_deref(),
            Some("Inter")
        );
        assert!(normalize_font_family(Some("Comic Sans".into())).is_err());
    }
}
