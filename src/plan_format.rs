//! Markdown plan format shared by PlanDay (agent output) and `PUT /plans/{id}`.
//!
//! A plan is a markdown document. The `#` title is the day's title and the paragraph
//! under it is the summary (max 280 characters). Every `##` heading carries a stable anchor:
//!
//! ```text
//! # Tactile ergonomics vs digital clutter
//! Contrast timer fatigue with the rotary dial.
//!
//! ## Video 1: Girl studying vs scrolling {#video-1}
//! - track: audience
//! - audience: 2
//! - daypart: prime
//! - caption: I couldn't get off Instagram even for exams
//! - hypothesis: Before/after screen time converts better than a generic demo.
//!
//! Concept paragraph(s)…
//!
//! ```flow
//! - 0:00-0:03 | Hook | Girl scrolling at her desk
//! - 0:03-0:09 | Reveal | Screen time drops to 35 minutes
//! ```
//!
//! ```assets
//! - [x] image: app icon @"App Store/app-icon.png"
//!   - name: App icon
//! - [ ] video: girl at desk scrolling, one static shot
//!   - name: Girl scrolling
//!   - reuse: 40
//!   - model: kling-2.5-turbo-pro
//! ```
//! ```
//!
//! Legacy `Hook[Girl scrolling] --> Reveal[App demo]` flows still parse (without timings).
//! Video sections have anchors starting with `video-`. Other `##` sections (strategy,
//! findings, …) are optional and free-form but must carry an anchor so comments can attach.
//! Checklist markers: `[ ]` open, `[~]` pending generation, `[x]` done; see
//! [`ChecklistStatus`] for the derived status.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

use crate::domain::plan::{Daypart, PlanTrack};

static HEADING_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(#{1,6})\s+(.*?)\s*(?:\{#([A-Za-z0-9][A-Za-z0-9_-]{0,63})\})?\s*$")
        .expect("heading regex")
});
static ANCHOR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$").expect("anchor regex"));
static META_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*[-*]\s*(track|audience|trend|daypart|caption|hypothesis)\s*:\s*(.*?)\s*$")
        .expect("meta regex")
});
static CHECK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(\s*[-*]\s*)\[( |~|x)\](\s*)(image|video|audio)\s*:\s*(.+?)\s*$")
        .expect("checklist regex")
});
/// Indented directive under a checklist item: `  - mode: reference` or `  - reuse: 35`.
static DIR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(\s{2,}|\t+)\s*[-*]\s*(mode|duration|input|reuse|name|model)\s*:\s*(.+?)\s*$")
        .expect("directive regex")
});
/// Timed flow step: `- 0:00-0:03 | Hook | Loud beeps and blinking counters`.
static FLOW_STEP_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\s*(?:[-*]\s*)?(\d{1,2}):(\d{2})\s*-\s*(\d{1,2}):(\d{2})\s*\|\s*([^|]*?)\s*(?:\|\s*(.*?))?\s*$",
    )
    .expect("flow step regex")
});
static MODEL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9._/-]{1,80}$").expect("model regex"));
/// Note inside `<!-- REGENERATE: … -->`.
static REGEN_NOTE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)<!--\s*REGENERATE:\s*([\s\S]*?)\s*-->").expect("regenerate note regex")
});

pub const MAX_SUMMARY_CHARS: usize = 280;
pub const MAX_CHECKLIST_NAME_CHARS: usize = 60;
const FALLBACK_NAME_WORDS: usize = 6;
const MAX_REGENERATE_NOTE_CHARS: usize = 500;
static REF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"@"([^"]+)"|@([^\s"',;)\]]+)"#).expect("ref regex"));
/// Checklist key like `video-1#2`.
static CHECKLIST_KEY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9][A-Za-z0-9_-]{0,63}#\d+$").expect("checklist key regex")
});
/// `<!-- GENERATE -->` and `<!-- REGENERATE: … -->` marks on a checklist line.
static GEN_MARK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\s*<!--\s*(?:GENERATE\s*|REGENERATE:[\s\S]*?)-->").expect("gen mark regex")
});
/// Flags a `video:` checklist description that asks one generation for multiple
/// shots or for transparency the model cannot produce.
///
/// A bare semicolon is allowed: one continuous shot often uses it as punctuation
/// ("hands at the counter; small hesitation"). Joining words are the signal.
static MULTI_SHOT_VIDEO_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:\splus\s|\sthen\s|\scut\sto\s|\btransparent\b|\balpha\b|\bgreen\sscreen\b)")
        .expect("multi-shot video regex")
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChecklistState {
    Open,
    Pending,
    Done,
}

impl ChecklistState {
    pub fn marker(self) -> char {
        match self {
            Self::Open => ' ',
            Self::Pending => '~',
            Self::Done => 'x',
        }
    }

    pub fn from_marker(s: &str) -> Option<Self> {
        match s {
            " " => Some(Self::Open),
            "~" => Some(Self::Pending),
            "x" | "X" => Some(Self::Done),
            _ => None,
        }
    }

    /// Accepts the marker names and the matching [`ChecklistStatus`] names.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Self::Open),
            "pending" | "queued" | "generated" | "regenerate_queued" => Some(Self::Pending),
            "done" | "confirmed" => Some(Self::Done),
            _ => None,
        }
    }
}

/// Status derived from the checklist marker, the attached `@path`, and a REGENERATE mark.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChecklistStatus {
    /// `[ ]`
    #[default]
    Open,
    /// `[~]` without an attached asset: waiting for generation.
    Queued,
    /// `[~]` with an attached asset: waiting for the operator to confirm.
    Generated,
    /// `[~]` with an attached asset and a `<!-- REGENERATE: … -->` note.
    RegenerateQueued,
    /// `[x]`
    Confirmed,
}

impl ChecklistStatus {
    pub fn derive(state: ChecklistState, has_ref: bool, regenerate: bool) -> Self {
        match state {
            ChecklistState::Open => Self::Open,
            ChecklistState::Done => Self::Confirmed,
            ChecklistState::Pending if !has_ref => Self::Queued,
            ChecklistState::Pending if regenerate => Self::RegenerateQueued,
            ChecklistState::Pending => Self::Generated,
        }
    }
}

/// One step of a video's ```flow block.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct FlowStep {
    pub label: String,
    pub description: String,
    /// Absent for legacy `A[x] --> B[y]` flows.
    pub start_ms: Option<u32>,
    pub end_ms: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChecklistKind {
    Image,
    Video,
    Audio,
}

impl ChecklistKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "image" => Some(Self::Image),
            "video" => Some(Self::Video),
            "audio" => Some(Self::Audio),
            _ => None,
        }
    }
}

/// How a video checklist item should be generated on fal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum VideoMode {
    /// Plain text-to-video (default when no mode is declared).
    Text,
    /// Animate from a start frame image.
    StartFrame,
    /// Animate between a start and end frame.
    FirstLastFrame,
    /// Multimodal references (images / videos / audio).
    Reference,
    /// Continue / extend an existing video.
    Extend,
}

impl VideoMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::StartFrame => "start_frame",
            Self::FirstLastFrame => "first_last_frame",
            Self::Reference => "reference",
            Self::Extend => "extend",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "text" | "text-to-video" | "t2v" => Some(Self::Text),
            "start_frame" | "start-frame" | "image-to-video" | "i2v" => Some(Self::StartFrame),
            "first_last_frame" | "first-last-frame" | "flf" => Some(Self::FirstLastFrame),
            "reference" | "reference-to-video" | "r2v" => Some(Self::Reference),
            "extend" | "extension" | "video-to-video" | "v2v" => Some(Self::Extend),
            _ => None,
        }
    }
}

/// Role a required input plays for a video generation call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum VideoInputRole {
    StartFrame,
    EndFrame,
    ReferenceImage,
    ReferenceVideo,
    ReferenceAudio,
    SourceVideo,
}

impl VideoInputRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StartFrame => "start_frame",
            Self::EndFrame => "end_frame",
            Self::ReferenceImage => "reference_image",
            Self::ReferenceVideo => "reference_video",
            Self::ReferenceAudio => "reference_audio",
            Self::SourceVideo => "source_video",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "start_frame" | "start-frame" | "image" | "image_url" => Some(Self::StartFrame),
            "end_frame" | "end-frame" | "end_image" | "end_image_url" => Some(Self::EndFrame),
            "reference_image" | "reference-image" | "ref_image" => Some(Self::ReferenceImage),
            "reference_video" | "reference-video" | "ref_video" => Some(Self::ReferenceVideo),
            "reference_audio" | "reference-audio" | "ref_audio" => Some(Self::ReferenceAudio),
            "source_video" | "source-video" | "video" | "video_url" | "extend_video" => {
                Some(Self::SourceVideo)
            }
            _ => None,
        }
    }

    /// Expected media kind for this input role.
    pub fn expected_kind(self) -> ChecklistKind {
        match self {
            Self::StartFrame | Self::EndFrame | Self::ReferenceImage => ChecklistKind::Image,
            Self::ReferenceVideo | Self::SourceVideo => ChecklistKind::Video,
            Self::ReferenceAudio => ChecklistKind::Audio,
        }
    }
}

/// Where a required generation input comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VideoInputTarget {
    /// Another checklist item in this plan, e.g. `video-1#1`.
    ChecklistKey { key: String },
    /// An existing asset-manager path (without leading `@`).
    AssetPath { path: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct VideoInput {
    pub role: VideoInputRole,
    pub target: VideoInputTarget,
    /// 0-based markdown line of the directive.
    pub line: usize,
}

/// Machine-readable generation controls attached to a `video:` checklist item.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct VideoGenerationSpec {
    /// Defaults to [`VideoMode::Text`] when omitted from the plan.
    pub mode: VideoMode,
    /// Requested clip length in seconds when declared.
    pub duration: Option<u32>,
    pub inputs: Vec<VideoInput>,
}

impl Default for VideoGenerationSpec {
    fn default() -> Self {
        Self {
            mode: VideoMode::Text,
            duration: None,
            inputs: Vec::new(),
        }
    }
}

/// Why a video checklist item cannot be generated yet.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct GenerationBlocker {
    pub role: String,
    /// checklist_key | asset_path
    pub target_kind: String,
    pub target: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChecklistItem {
    /// Stable key `{anchor}#{n}` (1-based index within the video's `assets` block).
    pub key: String,
    pub state: ChecklistState,
    pub kind: ChecklistKind,
    pub text: String,
    /// `@path` references found in `text` (without the `@`). Output path when done.
    pub refs: Vec<String>,
    /// 0-based markdown line index of the checklist item itself.
    pub line: usize,
    /// Declared generation controls (video items only; empty/default for others).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<VideoGenerationSpec>,
    /// True when every declared input is confirmed / present.
    #[serde(default)]
    pub generation_ready: bool,
    /// Human-readable blockers when `generation_ready` is false.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub generation_blockers: Vec<GenerationBlocker>,
    /// 0–100 how reusable this open asset is across other ideas. Absent on confirmed lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reuse: Option<u8>,
    /// Short label from `- name:`; older lines fall back to the first words of `text`.
    #[serde(default)]
    pub name: String,
    /// True when the line declares `- name:` itself.
    #[serde(skip)]
    pub name_declared: bool,
    /// Generation model id from `- model:`.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub status: ChecklistStatus,
    /// Text of a `<!-- REGENERATE: … -->` mark on the line.
    #[serde(default)]
    pub regenerate_note: Option<String>,
    /// Signed read URL for the first `@path` (filled in by the API).
    #[serde(default)]
    pub preview_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlanIssue {
    /// 1-based line number when known.
    pub line: Option<usize>,
    pub anchor: Option<String>,
    pub message: String,
}

impl std::fmt::Display for PlanIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.anchor, self.line) {
            (Some(a), Some(l)) => write!(f, "[{a}, line {l}] {}", self.message),
            (Some(a), None) => write!(f, "[{a}] {}", self.message),
            (None, Some(l)) => write!(f, "[line {l}] {}", self.message),
            (None, None) => write!(f, "{}", self.message),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ParsedSection {
    pub anchor: Option<String>,
    pub heading: String,
    pub level: u8,
    /// 0-based, inclusive.
    pub start_line: usize,
    /// 0-based, exclusive.
    pub end_line: usize,
    pub is_video: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ParsedVideo {
    pub anchor: String,
    pub title: String,
    pub position: i32,
    pub track: Option<PlanTrack>,
    /// Raw `audience:` value: uuid, 1-based position, or audience name.
    pub audience: Option<String>,
    /// Raw `trend:` value: trend id or title.
    pub trend: Option<String>,
    pub daypart: Option<Daypart>,
    pub caption: String,
    pub hypothesis: String,
    /// Raw text inside the ```flow block.
    pub flow: String,
    pub flow_steps: Vec<FlowStep>,
    pub checklist: Vec<ChecklistItem>,
}

#[derive(Debug, Clone, Default)]
pub struct ParsedPlan {
    /// Text of the first `#` heading (the day's title).
    pub title: Option<String>,
    /// Paragraph between the `#` title and the first other heading, joined onto one line.
    pub summary: String,
    pub sections: Vec<ParsedSection>,
    pub videos: Vec<ParsedVideo>,
    /// Structural problems found while parsing.
    pub issues: Vec<PlanIssue>,
}

#[derive(Debug, Clone)]
pub struct ValidateOpts {
    /// Exact number of audience-track videos (PlanDay), when known.
    pub audience_videos: Option<usize>,
    /// Upper bound for trend-track videos (PlanDay), when known.
    pub max_trend_videos: Option<usize>,
    /// Require each video to have a non-empty caption.
    pub require_caption: bool,
    /// Require a `#` title with a summary paragraph (PlanDay output).
    pub require_title: bool,
    /// Require `- name:` on every checklist line (PlanDay output).
    pub require_names: bool,
    pub max_videos: usize,
}

impl Default for ValidateOpts {
    fn default() -> Self {
        Self {
            audience_videos: None,
            max_trend_videos: None,
            require_caption: false,
            require_title: false,
            require_names: false,
            max_videos: 40,
        }
    }
}

/// `true` when the anchor designates a video section.
pub fn is_video_anchor(anchor: &str) -> bool {
    anchor.starts_with("video-")
}

fn fence_info(line: &str) -> Option<String> {
    let t = line.trim_start();
    let rest = t.strip_prefix("```")?;
    Some(
        rest.trim()
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase(),
    )
}

/// Extract `@path` / `@"path with spaces"` references from free text.
pub fn extract_refs(text: &str) -> Vec<String> {
    REF_RE
        .captures_iter(text)
        .filter_map(|c| {
            c.get(1)
                .or_else(|| c.get(2))
                .map(|m| m.as_str().trim().trim_end_matches(['.', ':']).to_owned())
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// Note of a `<!-- REGENERATE: … -->` mark, when present.
pub fn regenerate_note(text: &str) -> Option<String> {
    REGEN_NOTE_RE
        .captures(text)
        .map(|c| c.get(1).map(|m| m.as_str().trim()).unwrap_or("").to_owned())
}

/// Label for a checklist line without `- name:`: its first words, else the file name of
/// its first `@path`, else the kind.
pub fn fallback_checklist_name(kind: ChecklistKind, text: &str) -> String {
    let body = GEN_MARK_RE.replace_all(text, " ");
    let body = REF_RE.replace_all(&body, " ");
    let words: Vec<&str> = body
        .split_whitespace()
        .filter(|w| !w.chars().all(|c| c.is_ascii_punctuation()))
        .take(FALLBACK_NAME_WORDS)
        .collect();
    let mut name = words
        .join(" ")
        .trim_end_matches([',', ';', ':', '.'])
        .to_owned();
    if name.is_empty() {
        name = extract_refs(text)
            .first()
            .and_then(|r| r.rsplit('/').next())
            .map(|f| {
                f.rsplit_once('.')
                    .map(|(stem, _)| stem)
                    .unwrap_or(f)
                    .to_owned()
            })
            .unwrap_or_default();
    }
    if name.is_empty() {
        name = capitalize(kind.as_str());
    }
    name.chars().take(MAX_CHECKLIST_NAME_CHARS).collect()
}

fn fmt_mmss(ms: u32) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Parse ```flow lines. Timed `- m:ss-m:ss | Step | description` lines win when any line
/// uses them; otherwise the block is a legacy `A[x] --> B[y]` chain without timings.
fn parse_flow(lines: &[(usize, &str)]) -> (Vec<FlowStep>, Vec<(usize, String)>) {
    let mut steps = Vec::new();
    let mut issues = Vec::new();
    let timed = lines.iter().any(|(_, l)| FLOW_STEP_RE.is_match(l));
    if timed {
        for (i, line) in lines {
            let Some(c) = FLOW_STEP_RE.captures(line) else {
                issues.push((
                    *i,
                    format!(
                        "flow line must look like `- 0:00-0:03 | Step | description`, got \"{}\"",
                        line.trim()
                    ),
                ));
                continue;
            };
            let num = |n: usize| c[n].parse::<u32>().unwrap_or(0);
            let (sm, ss, em, es) = (num(1), num(2), num(3), num(4));
            if ss >= 60 || es >= 60 {
                issues.push((
                    *i,
                    format!("flow time seconds must be 00-59 in \"{}\"", line.trim()),
                ));
                continue;
            }
            let label = c[5].trim().to_owned();
            if label.is_empty() {
                issues.push((
                    *i,
                    "flow step needs a label between the `|` separators".into(),
                ));
                continue;
            }
            steps.push(FlowStep {
                label,
                description: c
                    .get(6)
                    .map(|m| m.as_str().trim().to_owned())
                    .unwrap_or_default(),
                start_ms: Some((sm * 60 + ss) * 1000),
                end_ms: Some((em * 60 + es) * 1000),
            });
        }
        return (steps, issues);
    }
    let joined = lines
        .iter()
        .map(|(_, l)| l.trim())
        .collect::<Vec<_>>()
        .join(" ");
    for node in joined.split("-->") {
        let node = node.trim();
        if node.is_empty() {
            continue;
        }
        let (label, description) = match (node.find('['), node.rfind(']')) {
            (Some(open), Some(close)) if close > open => (
                node[..open].trim().to_owned(),
                node[open + 1..close].trim().to_owned(),
            ),
            _ => (node.to_owned(), String::new()),
        };
        let (label, description) = if label.is_empty() {
            (description, String::new())
        } else {
            (label, description)
        };
        steps.push(FlowStep {
            label,
            description,
            start_ms: None,
            end_ms: None,
        });
    }
    (steps, issues)
}

/// Timing rules for timed flows: contiguous from 0:00, 2-8 steps, 8-25 seconds in total.
fn validate_flow_steps(anchor: &str, steps: &[FlowStep]) -> Vec<PlanIssue> {
    let mut issues = Vec::new();
    if steps.is_empty() || steps.iter().any(|s| s.start_ms.is_none()) {
        return issues;
    }
    let issue = |message: String| PlanIssue {
        line: None,
        anchor: Some(anchor.to_owned()),
        message,
    };
    if !(2..=8).contains(&steps.len()) {
        issues.push(issue(format!(
            "flow needs 2-8 timed steps, found {}",
            steps.len()
        )));
    }
    let mut expected = 0u32;
    for s in steps {
        let (start, end) = (s.start_ms.unwrap_or(0), s.end_ms.unwrap_or(0));
        if start != expected {
            issues.push(issue(format!(
                "flow steps must be contiguous from 0:00: \"{}\" starts at {}, expected {}",
                s.label,
                fmt_mmss(start),
                fmt_mmss(expected)
            )));
        }
        if end <= start {
            issues.push(issue(format!(
                "flow step \"{}\" must end after it starts ({}-{})",
                s.label,
                fmt_mmss(start),
                fmt_mmss(end)
            )));
        }
        expected = end;
    }
    let total = steps.last().and_then(|s| s.end_ms).unwrap_or(0);
    if !(8_000..=25_000).contains(&total) {
        issues.push(issue(format!(
            "flow runs {}s; videos must be 8-25 seconds",
            total / 1000
        )));
    }
    issues
}

/// Parse `input: <role> <checklist-key|@path>`.
fn parse_input_directive(raw: &str, line: usize) -> Result<VideoInput, String> {
    let raw = raw.trim();
    let (role_s, rest) = raw
        .split_once(char::is_whitespace)
        .ok_or_else(|| format!("input must be `<role> <checklist-key|@path>`, got \"{raw}\""))?;
    let role = VideoInputRole::parse(role_s).ok_or_else(|| {
        format!(
            "unknown input role \"{role_s}\" (use start_frame|end_frame|reference_image|\
             reference_video|reference_audio|source_video)"
        )
    })?;
    let rest = rest.trim();
    let target = if CHECKLIST_KEY_RE.is_match(rest) {
        VideoInputTarget::ChecklistKey {
            key: rest.to_owned(),
        }
    } else {
        let refs = extract_refs(rest);
        let path = if let Some(p) = refs.first() {
            p.clone()
        } else {
            let bare = rest.trim_start_matches('@').trim_matches('"').trim();
            if bare.is_empty() || bare.contains(char::is_whitespace) && !rest.contains('@') {
                return Err(format!(
                    "input target must be a checklist key (video-1#1) or @\"path\", got \"{rest}\""
                ));
            }
            bare.to_owned()
        };
        if path.is_empty() {
            return Err(format!("input path is empty in \"{raw}\""));
        }
        VideoInputTarget::AssetPath { path }
    };
    Ok(VideoInput { role, target, line })
}

/// Replace em dashes and en dashes with a hyphen so model-written plans
/// do not carry them into the document the operator reads.
pub fn strip_typography_dashes(markdown: &str) -> String {
    markdown.replace(['—', '–'], "-")
}

/// Split `video:` lines inside ```assets fences that join more than one shot.
///
/// `plus`, `then`, and `cut to` each start a new line. A semicolon does not:
/// a qualifier or an "inspect" note stays on that shot, and a "record the
/// missing…" clause is dropped. A comma list of shots (`hook, post, and
/// return`) is also split. A line that already points at one `@path` stays
/// one line.
pub fn split_compound_video_lines(markdown: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_assets = false;
    for line in markdown.split('\n') {
        let (bare, cr) = match line.strip_suffix('\r') {
            Some(s) => (s, "\r"),
            None => (line, ""),
        };
        if fence_info(bare).as_deref() == Some("assets") {
            in_assets = true;
            out.push(line.to_owned());
            continue;
        }
        if in_assets && bare.trim_start().starts_with("```") {
            in_assets = false;
            out.push(line.to_owned());
            continue;
        }
        if in_assets
            && let Some(caps) = CHECK_RE.captures(bare)
            && caps[4].eq_ignore_ascii_case("video")
        {
            let original = caps.get(5).map(|m| m.as_str()).unwrap_or("");
            let parts = split_video_description(original);
            let unchanged = parts.len() == 1 && parts.first().map(String::as_str) == Some(original);
            if !unchanged {
                for part in parts {
                    out.push(format!(
                        "{}[{}]{}{}: {part}{cr}",
                        &caps[1],
                        &caps[2],
                        &caps[3],
                        caps[4].to_ascii_lowercase()
                    ));
                }
                continue;
            }
        }
        out.push(line.to_owned());
    }
    out.join("\n")
}

fn split_video_description(text: &str) -> Vec<String> {
    let refs = extract_refs(text);
    let body = REF_RE
        .replace_all(text, " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if body.is_empty() {
        return vec![text.trim().to_owned()];
    }
    if !refs.is_empty() && !SHOT_WORD_RE.is_match(&body) {
        let folded = fold_semicolon_clauses(&body).unwrap_or_else(|| body.clone());
        return vec![attach_refs(&folded, &refs)];
    }
    let mut shots: Vec<String> = Vec::new();
    for piece in SHOT_WORD_RE.split(&body) {
        let Some(shot) = fold_semicolon_clauses(piece) else {
            continue;
        };
        if is_constraint(&shot) {
            if let Some(prev) = shots.last_mut() {
                *prev = format!("{prev}. {}", capitalize(&shot));
            }
            continue;
        }
        for shot in explode_shot_list(&shot) {
            let shot = shot
                .trim()
                .trim_matches(|c: char| c == ',' || c == '.')
                .trim();
            if shot.is_empty() || is_record_instruction(shot) {
                continue;
            }
            shots.push(capitalize(shot));
        }
    }
    if shots.is_empty() {
        return vec![attach_refs(&body, &refs)];
    }
    if !refs.is_empty() {
        let idx = shots
            .iter()
            .position(|s| {
                let l = s.to_lowercase();
                l.contains("existing") || l.contains("recording")
            })
            .unwrap_or(0);
        shots[idx] = attach_refs(&shots[idx], &refs);
    }
    shots
}

static SHOT_WORD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\s+plus\s+|\s+then\s+|\s+cut\s+to\s+").expect("shot joiner regex")
});

/// Join semicolon clauses onto one shot. Drop "record / upload if absent".
fn fold_semicolon_clauses(piece: &str) -> Option<String> {
    let mut main: Option<String> = None;
    for clause in piece.split(';') {
        let clause = clause
            .trim()
            .trim_matches(|c: char| c == ',' || c == '.')
            .trim();
        if clause.is_empty() || is_record_instruction(clause) {
            continue;
        }
        match main.as_mut() {
            Some(prev) => *prev = format!("{prev}. {}", capitalize(clause)),
            None => main = Some(clause.to_owned()),
        }
    }
    main.filter(|s| !s.is_empty())
}

fn is_constraint(s: &str) -> bool {
    let l = s.to_lowercase();
    [
        "no ",
        "not ",
        "without ",
        "do not ",
        "don't ",
        "dont ",
        "avoid ",
        "keep ",
        "never ",
        "opaque",
        "if absent",
        "if needed",
    ]
    .iter()
    .any(|p| l.starts_with(p))
}

fn is_record_instruction(s: &str) -> bool {
    let l = s.to_lowercase();
    l.starts_with("record ")
        || l.starts_with("upload ")
        || l.starts_with("film ")
        || l.starts_with("shoot ")
        || l.contains("if absent")
        || l.contains("as needed")
}

/// "hook, post, and return shot" is three shots. A single shot that lists
/// style notes ("one static shot, opaque") is not.
fn explode_shot_list(piece: &str) -> Vec<String> {
    let lower = piece.to_lowercase();
    if lower.starts_with("one ")
        || lower.starts_with("a single ")
        || lower.starts_with("existing ")
        || lower.contains("static")
        || lower.contains("opaque")
        || lower.contains(" no ")
        || piece.matches(',').count() < 2
    {
        return vec![piece.to_owned()];
    }
    let normalized = piece.replace(", and ", ", ").replace(", And ", ", ");
    let parts: Vec<String> = normalized
        .split(',')
        .map(|s| s.trim().trim_start_matches("and ").to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    if parts.len() < 3
        || parts
            .iter()
            .any(|p| is_constraint(p) || p.split_whitespace().count() > 12)
    {
        return vec![piece.to_owned()];
    }
    parts
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn attach_refs(text: &str, refs: &[String]) -> String {
    let mut out = text.trim().to_owned();
    for r in refs {
        let quoted = format!("@\"{r}\"");
        if !out.contains(&quoted) {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&quoted);
        }
    }
    out
}

/// Prose lines in `[start, end)` outside code fences, joined onto one line.
fn summary_between(lines: &[&str], start: usize, end: usize) -> String {
    let mut in_fence = false;
    let mut out: Vec<&str> = Vec::new();
    for raw in lines.iter().take(end).skip(start) {
        let line = raw.trim_end_matches('\r').trim();
        if fence_info(line).is_some() {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || line.is_empty() || line.starts_with("<!--") {
            continue;
        }
        out.push(line);
    }
    out.join(" ")
}

pub fn parse(markdown: &str) -> ParsedPlan {
    let lines: Vec<&str> = markdown.split('\n').collect();
    let mut plan = ParsedPlan::default();

    // Pass 1: headings (outside fences) and fence balance.
    let mut in_fence = false;
    let mut fence_open_line = 0usize;
    let mut headings: Vec<(usize, u8, String, Option<String>)> = Vec::new();
    for (i, raw) in lines.iter().enumerate() {
        let line = raw.trim_end_matches('\r');
        if fence_info(line).is_some() {
            if !in_fence {
                fence_open_line = i;
            }
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(c) = HEADING_RE.captures(line) {
            let level = c[1].len() as u8;
            let text = c[2].trim().to_owned();
            let anchor = c.get(3).map(|m| m.as_str().to_owned());
            headings.push((i, level, text, anchor));
        }
    }
    if in_fence {
        plan.issues.push(PlanIssue {
            line: Some(fence_open_line + 1),
            anchor: None,
            message: "unterminated ``` code fence".into(),
        });
    }

    for (idx, (line, level, text, anchor)) in headings.iter().enumerate() {
        if *level == 1 {
            if plan.title.is_none() {
                plan.title = Some(text.clone()).filter(|t| !t.is_empty());
                let next = headings
                    .get(idx + 1)
                    .map(|(l, _, _, _)| *l)
                    .unwrap_or(lines.len());
                plan.summary = summary_between(&lines, line + 1, next);
            }
            continue;
        }
        if *level > 3 {
            continue;
        }
        // Section end: next heading of level <= this level (level-3 sections end at any ## / ###).
        let end = headings[idx + 1..]
            .iter()
            .find(|(_, l, _, _)| *l <= *level)
            .map(|(l, _, _, _)| *l)
            .unwrap_or(lines.len());
        let is_video = *level == 2 && anchor.as_deref().is_some_and(is_video_anchor);
        plan.sections.push(ParsedSection {
            anchor: anchor.clone(),
            heading: text.clone(),
            level: *level,
            start_line: *line,
            end_line: end,
            is_video,
        });
    }

    // Anchors: required on `##`, unique everywhere.
    let mut seen: HashSet<String> = HashSet::new();
    for s in &plan.sections {
        match &s.anchor {
            None if s.level == 2 => plan.issues.push(PlanIssue {
                line: Some(s.start_line + 1),
                anchor: None,
                message: format!(
                    "section \"{}\" needs an anchor like {{#strategy}}",
                    s.heading
                ),
            }),
            Some(a) => {
                if !seen.insert(a.to_ascii_lowercase()) {
                    plan.issues.push(PlanIssue {
                        line: Some(s.start_line + 1),
                        anchor: Some(a.clone()),
                        message: format!("duplicate anchor {a}"),
                    });
                }
            }
            None => {}
        }
    }

    // Pass 2: video sections.
    let video_sections: Vec<ParsedSection> = plan
        .sections
        .iter()
        .filter(|s| s.is_video)
        .cloned()
        .collect();
    for (pos, sec) in video_sections.iter().enumerate() {
        let anchor = sec.anchor.clone().unwrap_or_default();
        let mut v = ParsedVideo {
            anchor: anchor.clone(),
            title: sec.heading.clone(),
            position: pos as i32,
            track: None,
            audience: None,
            trend: None,
            daypart: None,
            caption: String::new(),
            hypothesis: String::new(),
            flow: String::new(),
            flow_steps: Vec::new(),
            checklist: Vec::new(),
        };
        let mut fence: Option<String> = None;
        let mut flow_blocks = 0usize;
        let mut asset_blocks = 0usize;
        let mut flow_buf: Vec<(usize, &str)> = Vec::new();
        for i in (sec.start_line + 1)..sec.end_line {
            let line = lines[i].trim_end_matches('\r');
            if let Some(info) = fence_info(line) {
                match fence.take() {
                    Some(_) => {} // closing fence
                    None => {
                        match info.as_str() {
                            "flow" => {
                                flow_blocks += 1;
                                flow_buf.clear();
                            }
                            "assets" => asset_blocks += 1,
                            _ => {}
                        }
                        fence = Some(info);
                    }
                }
                continue;
            }
            match fence.as_deref() {
                Some("flow") => {
                    if !line.trim().is_empty() {
                        flow_buf.push((i, line));
                    }
                    v.flow = flow_buf
                        .iter()
                        .map(|(_, l)| *l)
                        .collect::<Vec<_>>()
                        .join("\n");
                }
                Some("assets") => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    if let Some(c) = CHECK_RE.captures(line) {
                        let state =
                            ChecklistState::from_marker(&c[2]).unwrap_or(ChecklistState::Open);
                        let kind = ChecklistKind::parse(&c[4]).unwrap_or(ChecklistKind::Image);
                        let text = c[5].to_owned();
                        let n = v.checklist.len() + 1;
                        let refs = extract_refs(&text);
                        let regenerate_note = regenerate_note(&text);
                        v.checklist.push(ChecklistItem {
                            key: format!("{anchor}#{n}"),
                            state,
                            kind,
                            status: ChecklistStatus::derive(
                                state,
                                !refs.is_empty(),
                                regenerate_note.is_some(),
                            ),
                            name: fallback_checklist_name(kind, &text),
                            name_declared: false,
                            model: None,
                            regenerate_note,
                            preview_url: None,
                            refs,
                            text,
                            line: i,
                            generation: if kind == ChecklistKind::Video {
                                Some(VideoGenerationSpec::default())
                            } else {
                                None
                            },
                            generation_ready: true,
                            generation_blockers: Vec::new(),
                            reuse: None,
                        });
                    } else if let Some(c) = DIR_RE.captures(line) {
                        let Some(item) = v.checklist.last_mut() else {
                            plan.issues.push(PlanIssue {
                                line: Some(i + 1),
                                anchor: Some(anchor.clone()),
                                message: "generation directive must follow a checklist item".into(),
                            });
                            continue;
                        };
                        let key = c[2].to_ascii_lowercase();
                        let val = c[3].trim();
                        if key == "reuse" {
                            match val.parse::<u8>() {
                                Ok(n) if n <= 100 => item.reuse = Some(n),
                                Ok(_) => plan.issues.push(PlanIssue {
                                    line: Some(i + 1),
                                    anchor: Some(anchor.clone()),
                                    message: format!(
                                        "reuse must be between 0 and 100, got \"{val}\""
                                    ),
                                }),
                                Err(_) => plan.issues.push(PlanIssue {
                                    line: Some(i + 1),
                                    anchor: Some(anchor.clone()),
                                    message: format!(
                                        "reuse must be an integer 0–100, got \"{val}\""
                                    ),
                                }),
                            }
                            continue;
                        }
                        if key == "name" {
                            let n = val.chars().count();
                            if n > MAX_CHECKLIST_NAME_CHARS {
                                plan.issues.push(PlanIssue {
                                    line: Some(i + 1),
                                    anchor: Some(anchor.clone()),
                                    message: format!(
                                        "name must be at most {MAX_CHECKLIST_NAME_CHARS} characters, got {n}"
                                    ),
                                });
                            } else {
                                item.name = val.to_owned();
                                item.name_declared = true;
                            }
                            continue;
                        }
                        if key == "model" {
                            if MODEL_RE.is_match(val) {
                                item.model = Some(val.to_owned());
                            } else {
                                plan.issues.push(PlanIssue {
                                    line: Some(i + 1),
                                    anchor: Some(anchor.clone()),
                                    message: format!(
                                        "model must match [a-z0-9._/-]{{1,80}}, got \"{val}\""
                                    ),
                                });
                            }
                            continue;
                        }
                        if item.kind != ChecklistKind::Video {
                            plan.issues.push(PlanIssue {
                                line: Some(i + 1),
                                anchor: Some(anchor.clone()),
                                message: format!(
                                    "generation directives only apply to video: lines (got {})",
                                    item.kind.as_str()
                                ),
                            });
                            continue;
                        }
                        let spec = item
                            .generation
                            .get_or_insert_with(VideoGenerationSpec::default);
                        match key.as_str() {
                            "mode" => match VideoMode::parse(val) {
                                Some(m) => spec.mode = m,
                                None => plan.issues.push(PlanIssue {
                                    line: Some(i + 1),
                                    anchor: Some(anchor.clone()),
                                    message: format!(
                                        "mode must be text|start_frame|first_last_frame|reference|extend, got \"{val}\""
                                    ),
                                }),
                            },
                            "duration" => match val.parse::<u32>() {
                                Ok(d) if (1..=60).contains(&d) => spec.duration = Some(d),
                                Ok(_) => plan.issues.push(PlanIssue {
                                    line: Some(i + 1),
                                    anchor: Some(anchor.clone()),
                                    message: format!(
                                        "duration must be between 1 and 60 seconds, got \"{val}\""
                                    ),
                                }),
                                Err(_) => plan.issues.push(PlanIssue {
                                    line: Some(i + 1),
                                    anchor: Some(anchor.clone()),
                                    message: format!("duration must be an integer, got \"{val}\""),
                                }),
                            },
                            "input" => match parse_input_directive(val, i) {
                                Ok(inp) => spec.inputs.push(inp),
                                Err(msg) => plan.issues.push(PlanIssue {
                                    line: Some(i + 1),
                                    anchor: Some(anchor.clone()),
                                    message: msg,
                                }),
                            },
                            _ => {}
                        }
                    } else {
                        plan.issues.push(PlanIssue {
                            line: Some(i + 1),
                            anchor: Some(anchor.clone()),
                            message:
                                "asset line must look like `- [ ] image|video|audio: description` \
                                 or an indented `- name|model|reuse|mode|duration|input: …` directive"
                                    .into(),
                        });
                    }
                }
                Some(_) => {}
                None => {
                    if let Some(c) = META_RE.captures(line) {
                        let key = c[1].to_ascii_lowercase();
                        let val = c[2].trim().to_owned();
                        match key.as_str() {
                            "track" => match PlanTrack::parse(&val.to_ascii_lowercase()) {
                                Some(t) => v.track = Some(t),
                                None => plan.issues.push(PlanIssue {
                                    line: Some(i + 1),
                                    anchor: Some(anchor.clone()),
                                    message: format!("track must be audience or trend, got \"{val}\""),
                                }),
                            },
                            "audience" => v.audience = Some(val).filter(|s| !s.is_empty()),
                            "trend" => v.trend = Some(val).filter(|s| !s.is_empty()),
                            "daypart" => match Daypart::parse(&val.to_ascii_lowercase()) {
                                Some(d) => v.daypart = Some(d),
                                None => plan.issues.push(PlanIssue {
                                    line: Some(i + 1),
                                    anchor: Some(anchor.clone()),
                                    message: format!(
                                        "daypart must be prime|morning|afternoon|evening|night, got \"{val}\""
                                    ),
                                }),
                            },
                            "caption" => v.caption = val,
                            "hypothesis" => v.hypothesis = val,
                            _ => {}
                        }
                    }
                }
            }
        }
        let (steps, flow_issues) = parse_flow(&flow_buf);
        v.flow_steps = steps;
        plan.issues
            .extend(flow_issues.into_iter().map(|(i, message)| PlanIssue {
                line: Some(i + 1),
                anchor: Some(anchor.clone()),
                message,
            }));
        if flow_blocks != 1 {
            plan.issues.push(PlanIssue {
                line: Some(sec.start_line + 1),
                anchor: Some(anchor.clone()),
                message: format!("expected exactly one ```flow block, found {flow_blocks}"),
            });
        } else if v.flow.trim().is_empty() {
            plan.issues.push(PlanIssue {
                line: Some(sec.start_line + 1),
                anchor: Some(anchor.clone()),
                message: "```flow block is empty".into(),
            });
        }
        if asset_blocks != 1 {
            plan.issues.push(PlanIssue {
                line: Some(sec.start_line + 1),
                anchor: Some(anchor.clone()),
                message: format!("expected exactly one ```assets block, found {asset_blocks}"),
            });
        }
        plan.videos.push(v);
    }

    plan
}

/// Parse and run semantic validation; `Err` carries every issue found.
pub fn parse_and_validate(
    markdown: &str,
    opts: &ValidateOpts,
) -> Result<ParsedPlan, Vec<PlanIssue>> {
    let plan = parse(markdown);
    let issues = validate(&plan, opts);
    if issues.is_empty() {
        Ok(plan)
    } else {
        Err(issues)
    }
}

/// True when a `video:` checklist description asks for multiple shots or for
/// alpha / transparent / green-screen output a video model cannot produce.
pub fn video_checklist_needs_split(text: &str) -> bool {
    MULTI_SHOT_VIDEO_RE.is_match(text)
}

pub fn validate(plan: &ParsedPlan, opts: &ValidateOpts) -> Vec<PlanIssue> {
    let mut issues = plan.issues.clone();

    let summary_len = plan.summary.chars().count();
    if summary_len > MAX_SUMMARY_CHARS {
        issues.push(PlanIssue {
            line: None,
            anchor: None,
            message: format!(
                "summary under the # title must be at most {MAX_SUMMARY_CHARS} characters, found {summary_len}"
            ),
        });
    }
    if opts.require_title {
        if plan.title.is_none() {
            issues.push(PlanIssue {
                line: Some(1),
                anchor: None,
                message: "start the plan with `# <day title>`".into(),
            });
        }
        if plan.summary.is_empty() {
            issues.push(PlanIssue {
                line: None,
                anchor: None,
                message: "add a one-paragraph summary directly under the # title".into(),
            });
        }
    }

    if plan.videos.len() > opts.max_videos {
        issues.push(PlanIssue {
            line: None,
            anchor: None,
            message: format!(
                "too many videos: {} (max {})",
                plan.videos.len(),
                opts.max_videos
            ),
        });
    }

    let (mut audience_n, mut trend_n) = (0usize, 0usize);
    for v in &plan.videos {
        let a = Some(v.anchor.clone());
        match v.track {
            None => issues.push(PlanIssue {
                line: None,
                anchor: a.clone(),
                message: "missing `- track: audience|trend`".into(),
            }),
            Some(PlanTrack::Audience) => {
                audience_n += 1;
                if v.audience.is_none() {
                    issues.push(PlanIssue {
                        line: None,
                        anchor: a.clone(),
                        message: "audience-track video needs `- audience: <id|position|name>`"
                            .into(),
                    });
                }
            }
            Some(PlanTrack::Trend) => {
                trend_n += 1;
                if v.trend.is_none() {
                    issues.push(PlanIssue {
                        line: None,
                        anchor: a.clone(),
                        message: "trend-track video needs `- trend: <id|title>`".into(),
                    });
                }
            }
        }
        if opts.require_caption && v.caption.trim().is_empty() {
            issues.push(PlanIssue {
                line: None,
                anchor: a.clone(),
                message: "missing `- caption: …`".into(),
            });
        }
        issues.extend(validate_flow_steps(&v.anchor, &v.flow_steps));
        for item in &v.checklist {
            if opts.require_names && !item.name_declared {
                issues.push(PlanIssue {
                    line: Some(item.line + 1),
                    anchor: a.clone(),
                    message: format!(
                        "{}: add an indented `- name: <short label>` (max {MAX_CHECKLIST_NAME_CHARS} characters) under the checklist line",
                        item.key
                    ),
                });
            }
            if item.kind == ChecklistKind::Video && video_checklist_needs_split(&item.text) {
                issues.push(PlanIssue {
                    line: Some(item.line + 1),
                    anchor: a.clone(),
                    message: format!(
                        "video checklist line must be one opaque shot (split multi-shot or drop transparent/alpha/green screen): \"{}\"",
                        item.text
                    ),
                });
            }
            if let Some(spec) = &item.generation {
                issues.extend(validate_generation_spec(plan, item, spec));
            }
        }
    }
    if let Some(n) = opts.audience_videos
        && audience_n != n
    {
        issues.push(PlanIssue {
            line: None,
            anchor: None,
            message: format!("expected exactly {n} audience-track videos, found {audience_n}"),
        });
    }
    if let Some(n) = opts.max_trend_videos
        && trend_n > n
    {
        issues.push(PlanIssue {
            line: None,
            anchor: None,
            message: format!("expected at most {n} trend-track videos, found {trend_n}"),
        });
    }
    issues.extend(validate_generation_cycles(plan));
    issues
}

fn find_checklist_item<'a>(plan: &'a ParsedPlan, key: &str) -> Option<&'a ChecklistItem> {
    plan.videos
        .iter()
        .flat_map(|v| v.checklist.iter())
        .find(|c| c.key == key)
}

fn validate_generation_spec(
    plan: &ParsedPlan,
    item: &ChecklistItem,
    spec: &VideoGenerationSpec,
) -> Vec<PlanIssue> {
    let mut issues = Vec::new();
    let a = Some(item.key.split('#').next().unwrap_or("").to_owned());

    // Mode / required-input sanity (soft: warn when clearly mismatched).
    match spec.mode {
        VideoMode::Text => {
            if !spec.inputs.is_empty() {
                issues.push(PlanIssue {
                    line: Some(item.line + 1),
                    anchor: a.clone(),
                    message: format!(
                        "{}: mode text does not take inputs; remove input directives or set mode",
                        item.key
                    ),
                });
            }
        }
        VideoMode::StartFrame => {
            if !spec
                .inputs
                .iter()
                .any(|i| i.role == VideoInputRole::StartFrame)
            {
                issues.push(PlanIssue {
                    line: Some(item.line + 1),
                    anchor: a.clone(),
                    message: format!(
                        "{}: mode start_frame needs an input: start_frame …",
                        item.key
                    ),
                });
            }
        }
        VideoMode::FirstLastFrame => {
            let has_start = spec
                .inputs
                .iter()
                .any(|i| i.role == VideoInputRole::StartFrame);
            let has_end = spec
                .inputs
                .iter()
                .any(|i| i.role == VideoInputRole::EndFrame);
            if !has_start || !has_end {
                issues.push(PlanIssue {
                    line: Some(item.line + 1),
                    anchor: a.clone(),
                    message: format!(
                        "{}: mode first_last_frame needs start_frame and end_frame inputs",
                        item.key
                    ),
                });
            }
        }
        VideoMode::Extend => {
            if !spec
                .inputs
                .iter()
                .any(|i| i.role == VideoInputRole::SourceVideo)
            {
                issues.push(PlanIssue {
                    line: Some(item.line + 1),
                    anchor: a.clone(),
                    message: format!("{}: mode extend needs an input: source_video …", item.key),
                });
            }
        }
        VideoMode::Reference => {
            if spec.inputs.is_empty() {
                issues.push(PlanIssue {
                    line: Some(item.line + 1),
                    anchor: a.clone(),
                    message: format!(
                        "{}: mode reference needs at least one input directive",
                        item.key
                    ),
                });
            }
        }
    }

    for inp in &spec.inputs {
        match &inp.target {
            VideoInputTarget::ChecklistKey { key } => {
                if key == &item.key {
                    issues.push(PlanIssue {
                        line: Some(inp.line + 1),
                        anchor: a.clone(),
                        message: format!("{}: input cannot reference itself ({key})", item.key),
                    });
                    continue;
                }
                match find_checklist_item(plan, key) {
                    None => issues.push(PlanIssue {
                        line: Some(inp.line + 1),
                        anchor: a.clone(),
                        message: format!(
                            "{}: input references unknown checklist key {key}",
                            item.key
                        ),
                    }),
                    Some(dep) => {
                        let want = inp.role.expected_kind();
                        if dep.kind != want {
                            issues.push(PlanIssue {
                                line: Some(inp.line + 1),
                                anchor: a.clone(),
                                message: format!(
                                    "{}: input {} expects a {} asset, but {key} is {}",
                                    item.key,
                                    inp.role.as_str(),
                                    want.as_str(),
                                    dep.kind.as_str()
                                ),
                            });
                        }
                    }
                }
            }
            VideoInputTarget::AssetPath { path: _ } => {
                // Existence is checked at readiness time (asset manager may change).
            }
        }
    }
    issues
}

/// Reject cycles among checklist-key generation inputs (A needs B needs A).
fn validate_generation_cycles(plan: &ParsedPlan) -> Vec<PlanIssue> {
    use std::collections::{HashMap, HashSet};

    let mut edges: HashMap<String, Vec<String>> = HashMap::new();
    for v in &plan.videos {
        for item in &v.checklist {
            let Some(spec) = &item.generation else {
                continue;
            };
            for inp in &spec.inputs {
                if let VideoInputTarget::ChecklistKey { key } = &inp.target {
                    edges.entry(item.key.clone()).or_default().push(key.clone());
                }
            }
        }
    }

    let mut issues = Vec::new();
    for start in edges.keys() {
        let mut stack = vec![start.clone()];
        let mut seen = HashSet::new();
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur.clone()) {
                continue;
            }
            for next in edges.get(&cur).into_iter().flatten() {
                if next == start {
                    issues.push(PlanIssue {
                        line: None,
                        anchor: Some(start.split('#').next().unwrap_or("").to_owned()),
                        message: format!(
                            "generation input cycle involving {start} (via checklist-key inputs)"
                        ),
                    });
                    break;
                }
                stack.push(next.clone());
            }
        }
    }
    // Deduplicate cycle messages.
    let mut uniq = HashSet::new();
    issues.retain(|i| uniq.insert(i.message.clone()));
    issues
}

/// All `@path` references used by video checklists (output refs + generation asset inputs).
pub fn referenced_paths(plan: &ParsedPlan) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for v in &plan.videos {
        for c in &v.checklist {
            for r in &c.refs {
                out.push((c.key.clone(), r.clone()));
            }
            if let Some(spec) = &c.generation {
                for inp in &spec.inputs {
                    if let VideoInputTarget::AssetPath { path } = &inp.target {
                        out.push((c.key.clone(), path.clone()));
                    }
                }
            }
        }
    }
    out
}

/// Known asset-manager file used when computing generation readiness.
#[derive(Debug, Clone)]
pub struct KnownAsset {
    pub path: String,
    pub kind: ChecklistKind,
}

/// Compute `generation_ready` / `generation_blockers` for every checklist item in place.
///
/// Same-plan inputs are ready only when the dependency item is [`ChecklistState::Done`]
/// and has an attached `@path`. Asset-manager inputs must exist with the expected kind.
pub fn annotate_generation_readiness(plan: &mut ParsedPlan, known: &[KnownAsset]) {
    // Snapshot dependency states first (immutable view).
    let states: Vec<(String, ChecklistState, ChecklistKind, Vec<String>)> = plan
        .videos
        .iter()
        .flat_map(|v| {
            v.checklist
                .iter()
                .map(|c| (c.key.clone(), c.state, c.kind, c.refs.clone()))
        })
        .collect();
    let lookup = |key: &str| -> Option<&(String, ChecklistState, ChecklistKind, Vec<String>)> {
        states.iter().find(|(k, _, _, _)| k == key)
    };

    for v in &mut plan.videos {
        for item in &mut v.checklist {
            let Some(spec) = item.generation.clone() else {
                item.generation_ready = true;
                item.generation_blockers.clear();
                continue;
            };
            let mut blockers = Vec::new();
            for inp in &spec.inputs {
                match &inp.target {
                    VideoInputTarget::ChecklistKey { key } => match lookup(key) {
                        None => blockers.push(GenerationBlocker {
                            role: inp.role.as_str().into(),
                            target_kind: "checklist_key".into(),
                            target: key.clone(),
                            reason: format!("unknown checklist key {key}"),
                        }),
                        Some((_, state, kind, refs)) => {
                            let want = inp.role.expected_kind();
                            if *kind != want {
                                blockers.push(GenerationBlocker {
                                    role: inp.role.as_str().into(),
                                    target_kind: "checklist_key".into(),
                                    target: key.clone(),
                                    reason: format!(
                                        "{key} is {}, expected {}",
                                        kind.as_str(),
                                        want.as_str()
                                    ),
                                });
                            } else if *state != ChecklistState::Done || refs.is_empty() {
                                blockers.push(GenerationBlocker {
                                    role: inp.role.as_str().into(),
                                    target_kind: "checklist_key".into(),
                                    target: key.clone(),
                                    reason: format!(
                                        "{key} must be confirmed (done with an attached asset)"
                                    ),
                                });
                            }
                        }
                    },
                    VideoInputTarget::AssetPath { path } => {
                        let want = inp.role.expected_kind();
                        match known.iter().find(|a| a.path == *path) {
                            None => blockers.push(GenerationBlocker {
                                role: inp.role.as_str().into(),
                                target_kind: "asset_path".into(),
                                target: path.clone(),
                                reason: format!("@{path} is not in the asset manager"),
                            }),
                            Some(a) if a.kind != want => blockers.push(GenerationBlocker {
                                role: inp.role.as_str().into(),
                                target_kind: "asset_path".into(),
                                target: path.clone(),
                                reason: format!(
                                    "@{path} is {}, expected {}",
                                    a.kind.as_str(),
                                    want.as_str()
                                ),
                            }),
                            Some(_) => {}
                        }
                    }
                }
            }
            item.generation_ready = blockers.is_empty();
            item.generation_blockers = blockers;
        }
    }
}

/// Resolve the concrete asset path for a generation input given the current plan + known assets.
pub fn resolve_input_asset_path(plan: &ParsedPlan, inp: &VideoInput) -> Result<String, String> {
    match &inp.target {
        VideoInputTarget::AssetPath { path } => Ok(path.clone()),
        VideoInputTarget::ChecklistKey { key } => {
            let item = find_checklist_item(plan, key)
                .ok_or_else(|| format!("unknown checklist key {key}"))?;
            if item.state != ChecklistState::Done || item.refs.is_empty() {
                return Err(format!(
                    "{key} must be confirmed (done with an attached asset)"
                ));
            }
            Ok(item.refs[0].clone())
        }
    }
}

/// Rewrite a checklist item's state (and optionally attach an `@"path"` reference).
/// Returns `None` when the key does not exist.
pub fn set_checklist_state(
    markdown: &str,
    key: &str,
    state: ChecklistState,
    attach_path: Option<&str>,
) -> Option<String> {
    edit_checklist_item(
        markdown,
        key,
        &ChecklistEdit {
            state: Some(state),
            attach_path,
            ..Default::default()
        },
    )
    .ok()
}

/// Changes to one checklist line. `None` leaves a field untouched; for `model` and
/// `regenerate_note`, `Some(None)` removes it.
#[derive(Debug, Default, Clone)]
pub struct ChecklistEdit<'a> {
    pub state: Option<ChecklistState>,
    pub attach_path: Option<&'a str>,
    pub model: Option<Option<&'a str>>,
    pub regenerate_note: Option<Option<&'a str>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChecklistEditError {
    UnknownKey,
    Invalid(String),
}

/// Apply a [`ChecklistEdit`] to the markdown.
pub fn edit_checklist_item(
    markdown: &str,
    key: &str,
    edit: &ChecklistEdit<'_>,
) -> Result<String, ChecklistEditError> {
    let plan = parse(markdown);
    let item = plan
        .videos
        .iter()
        .flat_map(|v| v.checklist.iter())
        .find(|c| c.key == key)
        .ok_or(ChecklistEditError::UnknownKey)?;
    let mut lines: Vec<String> = markdown.split('\n').map(str::to_owned).collect();
    let old = lines
        .get(item.line)
        .cloned()
        .ok_or(ChecklistEditError::UnknownKey)?;
    let c = CHECK_RE
        .captures(old.trim_end_matches('\r'))
        .ok_or(ChecklistEditError::UnknownKey)?;
    let mut text = c[5].to_owned();
    // Attaching a generated file replaces any previous @"path" and drops
    // GENERATE / REGENERATE marks so the line becomes pending review.
    // Parallel callers each lock the plan row, so every asset lands.
    if let Some(p) = edit.attach_path {
        let stripped = REF_RE.replace_all(&text, "");
        let stripped = GEN_MARK_RE.replace_all(&stripped, "");
        let body = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
        text = if body.is_empty() {
            format!("@\"{p}\"")
        } else {
            format!("{body} @\"{p}\"")
        };
    }
    if let Some(note) = edit.regenerate_note {
        let stripped = REGEN_NOTE_RE.replace_all(&text, "");
        text = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
        let note = note
            .map(|n| {
                n.replace("-->", "->")
                    .replace("<!--", "<-")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|n| !n.is_empty());
        if let Some(n) = note {
            let n: String = n.chars().take(MAX_REGENERATE_NOTE_CHARS).collect();
            text = format!("{text} <!-- REGENERATE: {n} -->");
        }
    }
    let state = edit.state.unwrap_or(item.state);
    let cr = if old.ends_with('\r') { "\r" } else { "" };
    lines[item.line] = format!(
        "{}[{}]{}{}: {}{cr}",
        &c[1],
        state.marker(),
        &c[3],
        &c[4].to_ascii_lowercase(),
        text
    );

    if let Some(model) = edit.model {
        if let Some(m) = model
            && !MODEL_RE.is_match(m)
        {
            return Err(ChecklistEditError::Invalid(format!(
                "model must match [a-z0-9._/-]{{1,80}}, got \"{m}\""
            )));
        }
        let mut end = item.line + 1;
        let mut model_line = None;
        let mut indent = String::from("  ");
        while let Some(l) = lines.get(end) {
            let Some(d) = DIR_RE.captures(l.trim_end_matches('\r')) else {
                break;
            };
            if end == item.line + 1 {
                indent = d[1].to_owned();
            }
            if d[2].eq_ignore_ascii_case("model") {
                model_line = Some(end);
            }
            end += 1;
        }
        match (model, model_line) {
            (Some(m), Some(i)) => lines[i] = format!("{indent}- model: {m}{cr}"),
            (Some(m), None) => lines.insert(end, format!("{indent}- model: {m}{cr}")),
            (None, Some(i)) => {
                lines.remove(i);
            }
            (None, None) => {}
        }
    }
    Ok(lines.join("\n"))
}

/// How much of a past plan to return to an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanReadPart {
    Outline,
    Section,
    Lines,
    All,
}

impl PlanReadPart {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "outline" | "" => Some(Self::Outline),
            "section" => Some(Self::Section),
            "lines" => Some(Self::Lines),
            "all" => Some(Self::All),
            _ => None,
        }
    }
}

const MAX_READ_LINES: usize = 200;

/// Slice plan markdown the way the desk `read_plan` tool does.
pub fn read_plan_part(
    markdown: &str,
    part: PlanReadPart,
    anchor: Option<&str>,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> serde_json::Value {
    let lines: Vec<&str> = markdown.split('\n').collect();
    let line_count = lines.len();
    match part {
        PlanReadPart::All => json!({
            "part": "all",
            "line_count": line_count,
            "markdown": markdown,
        }),
        PlanReadPart::Lines => {
            let start = start_line.unwrap_or(1).max(1);
            let end = end_line
                .unwrap_or(start + MAX_READ_LINES - 1)
                .max(start)
                .min(line_count);
            let capped = end.min(start + MAX_READ_LINES - 1).min(line_count);
            let slice = if start > line_count {
                String::new()
            } else {
                lines[start - 1..capped].join("\n")
            };
            json!({
                "part": "lines",
                "line_count": line_count,
                "start_line": start,
                "end_line": capped,
                "markdown": slice,
            })
        }
        PlanReadPart::Section => {
            let want = match anchor.map(str::trim).filter(|a| !a.is_empty()) {
                Some(a) => a,
                None => {
                    return json!({
                        "part": "section",
                        "line_count": line_count,
                        "error": "section needs an anchor (e.g. video-1).",
                    });
                }
            };
            let plan = parse(markdown);
            let sec = plan.sections.iter().find(|s| {
                s.anchor
                    .as_deref()
                    .is_some_and(|a| a.eq_ignore_ascii_case(want))
            });
            match sec {
                Some(s) => json!({
                    "part": "section",
                    "line_count": line_count,
                    "anchor": s.anchor,
                    "heading": s.heading,
                    "start_line": s.start_line + 1,
                    "end_line": s.end_line,
                    "markdown": lines[s.start_line..s.end_line].join("\n"),
                }),
                None => {
                    let known: Vec<String> = plan
                        .sections
                        .iter()
                        .filter_map(|s| s.anchor.clone())
                        .collect();
                    json!({
                        "part": "section",
                        "line_count": line_count,
                        "error": format!("unknown anchor \"{want}\""),
                        "known_anchors": known,
                    })
                }
            }
        }
        PlanReadPart::Outline => {
            let plan = parse(markdown);
            let sections: Vec<serde_json::Value> = plan
                .sections
                .iter()
                .map(|s| {
                    let checklist: Vec<serde_json::Value> = if s.is_video {
                        plan.videos
                            .iter()
                            .find(|v| Some(v.anchor.as_str()) == s.anchor.as_deref())
                            .map(|v| {
                                v.checklist
                                    .iter()
                                    .map(|c| {
                                        let mut obj = json!({
                                            "key": c.key,
                                            "state": match c.state {
                                                ChecklistState::Open => "open",
                                                ChecklistState::Pending => "pending",
                                                ChecklistState::Done => "done",
                                            },
                                            "status": c.status,
                                            "kind": c.kind.as_str(),
                                            "name": c.name,
                                            "text": c.text,
                                            "generation_ready": c.generation_ready,
                                        });
                                        if let Some(model) = &c.model {
                                            obj["model"] = json!(model);
                                        }
                                        if let Some(note) = &c.regenerate_note {
                                            obj["regenerate_note"] = json!(note);
                                        }
                                        if let Some(spec) = &c.generation {
                                            obj["generation"] = json!({
                                                "mode": spec.mode.as_str(),
                                                "duration": spec.duration,
                                                "inputs": spec.inputs.iter().map(|inp| {
                                                    json!({
                                                        "role": inp.role.as_str(),
                                                        "target": match &inp.target {
                                                            VideoInputTarget::ChecklistKey { key } => json!({
                                                                "type": "checklist_key",
                                                                "key": key,
                                                            }),
                                                            VideoInputTarget::AssetPath { path } => json!({
                                                                "type": "asset_path",
                                                                "path": path,
                                                            }),
                                                        },
                                                    })
                                                }).collect::<Vec<_>>(),
                                            });
                                        }
                                        if !c.generation_blockers.is_empty() {
                                            obj["generation_blockers"] = json!(c.generation_blockers);
                                        }
                                        if let Some(reuse) = c.reuse {
                                            obj["reuse"] = json!(reuse);
                                        }
                                        obj
                                    })
                                    .collect()
                            })
                            .unwrap_or_default()
                    } else {
                        vec![]
                    };
                    json!({
                        "heading": s.heading,
                        "anchor": s.anchor,
                        "start_line": s.start_line + 1,
                        "checklist": checklist,
                    })
                })
                .collect();
            json!({
                "part": "outline",
                "line_count": line_count,
                "title": plan.title,
                "summary": plan.summary,
                "sections": sections,
            })
        }
    }
}

#[derive(Debug, Clone)]
pub struct CommentNote {
    pub anchor: String,
    pub author: String,
    pub body: String,
}

/// Raw markdown with comments injected at the end of the anchored section as
/// `> User commented: …` / `> Agent commented: …` so an agent can read everything in one file.
pub fn annotate(markdown: &str, comments: &[CommentNote]) -> String {
    if comments.is_empty() {
        return markdown.to_owned();
    }
    let plan = parse(markdown);
    let lines: Vec<&str> = markdown.split('\n').collect();
    let mut inserts: Vec<(usize, Vec<String>)> = Vec::new(); // (insert-before line index, text)
    let mut orphans: Vec<&CommentNote> = Vec::new();

    for c in comments {
        let sec = plan.sections.iter().find(|s| {
            s.anchor
                .as_deref()
                .is_some_and(|a| a.eq_ignore_ascii_case(&c.anchor))
        });
        match sec {
            Some(s) => {
                // Insert before trailing blank lines of the section.
                let mut at = s.end_line;
                while at > s.start_line + 1
                    && lines.get(at - 1).is_some_and(|l| l.trim().is_empty())
                {
                    at -= 1;
                }
                let who = if c.author == "agent" { "Agent" } else { "User" };
                let note = format!("> {who} commented: {}", c.body.replace('\n', " "));
                match inserts.iter_mut().find(|(i, _)| *i == at) {
                    Some((_, v)) => v.push(note),
                    None => inserts.push((at, vec![note])),
                }
            }
            None => orphans.push(c),
        }
    }

    inserts.sort_by_key(|(i, _)| *i);
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + comments.len() * 2);
    let mut ins_iter = inserts.into_iter().peekable();
    for (i, l) in lines.iter().enumerate() {
        while ins_iter.peek().is_some_and(|(at, _)| *at == i) {
            let (_, notes) = ins_iter.next().unwrap();
            out.push(String::new());
            out.extend(notes);
        }
        out.push((*l).to_owned());
    }
    for (_, notes) in ins_iter {
        out.push(String::new());
        out.extend(notes);
    }
    if !orphans.is_empty() {
        out.push(String::new());
        for c in orphans {
            let who = if c.author == "agent" { "Agent" } else { "User" };
            out.push(format!(
                "> {who} commented on #{}: {}",
                c.anchor,
                c.body.replace('\n', " ")
            ));
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"# Plan for 2026-10-16

## Strategy {#strategy}
Lean into the study hook.

## Video 1 — Girl studying vs scrolling {#video-1}
- track: audience
- audience: 2
- daypart: prime
- caption: I couldn't get off Instagram even for exams
- hypothesis: before/after screen time converts

She scrolls with books open.

```flow
Hook[Girl scrolling] --> Problem[12h screen time] --> Reveal[35m screen time]
```

```assets
- [x] image: app icon @"App Store/app-icon.png"
- [ ] video: girl at desk scrolling, one static shot
- [~] image: screen time screenshot showing 12h
```

## Video 2 — Trend remix {#video-2}
- track: trend
- trend: sound_of_the_day
- caption: POV: finals week

```flow
A --> B
```

```assets
- [ ] audio: trending sound
```
"#;

    #[test]
    fn parses_videos_and_checklists() {
        let p = parse(SAMPLE);
        assert!(p.issues.is_empty(), "{:?}", p.issues);
        assert_eq!(p.videos.len(), 2);
        let v = &p.videos[0];
        assert_eq!(v.anchor, "video-1");
        assert_eq!(v.track, Some(PlanTrack::Audience));
        assert_eq!(v.audience.as_deref(), Some("2"));
        assert_eq!(v.daypart, Some(Daypart::Prime));
        assert_eq!(v.checklist.len(), 3);
        assert_eq!(v.checklist[0].state, ChecklistState::Done);
        assert_eq!(v.checklist[0].refs, vec!["App Store/app-icon.png"]);
        assert_eq!(v.checklist[1].state, ChecklistState::Open);
        assert_eq!(v.checklist[2].state, ChecklistState::Pending);
        assert_eq!(v.checklist[2].key, "video-1#3");
        assert!(v.flow.contains("Hook"));
        assert_eq!(p.videos[1].position, 1);
        assert_eq!(p.videos[1].trend.as_deref(), Some("sound_of_the_day"));
    }

    #[test]
    fn validate_counts() {
        let p = parse(SAMPLE);
        let ok = ValidateOpts {
            audience_videos: Some(1),
            max_trend_videos: Some(1),
            require_caption: true,
            ..Default::default()
        };
        assert!(validate(&p, &ok).is_empty());
        let bad = ValidateOpts {
            audience_videos: Some(5),
            ..Default::default()
        };
        assert_eq!(validate(&p, &bad).len(), 1);
    }

    #[test]
    fn reports_missing_blocks_and_anchors() {
        let md = "## No anchor\n\n## Video 1 {#video-1}\n- track: audience\n";
        let p = parse(md);
        let issues = validate(&p, &ValidateOpts::default());
        let text: Vec<String> = issues.iter().map(|i| i.to_string()).collect();
        assert!(
            text.iter().any(|t| t.contains("needs an anchor")),
            "{text:?}"
        );
        assert!(text.iter().any(|t| t.contains("```flow")), "{text:?}");
        assert!(text.iter().any(|t| t.contains("```assets")), "{text:?}");
        assert!(
            text.iter()
                .any(|t| t.contains("audience-track video needs")),
            "{text:?}"
        );
    }

    #[test]
    fn duplicate_anchor_flagged() {
        let md = "## A {#x}\n\n## B {#x}\n";
        let p = parse(md);
        assert!(
            p.issues
                .iter()
                .any(|i| i.message.contains("duplicate anchor"))
        );
    }

    #[test]
    fn toggles_checklist_state() {
        let out = set_checklist_state(
            SAMPLE,
            "video-1#2",
            ChecklistState::Done,
            Some("Generated/girl.mp4"),
        )
        .unwrap();
        let p = parse(&out);
        let item = &p.videos[0].checklist[1];
        assert_eq!(item.state, ChecklistState::Done);
        assert_eq!(item.refs, vec!["Generated/girl.mp4"]);
        assert!(set_checklist_state(SAMPLE, "video-9#1", ChecklistState::Done, None).is_none());
    }

    #[test]
    fn attach_replaces_ref_and_strips_regenerate_mark() {
        let md = SAMPLE.replace(
            "- [~] image: screen time screenshot showing 12h",
            "- [~] image: screen time screenshot showing 12h @\"Generated images/old.png\" <!-- REGENERATE: brighter -->",
        );
        let out = set_checklist_state(
            &md,
            "video-1#3",
            ChecklistState::Pending,
            Some("Generated images/new.png"),
        )
        .unwrap();
        let parsed = parse(&out);
        let item = parsed.videos[0]
            .checklist
            .iter()
            .find(|c| c.key == "video-1#3")
            .unwrap();
        assert_eq!(item.state, ChecklistState::Pending);
        assert_eq!(item.refs, vec!["Generated images/new.png"]);
        assert!(!item.text.contains("REGENERATE"));
        assert!(!item.text.contains("old.png"));
    }

    #[test]
    fn flags_compound_video_checklist_lines() {
        assert!(video_checklist_needs_split(
            "Original hand-and-phone hook plus a fictional DM reply; record missing app transitions"
        ));
        assert!(video_checklist_needs_split(
            "overhead desk hook, then return-to-notebook shot"
        ));
        assert!(video_checklist_needs_split(
            "girl scrolling, cut to studying"
        ));
        assert!(video_checklist_needs_split(
            "girl on transparent background"
        ));
        assert!(video_checklist_needs_split("subject with alpha channel"));
        assert!(video_checklist_needs_split("character on green screen"));
        assert!(!video_checklist_needs_split(
            "girl at desk scrolling, one static shot, opaque"
        ));
        assert!(!video_checklist_needs_split(
            "locked close-up of hands at a kitchen counter; small distracted hesitation, one static shot, opaque"
        ));
        assert!(!video_checklist_needs_split(
            "@\"Screen recordings/using instagram.MP4\" — Existing real-app recording; inspect for a usable excerpt"
        ));

        let md = r#"## Video 1 — Bad checklist {#video-1}
- track: audience
- audience: 1
- daypart: prime
- caption: hi
- hypothesis: test

```flow
A --> B
```

```assets
- [ ] video: Original overhead desk hook plus a fictional study-group post
- [ ] video: girl scrolling, one static shot, opaque
```
"#;
        let p = parse(md);
        let issues = validate(&p, &ValidateOpts::default());
        assert!(
            issues.iter().any(|i| i.message.contains("one opaque shot")),
            "{issues:?}"
        );
        assert!(
            !issues.iter().any(|i| i.message.contains("girl scrolling")),
            "single-shot line should not be flagged: {issues:?}"
        );
    }

    #[test]
    fn splits_joined_video_checklist_lines() {
        let md = r#"## Video 1 — Hook {#video-1}
- track: audience
- caption: hi

```assets
- [ ] video: Original hand-and-phone hook plus a fictional DM reply; record missing app transitions if absent from the existing recording
- [ ] video: Existing Instagram-use recording; inspect for usable task and session footage @"Screen recordings/using instagram.MP4"
- [ ] video: One original static talking-head shot; no camera moves or fabricated testimonial
- [ ] video: Original overhead desk hook, fictional study-group post, and return-to-notebook shot; record missing app footage as needed
- [ ] video: Original Instagram save-and-exit demonstration plus missing app transitions
```
"#;
        let out = split_compound_video_lines(md);
        let lines: Vec<&str> = out.lines().filter(|l| l.contains("video:")).collect();
        assert_eq!(
            lines,
            vec![
                "- [ ] video: Original hand-and-phone hook",
                "- [ ] video: A fictional DM reply",
                "- [ ] video: Existing Instagram-use recording. Inspect for usable task and session footage @\"Screen recordings/using instagram.MP4\"",
                "- [ ] video: One original static talking-head shot. No camera moves or fabricated testimonial",
                "- [ ] video: Original overhead desk hook",
                "- [ ] video: Fictional study-group post",
                "- [ ] video: Return-to-notebook shot",
                "- [ ] video: Original Instagram save-and-exit demonstration",
                "- [ ] video: Missing app transitions",
            ]
        );
        let healed = strip_typography_dashes(&out);
        assert!(!healed.contains('—'));
        assert!(
            !validate(&parse(&healed), &ValidateOpts::default())
                .iter()
                .any(|i| i.message.contains("one opaque shot"))
        );

        let note = split_video_description(
            "locked close-up of hands at a kitchen counter; small distracted hesitation, one static shot, opaque",
        );
        assert_eq!(
            note,
            vec![
                "Locked close-up of hands at a kitchen counter. Small distracted hesitation, one static shot, opaque"
            ]
        );
    }

    #[test]
    fn annotates_comments_in_section() {
        let out = annotate(
            SAMPLE,
            &[CommentNote {
                anchor: "video-1".into(),
                author: "user".into(),
                body: "Make the hook faster".into(),
            }],
        );
        let idx_comment = out.find("> User commented: Make the hook faster").unwrap();
        let idx_v2 = out.find("## Video 2").unwrap();
        let idx_v1 = out.find("## Video 1").unwrap();
        assert!(idx_v1 < idx_comment && idx_comment < idx_v2);
        // Annotated markdown still parses to the same videos.
        assert_eq!(parse(&out).videos.len(), 2);
    }

    #[test]
    fn headings_inside_fences_ignored() {
        let md = "## A {#video-1}\n```flow\n## not a heading\nx\n```\n```assets\n```\n";
        let p = parse(md);
        assert_eq!(p.sections.len(), 1);
        assert_eq!(p.videos[0].flow, "## not a heading\nx");
    }

    #[test]
    fn refs_support_quotes_and_bare() {
        let refs = extract_refs(r#"use @"Screen recordings/demo 1.mp4" and @Characters/girl.png."#);
        assert_eq!(
            refs,
            vec!["Screen recordings/demo 1.mp4", "Characters/girl.png"]
        );
    }

    #[test]
    fn parses_generation_directives() {
        let md = r#"## Video 1 — Ref {#video-1}
- track: audience
- audience: 1
- daypart: prime
- caption: hi
- hypothesis: test

```flow
A --> B
```

```assets
- [ ] image: character portrait on a plain wall
  - reuse: 80
- [ ] video: character turns to the door, one shot, opaque
  - reuse: 35
  - mode: reference
  - duration: 8
  - input: reference_image video-1#1
  - input: reference_video @"Screen recordings/walk.mp4"
- [ ] video: continue into the hallway
  - mode: extend
  - duration: 5
  - input: source_video video-1#2
```
"#;
        let p = parse(md);
        assert!(p.issues.is_empty(), "{:?}", p.issues);
        let v = &p.videos[0];
        assert_eq!(v.checklist.len(), 3);
        assert!(v.checklist[0].generation.is_none());
        assert_eq!(v.checklist[0].reuse, Some(80));
        assert_eq!(v.checklist[1].reuse, Some(35));
        let spec = v.checklist[1].generation.as_ref().unwrap();
        assert_eq!(spec.mode, VideoMode::Reference);
        assert_eq!(spec.duration, Some(8));
        assert_eq!(spec.inputs.len(), 2);
        assert_eq!(spec.inputs[0].role, VideoInputRole::ReferenceImage);
        assert_eq!(
            spec.inputs[0].target,
            VideoInputTarget::ChecklistKey {
                key: "video-1#1".into()
            }
        );
        assert_eq!(
            spec.inputs[1].target,
            VideoInputTarget::AssetPath {
                path: "Screen recordings/walk.mp4".into()
            }
        );
        let ext = v.checklist[2].generation.as_ref().unwrap();
        assert_eq!(ext.mode, VideoMode::Extend);
        assert_eq!(ext.inputs[0].role, VideoInputRole::SourceVideo);
        assert_eq!(v.checklist[2].reuse, None);

        let issues = validate(&p, &ValidateOpts::default());
        assert!(
            issues.is_empty(),
            "valid generation specs should pass: {issues:?}"
        );
    }

    #[test]
    fn generation_readiness_and_cycles() {
        let md = r#"## Video 1 — Hook {#video-1}
- track: audience
- audience: 1
- caption: hi

```flow
A --> B
```

```assets
- [x] image: character @"Characters/girl.png"
- [ ] video: turn to door
  - mode: reference
  - input: reference_image video-1#1
- [ ] video: loop A
  - mode: extend
  - input: source_video video-1#4
- [ ] video: loop B
  - mode: extend
  - input: source_video video-1#3
```
"#;
        let p = parse(md);
        let cycle_issues = validate(&p, &ValidateOpts::default());
        assert!(
            cycle_issues.iter().any(|i| i.message.contains("cycle")),
            "{cycle_issues:?}"
        );

        // Drop the cycle pair and check readiness.
        let md2 = r#"## Video 1 — Hook {#video-1}
- track: audience
- audience: 1
- caption: hi

```flow
A --> B
```

```assets
- [x] image: character @"Characters/girl.png"
- [ ] video: turn to door
  - mode: reference
  - input: reference_image video-1#1
  - input: reference_video @"Screen recordings/walk.mp4"
```
"#;
        let mut p2 = parse(md2);
        annotate_generation_readiness(
            &mut p2,
            &[KnownAsset {
                path: "Screen recordings/walk.mp4".into(),
                kind: ChecklistKind::Video,
            }],
        );
        assert!(p2.videos[0].checklist[1].generation_ready);
        assert!(p2.videos[0].checklist[1].generation_blockers.is_empty());

        // Unconfirmed prerequisite blocks generation.
        let md3 = md2.replace(
            "- [x] image: character @\"Characters/girl.png\"",
            "- [ ] image: character portrait",
        );
        let mut p3 = parse(&md3);
        annotate_generation_readiness(&mut p3, &[]);
        assert!(!p3.videos[0].checklist[1].generation_ready);
        assert!(
            p3.videos[0].checklist[1]
                .generation_blockers
                .iter()
                .any(|b| b.target == "video-1#1")
        );
    }

    const SAMPLE_V2: &str = r#"# Tactile ergonomics vs digital clutter
Contrast timer fatigue with the rotary dial; test whether tactile beats
yesterday's widget reel.

## Video 1: Why 99% of focus timers ruin your dopamine {#video-1}
- track: audience
- audience: 2
- daypart: prime
- caption: Your timer is shouting at you
- hypothesis: Tactile contrast beats the widget reel.

Concept prose.

```flow
- 0:00-0:03 | Hook: Digital noise | Loud beeps and blinking counters
- 0:03-0:07 | Friction | Sensory fatigue of a normal timer
- 0:07-0:12 | Reveal
```

```assets
- [ ] video: close-up of a hand turning a rotary dial on an iPhone, soft daylight
  - name: Rotary dial close-up
  - reuse: 40
  - model: kling-2.5-turbo-pro
- [x] image: app icon @"App Store/app-icon.png"
  - name: App icon
- [~] image: blinking kitchen timer on a counter
  - name: Kitchen timer
  - reuse: 70
- [~] image: timer still @"Generated images/timer.png"
  - name: Timer still
- [~] image: timer still @"Generated images/timer2.png" <!-- REGENERATE: warmer light -->
  - name: Warm timer still
```
"#;

    fn v2_opts() -> ValidateOpts {
        ValidateOpts {
            audience_videos: Some(1),
            max_trend_videos: Some(0),
            require_caption: true,
            require_title: true,
            require_names: true,
            ..Default::default()
        }
    }

    #[test]
    fn parses_v2_title_summary_flow_and_checklist() {
        let p = parse(SAMPLE_V2);
        assert!(p.issues.is_empty(), "{:?}", p.issues);
        assert_eq!(
            p.title.as_deref(),
            Some("Tactile ergonomics vs digital clutter")
        );
        assert_eq!(
            p.summary,
            "Contrast timer fatigue with the rotary dial; test whether tactile beats yesterday's widget reel."
        );
        let v = &p.videos[0];
        assert_eq!(
            v.flow_steps[0],
            FlowStep {
                label: "Hook: Digital noise".into(),
                description: "Loud beeps and blinking counters".into(),
                start_ms: Some(0),
                end_ms: Some(3000),
            }
        );
        assert_eq!(v.flow_steps[2].label, "Reveal");
        assert_eq!(v.flow_steps[2].description, "");
        assert_eq!(v.flow_steps[2].end_ms, Some(12_000));

        let c = &v.checklist;
        assert_eq!(c.len(), 5);
        assert_eq!(c[0].name, "Rotary dial close-up");
        assert!(c[0].name_declared);
        assert_eq!(c[0].model.as_deref(), Some("kling-2.5-turbo-pro"));
        assert_eq!(c[0].reuse, Some(40));
        assert_eq!(c[0].status, ChecklistStatus::Open);
        assert_eq!(c[1].status, ChecklistStatus::Confirmed);
        assert_eq!(c[2].status, ChecklistStatus::Queued);
        assert_eq!(c[2].reuse, Some(70));
        assert_eq!(c[3].status, ChecklistStatus::Generated);
        assert_eq!(c[4].status, ChecklistStatus::RegenerateQueued);
        assert_eq!(c[4].regenerate_note.as_deref(), Some("warmer light"));
        assert_eq!(c[3].regenerate_note, None);

        let issues = validate(&p, &v2_opts());
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn legacy_plans_still_parse_with_fallback_names() {
        let p = parse(SAMPLE);
        assert!(p.issues.is_empty(), "{:?}", p.issues);
        assert_eq!(p.title.as_deref(), Some("Plan for 2026-10-16"));
        assert_eq!(p.summary, "");
        let v = &p.videos[0];
        let labels: Vec<(&str, &str)> = v
            .flow_steps
            .iter()
            .map(|s| (s.label.as_str(), s.description.as_str()))
            .collect();
        assert_eq!(
            labels,
            vec![
                ("Hook", "Girl scrolling"),
                ("Problem", "12h screen time"),
                ("Reveal", "35m screen time"),
            ]
        );
        assert!(v.flow_steps.iter().all(|s| s.start_ms.is_none()));
        let bare: Vec<&str> = p.videos[1]
            .flow_steps
            .iter()
            .map(|s| s.label.as_str())
            .collect();
        assert_eq!(bare, vec!["A", "B"]);

        assert_eq!(v.checklist[0].name, "app icon");
        assert_eq!(v.checklist[1].name, "girl at desk scrolling, one static");
        assert!(!v.checklist[1].name_declared);
        assert_eq!(v.checklist[2].status, ChecklistStatus::Queued);
        assert_eq!(
            fallback_checklist_name(ChecklistKind::Image, r#"@"App Store/app-icon.png""#),
            "app-icon"
        );
        assert_eq!(fallback_checklist_name(ChecklistKind::Audio, "  "), "Audio");

        // Legacy flows skip timing checks; only PlanDay requires names and a summary.
        assert!(validate(&p, &ValidateOpts::default()).is_empty());
        let strict = validate(&p, &v2_opts());
        assert!(
            strict.iter().any(|i| i.message.contains("- name:")),
            "{strict:?}"
        );
        assert!(
            strict.iter().any(|i| i.message.contains("summary")),
            "{strict:?}"
        );
    }

    fn with_flow(flow: &str) -> ParsedPlan {
        parse(&SAMPLE_V2.replace(
            "- 0:00-0:03 | Hook: Digital noise | Loud beeps and blinking counters\n\
             - 0:03-0:07 | Friction | Sensory fatigue of a normal timer\n\
             - 0:07-0:12 | Reveal",
            flow,
        ))
    }

    fn flow_messages(p: &ParsedPlan) -> Vec<String> {
        validate(p, &ValidateOpts::default())
            .into_iter()
            .map(|i| i.message)
            .collect()
    }

    #[test]
    fn timed_flow_rules() {
        let gap = with_flow("- 0:00-0:03 | Hook | a\n- 0:04-0:10 | Reveal | b");
        assert!(
            flow_messages(&gap)
                .iter()
                .any(|m| m.contains("contiguous") && m.contains("0:03")),
            "{:?}",
            flow_messages(&gap)
        );
        let late = with_flow("- 0:01-0:05 | Hook | a\n- 0:05-0:10 | Reveal | b");
        assert!(
            flow_messages(&late)
                .iter()
                .any(|m| m.contains("contiguous"))
        );
        let short = with_flow("- 0:00-0:03 | Hook | a\n- 0:03-0:06 | Reveal | b");
        assert!(flow_messages(&short).iter().any(|m| m.contains("8-25")));
        let long = with_flow("- 0:00-0:10 | Hook | a\n- 0:10-0:26 | Reveal | b");
        assert!(flow_messages(&long).iter().any(|m| m.contains("8-25")));
        let one = with_flow("- 0:00-0:10 | Hook | a");
        assert!(flow_messages(&one).iter().any(|m| m.contains("2-8")));
        let nine = (0..9)
            .map(|i| format!("- 0:{:02}-0:{:02} | S{i} | x", i * 2, i * 2 + 2))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            flow_messages(&with_flow(&nine))
                .iter()
                .any(|m| m.contains("2-8"))
        );
        let backwards =
            with_flow("- 0:00-0:05 | Hook | a\n- 0:05-0:05 | Reveal | b\n- 0:05-0:10 | End | c");
        assert!(
            flow_messages(&backwards)
                .iter()
                .any(|m| m.contains("must end after"))
        );
        let mixed = with_flow("- 0:00-0:05 | Hook | a\nReveal[b]\n- 0:05-0:10 | End | c");
        assert!(
            mixed
                .issues
                .iter()
                .any(|i| i.message.contains("flow line must look like")),
            "{:?}",
            mixed.issues
        );
        let bad_secs = with_flow("- 0:00-0:75 | Hook | a\n- 0:05-0:10 | End | c");
        assert!(bad_secs.issues.iter().any(|i| i.message.contains("00-59")));
        let ok = with_flow("- 0:00-0:04 | Hook | a\n- 0:04-0:25 | End | c");
        assert!(flow_messages(&ok).is_empty(), "{:?}", flow_messages(&ok));
    }

    #[test]
    fn rejects_bad_names_models_and_long_summary() {
        let long_name = "x".repeat(61);
        let md = SAMPLE_V2
            .replace("  - name: Kitchen timer", &format!("  - name: {long_name}"))
            .replace("kling-2.5-turbo-pro", "Kling 2.5!");
        let p = parse(&md);
        let msgs: Vec<&str> = p.issues.iter().map(|i| i.message.as_str()).collect();
        assert!(msgs.iter().any(|m| m.contains("at most 60")), "{msgs:?}");
        assert!(
            msgs.iter().any(|m| m.contains("model must match")),
            "{msgs:?}"
        );

        let summary = "word ".repeat(70);
        let md = SAMPLE_V2.replace(
            "Contrast timer fatigue with the rotary dial; test whether tactile beats\nyesterday's widget reel.",
            &summary,
        );
        let issues = validate(&parse(&md), &ValidateOpts::default());
        assert!(
            issues.iter().any(|i| i.message.contains("280")),
            "{issues:?}"
        );

        let no_title = SAMPLE_V2.replace("# Tactile ergonomics vs digital clutter\n", "");
        let issues = validate(&parse(&no_title), &v2_opts());
        assert!(
            issues.iter().any(|i| i.message.contains("# <day title>")),
            "{issues:?}"
        );
    }

    #[test]
    fn name_and_model_apply_to_any_kind() {
        let p = parse(SAMPLE_V2);
        let icon = &p.videos[0].checklist[1];
        assert_eq!(icon.kind, ChecklistKind::Image);
        assert_eq!(icon.name, "App icon");
        assert!(icon.generation.is_none());
    }

    #[test]
    fn edits_model_and_regenerate_note() {
        let item = |md: &str, key: &str| {
            parse(md)
                .videos
                .iter()
                .flat_map(|v| v.checklist.clone())
                .find(|c| c.key == key)
                .unwrap()
        };

        // Insert a model after the existing directives, keeping the state.
        let md = edit_checklist_item(
            SAMPLE_V2,
            "video-1#3",
            &ChecklistEdit {
                model: Some(Some("fal-ai/flux/dev")),
                ..Default::default()
            },
        )
        .unwrap();
        let c = item(&md, "video-1#3");
        assert_eq!(c.model.as_deref(), Some("fal-ai/flux/dev"));
        assert_eq!(c.state, ChecklistState::Pending);
        assert_eq!(c.reuse, Some(70));
        assert!(md.contains("  - reuse: 70\n  - model: fal-ai/flux/dev\n"));
        assert_eq!(item(&md, "video-1#4").name, "Timer still");

        // Replace, then clear, an existing model.
        let md = edit_checklist_item(
            SAMPLE_V2,
            "video-1#1",
            &ChecklistEdit {
                model: Some(Some("veo-3")),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(item(&md, "video-1#1").model.as_deref(), Some("veo-3"));
        assert_eq!(md.matches("- model:").count(), 1);
        let md = edit_checklist_item(
            &md,
            "video-1#1",
            &ChecklistEdit {
                model: Some(None),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(item(&md, "video-1#1").model, None);
        assert!(!md.contains("- model:"));
        assert_eq!(item(&md, "video-1#1").name, "Rotary dial close-up");

        // Set and clear a regenerate note.
        let md = edit_checklist_item(
            SAMPLE_V2,
            "video-1#4",
            &ChecklistEdit {
                regenerate_note: Some(Some("less blue --> more warm")),
                ..Default::default()
            },
        )
        .unwrap();
        let c = item(&md, "video-1#4");
        assert_eq!(c.status, ChecklistStatus::RegenerateQueued);
        assert_eq!(c.regenerate_note.as_deref(), Some("less blue -> more warm"));
        assert_eq!(c.refs, vec!["Generated images/timer.png"]);
        let md = edit_checklist_item(
            &md,
            "video-1#4",
            &ChecklistEdit {
                regenerate_note: Some(None),
                ..Default::default()
            },
        )
        .unwrap();
        let c = item(&md, "video-1#4");
        assert_eq!(c.status, ChecklistStatus::Generated);
        assert_eq!(c.regenerate_note, None);

        // Attaching a new render clears the regenerate note.
        let md = set_checklist_state(
            SAMPLE_V2,
            "video-1#5",
            ChecklistState::Pending,
            Some("Generated images/timer3.png"),
        )
        .unwrap();
        let c = item(&md, "video-1#5");
        assert_eq!(c.status, ChecklistStatus::Generated);
        assert_eq!(c.refs, vec!["Generated images/timer3.png"]);

        assert_eq!(
            edit_checklist_item(SAMPLE_V2, "video-9#1", &ChecklistEdit::default()),
            Err(ChecklistEditError::UnknownKey)
        );
        assert!(matches!(
            edit_checklist_item(
                SAMPLE_V2,
                "video-1#1",
                &ChecklistEdit {
                    model: Some(Some("Bad Model")),
                    ..Default::default()
                },
            ),
            Err(ChecklistEditError::Invalid(_))
        ));
    }

    #[test]
    fn checklist_state_accepts_status_names() {
        assert_eq!(
            ChecklistState::parse("queued"),
            Some(ChecklistState::Pending)
        );
        assert_eq!(
            ChecklistState::parse("confirmed"),
            Some(ChecklistState::Done)
        );
        assert_eq!(ChecklistState::parse("nope"), None);
    }
}
