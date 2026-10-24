use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::client::{AiClient, AiError, Effort, TextRole};
use super::context::KnowledgePack;
use super::prompt::{self, data_json};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SuggestedAudienceOut {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProductDiscoveryOut {
    pub name: String,
    pub description: String,
    pub primary_color: Option<String>,
    pub secondary_color: Option<String>,
    pub tertiary_color: Option<String>,
    pub font_family: Option<String>,
    pub app_store_app_id: Option<String>,
    #[serde(default)]
    pub suggested_audiences: Vec<SuggestedAudienceOut>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SocialTrendItemOut {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub source_urls: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SocialTrendsOut {
    pub items: Vec<SocialTrendItemOut>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IndustryClassificationOut {
    /// snake_case industry id from the catalog.
    pub industry: String,
    pub confidence: f64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AdjustPostTimingOut {
    /// "keep" or "update"
    pub action: String,
    /// Legacy point slots (optional; derived from prime window midpoints when omitted).
    #[serde(default)]
    pub slots: Vec<TimingSlotOut>,
    /// Preferred: per-weekday prime windows for rank-1 posts.
    #[serde(default)]
    pub prime_windows: Vec<TimingWindowOut>,
    #[serde(default)]
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TimingSlotOut {
    /// 0=Mon … 6=Sun
    pub weekday: u8,
    /// Minutes since local midnight.
    pub minute: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TimingWindowOut {
    /// 0=Mon … 6=Sun
    pub weekday: u8,
    pub start_minute: u16,
    pub end_minute: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExperimentAnalysisOut {
    pub observation: String,
    /// Generalizable creative patterns worth proposing for the central playbook (usually empty).
    #[serde(default)]
    pub playbook_candidates: Vec<PlaybookCandidateOut>,
}

/// A creative pattern the agent believes generalizes beyond this project.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlaybookCandidateOut {
    pub title: String,
    /// Markdown body: what to do, why it works, constraints.
    pub body_md: String,
    /// App Store style categories this applies to (lowercase), e.g. `productivity`.
    #[serde(default)]
    pub app_categories: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Short evidence summary with numbers copied from the data.
    #[serde(default)]
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlatformAnalysisOut {
    pub observation: String,
}

fn pack_ctx(pack: &KnowledgePack) -> Result<String, AiError> {
    data_json("knowledge", pack)
}

pub async fn discover_product(
    ai: &AiClient,
    site_summary: &Value,
    app_store: Option<&Value>,
    palette: &[String],
) -> Result<ProductDiscoveryOut, AiError> {
    let system = "You extract a Skywoop project draft from crawled product data. \
Prefer App Store name and description when present. Colors must be #rrggbb hex. \
font_family must be one of the known web-safe/display families or null. \
app_store_app_id is the numeric App Store id when known.";
    let prompt = super::client::RenderedPrompt {
        system: system.into(),
        user: format!(
            "Build a product draft.\n\n<data name=\"site\">\n{}\n</data>\n\n\
<data name=\"app_store\">\n{}\n</data>\n\n\
<data name=\"palette\">\n{}\n</data>",
            serde_json::to_string_pretty(site_summary).map_err(|e| AiError::Other(e.into()))?,
            app_store
                .map(serde_json::to_string_pretty)
                .transpose()
                .map_err(|e| AiError::Other(e.into()))?
                .unwrap_or_else(|| "{}".into()),
            serde_json::to_string_pretty(palette).map_err(|e| AiError::Other(e.into()))?,
        ),
        version: "discover_product:1".into(),
    };
    ai.structured(
        TextRole::Planner,
        "discover_product",
        &prompt,
        &[],
        Effort::Medium,
    )
    .await
}

pub async fn social_trends_via_web(
    ai: &AiClient,
    country: &str,
    platform: &str,
) -> Result<SocialTrendsOut, AiError> {
    let system = "You research what is trending today for social feed posts. \
Return 5-10 short items relevant to Instagram/Facebook organic content in the given country. \
Skip tragedy, death, disaster, politics, crime, and health crises. \
Each item needs a short id (snake_case), title, summary, and source_urls.";
    let prompt = super::client::RenderedPrompt {
        system: system.into(),
        user: format!(
            "What cultural, meme, format, and event topics are trending today for {platform} \
posts in country {country}? Prefer items a consumer app could hitch onto."
        ),
        version: "social_trends:1".into(),
    };
    ai.structured_with_tools(
        TextRole::Planner,
        "social_trends",
        &prompt,
        &[],
        Effort::Low,
        &[json!({ "type": "web_search" })],
        Some(json!({ "country": country })),
    )
    .await
}

pub async fn analyze_experiment(
    ai: &AiClient,
    pack: &KnowledgePack,
    experiment: &Value,
    video: &Value,
) -> Result<ExperimentAnalysisOut, AiError> {
    let prompt = prompt::render(
        "analyze_experiment",
        &json!({
            "knowledge": pack_ctx(pack)?,
            "experiment": serde_json::to_string_pretty(experiment)
                .map_err(|e| AiError::Other(e.into()))?,
            "video": serde_json::to_string_pretty(video)
                .map_err(|e| AiError::Other(e.into()))?,
        }),
    )?;
    ai.structured(
        TextRole::Planner,
        "analyze_experiment",
        &prompt,
        &[],
        Effort::Medium,
    )
    .await
}

pub async fn analyze_platform(
    ai: &AiClient,
    pack: &KnowledgePack,
    recent_experiments: &Value,
    other_platform_medians: &Value,
) -> Result<PlatformAnalysisOut, AiError> {
    let prompt = prompt::render(
        "analyze_platform",
        &json!({
            "knowledge": pack_ctx(pack)?,
            "recent_experiments": serde_json::to_string_pretty(recent_experiments)
                .map_err(|e| AiError::Other(e.into()))?,
            "other_platform_medians": serde_json::to_string_pretty(other_platform_medians)
                .map_err(|e| AiError::Other(e.into()))?,
        }),
    )?;
    ai.structured(
        TextRole::Planner,
        "analyze_platform",
        &prompt,
        &[],
        Effort::Low,
    )
    .await
}

pub async fn classify_industry(
    ai: &AiClient,
    project: &Value,
    industries: &Value,
) -> Result<IndustryClassificationOut, AiError> {
    let prompt = prompt::render(
        "classify_industry",
        &json!({
            "project": serde_json::to_string_pretty(project)
                .map_err(|e| AiError::Other(e.into()))?,
            "industries": serde_json::to_string_pretty(industries)
                .map_err(|e| AiError::Other(e.into()))?,
        }),
    )?;
    ai.structured(
        TextRole::Copy,
        "classify_industry",
        &prompt,
        &[],
        Effort::Low,
    )
    .await
}

pub async fn adjust_post_timing(
    ai: &AiClient,
    pack: &KnowledgePack,
    current_schedule: &Value,
    industry_prior: &Value,
    online_followers_by_hour: &Value,
    post_slot_performance: &Value,
    max_daily_posts: i32,
) -> Result<AdjustPostTimingOut, AiError> {
    let prompt = prompt::render(
        "adjust_post_timing",
        &json!({
            "knowledge": pack_ctx(pack)?,
            "current_schedule": serde_json::to_string_pretty(current_schedule)
                .map_err(|e| AiError::Other(e.into()))?,
            "industry_prior": serde_json::to_string_pretty(industry_prior)
                .map_err(|e| AiError::Other(e.into()))?,
            "online_followers_by_hour": serde_json::to_string_pretty(online_followers_by_hour)
                .map_err(|e| AiError::Other(e.into()))?,
            "post_slot_performance": serde_json::to_string_pretty(post_slot_performance)
                .map_err(|e| AiError::Other(e.into()))?,
            "max_daily_posts": max_daily_posts,
        }),
    )?;
    ai.structured(
        TextRole::Planner,
        "adjust_post_timing",
        &prompt,
        &[],
        Effort::Low,
    )
    .await
}
