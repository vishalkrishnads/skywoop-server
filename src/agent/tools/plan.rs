//! Planning tools: asset search / browsing and the creative playbook.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::{AgentCtx, AgentTool};
use crate::ai::schema::strict_schema;
use crate::domain::assets::{self, Asset, AssetKind};
use crate::domain::text_template;
use crate::playbook_doc::{self, PlaybookPart};

fn args_from<T: for<'de> Deserialize<'de>>(v: Value) -> Result<T, String> {
    serde_json::from_value(v).map_err(|e| e.to_string())
}

fn kind_filter(kind: &Option<String>) -> Result<Option<AssetKind>, String> {
    match kind.as_deref() {
        None | Some("") | Some("any") => Ok(None),
        Some(k @ ("image" | "video" | "audio")) => Ok(AssetKind::parse(k)),
        Some(other) => Err(format!("kind must be image, video or audio, got {other}")),
    }
}

/// Compact asset view for the model (never includes URLs or hashes).
fn asset_json(a: &Asset) -> Value {
    json!({
        "path": a.path,
        "reference": format!("@\"{}\"", a.path),
        "kind": a.kind,
        "description": a.description,
        "tags": a.tags,
        "width": a.width,
        "height": a.height,
        "duration_ms": a.duration_ms,
    })
}

struct SearchAssets;

#[derive(Deserialize, JsonSchema)]
struct SearchAssetsArgs {
    /// Free-text query matched against paths, names, descriptions and tags.
    query: String,
    /// image | video | audio (omit for any).
    #[serde(default)]
    kind: Option<String>,
    /// Restrict to a directory path, e.g. "Screen recordings".
    #[serde(default)]
    dir: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

#[async_trait]
impl AgentTool for SearchAssets {
    fn name(&self) -> &'static str {
        "search_assets"
    }

    fn description(&self) -> &'static str {
        "Search the project's asset manager (images, screen recordings, audio) by meaning/text. \
         Returns matching files with descriptions; cite them in plans as @\"path\"."
    }

    fn parameters_schema(&self) -> Value {
        strict_schema::<SearchAssetsArgs>()
    }

    async fn call(&self, ctx: &AgentCtx, args: Value) -> Result<Value, String> {
        let a: SearchAssetsArgs = args_from(args)?;
        let kind = kind_filter(&a.kind)?;
        let res = assets::search(
            &ctx.db,
            ctx.project_id,
            &a.query,
            kind,
            a.dir.as_deref(),
            a.limit.unwrap_or(15).clamp(1, 50),
        )
        .await
        .map_err(|e| e.to_string())?;
        Ok(json!({
            "assets": res.assets.iter().map(asset_json).collect::<Vec<_>>(),
            "dirs": res.dirs.iter().map(|d| json!({
                "path": d.path,
                "description": d.description,
                "file_count": d.file_count,
            })).collect::<Vec<_>>(),
        }))
    }
}

struct ListAssetDir;

#[derive(Deserialize, JsonSchema)]
struct ListAssetDirArgs {
    /// Directory path ("" or "/" for the root).
    path: String,
    #[serde(default)]
    kind: Option<String>,
}

#[async_trait]
impl AgentTool for ListAssetDir {
    fn name(&self) -> &'static str {
        "list_asset_dir"
    }

    fn description(&self) -> &'static str {
        "List the subdirectories (with descriptions) and files of one asset directory."
    }

    fn parameters_schema(&self) -> Value {
        strict_schema::<ListAssetDirArgs>()
    }

    async fn call(&self, ctx: &AgentCtx, args: Value) -> Result<Value, String> {
        let a: ListAssetDirArgs = args_from(args)?;
        let kind = kind_filter(&a.kind)?;
        let path = a.path.trim().trim_matches('/').to_owned();
        let (dirs, files) = assets::list_children(&ctx.db, ctx.project_id, &path, kind)
            .await
            .map_err(|e| e.to_string())?;
        Ok(json!({
            "path": path,
            "dirs": dirs.iter().map(|d| json!({
                "path": d.path,
                "description": d.description,
                "file_count": d.file_count,
                "dir_count": d.dir_count,
            })).collect::<Vec<_>>(),
            "files": files.iter().map(asset_json).collect::<Vec<_>>(),
        }))
    }
}

struct ReadPlaybook;

#[derive(Deserialize, JsonSchema)]
struct ReadPlaybookArgs {
    /// outline (default) | section | search | all
    #[serde(default)]
    part: Option<String>,
    /// Heading title for part=section (case-insensitive match).
    #[serde(default)]
    heading: Option<String>,
    /// Free-text query for part=search.
    #[serde(default)]
    query: Option<String>,
}

#[async_trait]
impl AgentTool for ReadPlaybook {
    fn name(&self) -> &'static str {
        "read_playbook"
    }

    fn description(&self) -> &'static str {
        "Read the Skywoop creative playbook. Prefer part=outline first, then part=section \
         with a heading, or part=search with a query. Use part=all only when you need the \
         whole document."
    }

    fn parameters_schema(&self) -> Value {
        strict_schema::<ReadPlaybookArgs>()
    }

    async fn call(&self, _ctx: &AgentCtx, args: Value) -> Result<Value, String> {
        let a: ReadPlaybookArgs = args_from(args)?;
        let part = match a.part.as_deref() {
            None | Some("") => PlaybookPart::Outline,
            Some(s) => PlaybookPart::parse(s)
                .ok_or_else(|| format!("part must be outline, section, search, or all; got {s}"))?,
        };
        let text = playbook_doc::read(part, a.heading.as_deref(), a.query.as_deref())?;
        Ok(json!({ "text": text }))
    }
}

struct SearchTemplates;

#[derive(Deserialize, JsonSchema)]
struct SearchTemplatesArgs {
    /// Optional match against name and description. Omit or null to list every template.
    #[serde(default)]
    query: Option<String>,
}

#[async_trait]
impl AgentTool for SearchTemplates {
    fn name(&self) -> &'static str {
        "search_templates"
    }

    fn description(&self) -> &'static str {
        "Search the text template catalog by name or description. An empty query returns the full catalog, including element names and boxes. Use a template id when writing on-screen copy."
    }

    fn parameters_schema(&self) -> Value {
        strict_schema::<SearchTemplatesArgs>()
    }

    async fn call(&self, ctx: &AgentCtx, args: Value) -> Result<Value, String> {
        let a: SearchTemplatesArgs = args_from(args)?;
        let templates = text_template::search(&ctx.db, a.query.as_deref())
            .await
            .map_err(|e| e.to_string())?;
        Ok(json!({ "templates": templates }))
    }
}

/// Tools PlanDay uses on top of the analytics tools.
pub fn plan_tools() -> Vec<Box<dyn AgentTool>> {
    vec![
        Box::new(SearchAssets),
        Box::new(ListAssetDir),
        Box::new(ReadPlaybook),
        Box::new(SearchTemplates),
    ]
}
