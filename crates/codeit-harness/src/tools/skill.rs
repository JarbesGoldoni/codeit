//! skill: loads a skill's instructions into the conversation.

use std::path::Path;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use codeit_providers::ToolSpec;
use serde_json::{Value, json};

use super::{Ctx, Output, Tool, arg, spec};
use crate::Harness;

pub struct SkillTool;

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }

    fn spec(&self, _: &Harness) -> ToolSpec {
        spec(
            "skill",
            "Load a skill: its instructions for a kind of task, and the folder its scripts and references are in. \
Load one when the task matches a skill listed in the system prompt, before starting the work.",
            json!({
                "type": "object",
                "properties": { "name": { "type": "string", "description": "Skill name, as listed" } },
                "required": ["name"]
            }),
        )
    }

    fn read_only(&self) -> bool {
        true
    }

    fn title(&self, input: &Value, _: &Path) -> String {
        input["name"].as_str().unwrap_or_default().to_string()
    }

    async fn run(&self, input: Value, ctx: &Ctx) -> Result<Output> {
        let name = arg(&input, "name")?;
        let skills = ctx.harness.skills();
        let skill = skills.iter().find(|s| s.name == name).ok_or_else(|| {
            let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
            anyhow!(
                "No skill named `{name}`. Available: {}",
                if names.is_empty() { "none".into() } else { names.join(", ") }
            )
        })?;
        ctx.ask("skill", &[name.to_string()], &[name.to_string()], format!("load skill {name}"), None).await?;
        let display = Some(format!("Loaded skill {name}"));
        if skill.text.is_some() {
            let content = format!("<skill name=\"{name}\">\n{}\n</skill>", skill.body().trim());
            return Ok(Output { content, display, diff: None, ..Default::default() });
        }
        let dir = skill.dir();
        let mut files: Vec<String> = ignore::WalkBuilder::new(dir)
            .max_depth(Some(3))
            .build()
            .flatten()
            .filter(|e| e.file_type().is_some_and(|t| t.is_file()) && e.file_name() != "SKILL.md")
            .map(|e| e.path().strip_prefix(dir).unwrap_or(e.path()).to_string_lossy().into_owned())
            .collect();
        files.sort();
        files.truncate(20);
        let mut out = format!("<skill name=\"{name}\" dir=\"{}\">\n{}\n", dir.display(), skill.body().trim());
        if !files.is_empty() {
            out.push_str(&format!("\nFiles in the skill folder (paths are relative to it): {}\n", files.join(", ")));
        }
        out.push_str("</skill>");
        Ok(Output { content: out, display, diff: None, ..Default::default() })
    }
}
