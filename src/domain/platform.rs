use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Instagram,
    Facebook,
}

impl Platform {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Instagram => "instagram",
            Self::Facebook => "facebook",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Self::Instagram => "ig",
            Self::Facebook => "fb",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "instagram" => Some(Self::Instagram),
            "facebook" => Some(Self::Facebook),
            _ => None,
        }
    }
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
