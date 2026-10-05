//! Agents: a prompt, a set of permissions and a mode.
//!
//! - `build` (default) works with every tool.
//! - `plan` is read-only: no edits except plan files under `.codeit/plans/`.
//! - `general` is a subagent for delegated multi-step work.
//! - `explore` is a fast read-only subagent for searching the codebase.
//!
//! More come from the `agent` config key and `.codeit/agents/*.md` (frontmatter: description,
//! mode, model, steps; body: the prompt), as in opencode.

use std::path::PathBuf;

use serde_json::json;

use crate::config::{AgentConfig, Config};
use crate::permission::Ruleset;
use crate::util;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Primary,
    Subagent,
    All,
}

#[derive(Clone, Debug)]
pub struct Agent {
    pub name: String,
    pub description: String,
    pub mode: Mode,
    /// Replaces the base system prompt when set.
    pub prompt: Option<String>,
    pub model: Option<String>,
    /// Most model requests in one turn.
    pub steps: Option<usize>,
    /// Rules on top of the defaults and the user's config.
    pub permission: Ruleset,
    /// Run by codeit itself (reviews), not offered with Tab or to the task tool.
    pub hidden: bool,
}

impl Agent {
    pub fn primary(&self) -> bool {
        self.mode != Mode::Subagent
    }
    pub fn subagent(&self) -> bool {
        self.mode != Mode::Primary
    }
}

fn builtin() -> Vec<Agent> {
    vec![
        Agent {
            name: "build".into(),
            description: "The default agent: reads, edits and runs commands.".into(),
            mode: Mode::Primary,
            prompt: None,
            model: None,
            steps: None,
            permission: Ruleset::from_config(&json!({ "task": "allow" })),
            hidden: false,
        },
        Agent {
            name: "plan".into(),
            description: "Read-only planning: investigates and proposes a plan without changing anything.".into(),
            mode: Mode::Primary,
            prompt: None,
            model: None,
            steps: None,
            permission: Ruleset::from_config(&json!({
                "edit": { "*": "deny", ".codeit/plans/*.md": "allow", "*/.codeit/plans/*.md": "allow" },
                "task": { "*": "allow", "general": "deny" },
            })),
            hidden: false,
        },
        Agent {
            name: "general".into(),
            description: "General-purpose agent for multi-step tasks and research. Use it to run independent pieces of work in parallel.".into(),
            mode: Mode::Subagent,
            prompt: Some(include_str!("prompts/general.md").into()),
            model: None,
            steps: None,
            permission: Ruleset::from_config(&json!({ "todo": "deny", "task": "deny" })),
            hidden: false,
        },
        Agent {
            name: "explore".into(),
            description: "Fast read-only agent for exploring codebases: finding files by pattern, searching code, answering questions about how the code works. Say how thorough it should be: quick, medium or very thorough.".into(),
            mode: Mode::Subagent,
            prompt: Some(include_str!("prompts/explore.md").into()),
            model: None,
            steps: None,
            permission: Ruleset::from_config(&json!({
                "*": "deny", "read": "allow", "grep": "allow", "glob": "allow", "bash": "allow",
                "webfetch": "allow", "websearch": "allow", "skill": "allow", "edit": "deny",
            })),
            hidden: false,
        },
        Agent {
            name: "review".into(),
            description: "Reviews a diff and leaves comments on its lines.".into(),
            mode: Mode::Primary,
            prompt: None,
            model: None,
            steps: None,
            permission: Ruleset::from_config(&json!({
                "edit": "deny", "todo": "deny", "question": "deny", "review_comment": "allow",
                "task": { "*": "deny", "explore": "allow" },
            })),
            hidden: true,
        },
    ]
}

fn from_config(name: &str, c: &AgentConfig, base: Option<&Agent>) -> Agent {
    let mut a = base.cloned().unwrap_or(Agent {
        name: name.into(),
        description: String::new(),
        mode: Mode::All,
        prompt: None,
        model: None,
        steps: None,
        permission: Ruleset::default(),
        hidden: false,
    });
    if let Some(d) = &c.description {
        a.description = d.clone();
    }
    a.mode = match c.mode.as_deref() {
        Some("primary") => Mode::Primary,
        Some("subagent") => Mode::Subagent,
        Some("all") => Mode::All,
        _ => a.mode,
    };
    if c.prompt.is_some() {
        a.prompt = c.prompt.clone();
    }
    if c.model.is_some() {
        a.model = c.model.clone();
    }
    if c.steps.is_some() {
        a.steps = c.steps;
    }
    for (tool, on) in &c.tools {
        a.permission.push(
            tool,
            "*",
            if *on { crate::permission::Action::Allow } else { crate::permission::Action::Deny },
        );
    }
    if let Some(p) = &c.permission {
        a.permission.extend(&Ruleset::from_config(p));
    }
    a
}

/// Agents defined in Markdown files: `name.md` with frontmatter and the prompt as body.
fn from_dirs(project_dirs: &[PathBuf]) -> Vec<(String, AgentConfig)> {
    let xdg = crate::config::xdg_config();
    let mut dirs = vec![xdg.join("opencode/agent"), xdg.join("opencode/agents"), xdg.join("codeit/agents")];
    for d in project_dirs {
        for sub in [".opencode/agent", ".opencode/agents", ".codeit/agents"] {
            dirs.push(d.join(sub));
        }
    }
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(read) = std::fs::read_dir(&dir) else { continue };
        let mut files: Vec<PathBuf> =
            read.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "md")).collect();
        files.sort();
        for p in files {
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            let (f, body) = util::frontmatter(&text);
            let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let c = AgentConfig {
                description: f.get("description").cloned(),
                mode: f.get("mode").cloned(),
                model: f.get("model").cloned(),
                prompt: (!body.trim().is_empty()).then_some(body),
                steps: f.get("steps").and_then(|s| s.parse().ok()),
                disable: f.get("disable").is_some_and(|v| v == "true"),
                ..Default::default()
            };
            out.push((name, c));
        }
    }
    out
}

pub fn load(project_dirs: &[PathBuf], config: &Config) -> Vec<Agent> {
    let mut agents = builtin();
    let mut defs = from_dirs(project_dirs);
    defs.extend(config.agent.iter().map(|(k, v)| (k.clone(), v.clone())));
    for (name, c) in defs {
        let existing = agents.iter().position(|a| a.name == name);
        if c.disable {
            if let Some(i) = existing {
                agents.remove(i);
            }
            continue;
        }
        let agent = from_config(&name, &c, existing.map(|i| &agents[i]));
        match existing {
            Some(i) => agents[i] = agent,
            None => agents.push(agent),
        }
    }
    agents
}
