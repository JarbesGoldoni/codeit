//! What codeit remembers between runs: the last model, effort and approval mode.

use codeit_harness::session::Approval;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct State {
    /// `provider/model`.
    pub model: Option<String>,
    pub effort: Option<String>,
    #[serde(default)]
    pub approval: Approval,
    /// How much of the agent's actions to show (ctrl+o): 0 folded, 1 list, 2 open.
    #[serde(default = "list")]
    pub actions: usize,
    /// Model for the plan agent and code reviews (default: `model`).
    #[serde(default)]
    pub think: Option<String>,
}

fn list() -> usize {
    1
}

fn path() -> std::path::PathBuf {
    codeit_providers::data_dir().join("state.json")
}

impl Default for State {
    fn default() -> Self {
        Self { model: None, effort: None, approval: Approval::default(), actions: 1, think: None }
    }
}

impl State {
    pub fn load() -> Self {
        std::fs::read_to_string(path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let p = path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(p, text);
        }
    }
}
