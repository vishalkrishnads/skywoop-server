use minijinja::Environment;
use serde::Serialize;

use super::client::{AiError, RenderedPrompt};

const SPLIT: &str = "<<<SKYWOOP_SPLIT>>>";

const LAYOUT: &str = r#"{% block system %}{% endblock %}

Never use an em dash or an en dash anywhere in the text you write. Use a hyphen, comma, period, or colon.
<<<SKYWOOP_SPLIT>>>
{% block user %}{% endblock %}"#;

macro_rules! templates {
    ($($name:literal => $path:literal),* $(,)?) => {
        pub fn source(name: &str) -> Option<&'static str> {
            match name {
                $($name => Some(include_str!($path)),)*
                _ => None,
            }
        }
    };
}

templates! {
    "describe_asset" => "../../prompts/describe_asset.j2",
    "analyze_experiment" => "../../prompts/analyze_experiment.j2",
    "analyze_platform" => "../../prompts/analyze_platform.j2",
    "classify_industry" => "../../prompts/classify_industry.j2",
    "adjust_post_timing" => "../../prompts/adjust_post_timing.j2",
    "plan_day" => "../../prompts/plan_day.j2",
    "update_diary" => "../../prompts/update_diary.j2",
}

pub fn render(name: &str, ctx: &impl Serialize) -> Result<RenderedPrompt, AiError> {
    let source = source(name)
        .ok_or_else(|| AiError::Other(anyhow::anyhow!("unknown prompt template: {name}")))?;
    let version = parse_version(source);
    let mut env = Environment::new();
    env.add_template("_layout", LAYOUT)
        .map_err(|e| AiError::Other(e.into()))?;
    env.add_template(name, source)
        .map_err(|e| AiError::Other(e.into()))?;
    let tmpl = env
        .get_template(name)
        .map_err(|e| AiError::Other(e.into()))?;
    let rendered = tmpl.render(ctx).map_err(|e| AiError::Other(e.into()))?;
    let (system, user) = split_blocks(&rendered)?;
    Ok(RenderedPrompt {
        system,
        user,
        version,
    })
}

pub fn data_json(name: &str, value: &impl Serialize) -> Result<String, AiError> {
    let pretty = serde_json::to_string_pretty(value).map_err(|e| AiError::Other(e.into()))?;
    Ok(format!("<data name=\"{name}\">\n{pretty}\n</data>"))
}

fn parse_version(source: &str) -> String {
    source
        .lines()
        .next()
        .and_then(|line| {
            let line = line.trim();
            let rest = line.strip_prefix("{#")?;
            let rest = rest.strip_suffix("#}")?;
            let rest = rest.trim();
            let ver = rest.strip_prefix("version:")?.trim();
            if ver.is_empty() {
                None
            } else {
                Some(ver.to_owned())
            }
        })
        .unwrap_or_else(|| "0".into())
}

fn split_blocks(rendered: &str) -> Result<(String, String), AiError> {
    let (system, user) = rendered.split_once(SPLIT).ok_or_else(|| {
        AiError::Other(anyhow::anyhow!("prompt render missing system/user split"))
    })?;
    Ok((system.trim().to_owned(), user.trim().to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_line() {
        assert_eq!(parse_version("{# version: 3 #}\nrest"), "3");
        assert_eq!(parse_version("no version"), "0");
    }

    #[test]
    fn render_describe_asset_splits_system_user() {
        let p = render(
            "describe_asset",
            &serde_json::json!({ "context": "App icon" }),
        )
        .unwrap();
        assert_eq!(p.version, "1");
        assert!(p.system.contains("catalogue"));
        assert!(p.user.contains("App icon"));
    }

    #[test]
    fn render_plan_day_splits_system_user() {
        let p = render(
            "plan_day",
            &serde_json::json!({
                "project_name": "Demo",
                "project_description": "A demo app.",
                "audiences": "[]",
                "for_date": "2026-10-10",
                "analysis_date": "2026-10-08",
                "audience_videos": 5,
                "trend_videos": 5,
                "validation_errors": "",
            }),
        )
        .unwrap();
        assert!(!p.system.is_empty());
        assert!(p.user.contains("2026-10-10"));
        assert!(p.system.contains("read_playbook"));
        assert!(p.system.contains("voiceover"));
        assert!(p.system.contains("sound effects"));
        assert!(!p.user.contains("knowledge"));
        assert!(p.system.contains("- 0:00-0:03 |"));
        assert!(p.system.contains("- name:"));
        assert!(p.system.contains("280 characters"));
        assert!(p.system.contains("`no_op` MUST be null"));
        assert!(!p.user.contains("no_op_metrics"));
        assert_eq!(p.version, "6");
    }

    #[test]
    fn render_plan_day_offers_no_op_when_allowed() {
        let p = render(
            "plan_day",
            &serde_json::json!({
                "project_name": "Demo",
                "project_description": "A demo app.",
                "audiences": "[]",
                "for_date": "2026-10-10",
                "analysis_date": "2026-10-08",
                "audience_videos": 1,
                "trend_videos": 0,
                "validation_errors": "",
                "allow_no_op": true,
                "no_op_metrics": "{\"views_last_48h\": 4200}",
            }),
        )
        .unwrap();
        assert!(p.system.contains("recent_posts_growing"));
        assert!(!p.system.contains("`no_op` MUST be null"));
        assert!(p.user.contains("views_last_48h"));
    }
}
