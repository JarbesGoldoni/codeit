//! `codeit.json`: the user's settings, in the same shape as opencode's `opencode.json` for the
//! keys codeit supports, so sections can be copied between the two.
//!
//! Read from `~/.config/codeit/codeit.json`, then every `codeit.json` and `.codeit/codeit.json` from the
//! project root down to the working directory; later files win. JSON with comments is accepted,
//! and `{env:NAME}` in a string is replaced by that environment variable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Default model, `provider/model`.
    pub model: Option<String>,
    /// Model for helper calls: condensing long tool output and summarizing the conversation
    /// (opencode's key). Default: the session's model.
    pub small_model: Option<String>,
    /// opencode-style permissions: `{"edit": "ask", "bash": {"*": "ask", "git status*": "allow"}}`.
    pub permission: Option<Value>,
    /// Extra instruction files (paths, globs or URLs) added to the system prompt.
    pub instructions: Vec<String>,
    pub mcp: BTreeMap<String, McpConfig>,
    pub command: BTreeMap<String, CommandConfig>,
    pub agent: BTreeMap<String, AgentConfig>,
    pub skills: SkillsConfig,
    pub compaction: CompactionConfig,
    pub tool_output: ToolOutputConfig,
    /// Shell for the bash tool (default: $SHELL if it is bash or zsh, else /bin/bash).
    pub shell: Option<String>,
    /// Language servers, in opencode's shape; `false` turns them off (see `lsp`).
    pub lsp: Option<Value>,
    pub tui: TuiConfig,
    /// The agent new sessions start with (opencode's key; default `build`).
    pub default_agent: Option<String>,
    /// `auto` or `ask` for new sessions (default: the last one you picked with /approvals).
    pub approval: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct SkillsConfig {
    pub paths: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct CompactionConfig {
    /// Summarize the conversation when it nears the model's context limit (default true).
    pub auto: Option<bool>,
    /// Remove old tool outputs from the context at the end of a turn (default true).
    pub prune: Option<bool>,
    /// Tokens kept free below the context limit (default 20000).
    pub reserved: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct TuiConfig {
    /// Scroll with the mouse wheel (default true). Selecting text then needs Shift+drag.
    pub mouse: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ToolOutputConfig {
    pub max_lines: Option<usize>,
    pub max_bytes: Option<usize>,
    /// Have the small model condense long command, fetch and MCP output (default true).
    pub condense: Option<bool>,
    /// Outputs at least this many tokens long are condensed (default 2000).
    pub condense_min_tokens: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpConfig {
    Local {
        command: Vec<String>,
        #[serde(default)]
        environment: BTreeMap<String, String>,
        #[serde(default = "yes")]
        enabled: bool,
        #[serde(default)]
        timeout: Option<u64>,
    },
    Remote {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
        #[serde(default = "yes")]
        enabled: bool,
        #[serde(default)]
        timeout: Option<u64>,
    },
}

impl McpConfig {
    pub fn enabled(&self) -> bool {
        match self {
            McpConfig::Local { enabled, .. } | McpConfig::Remote { enabled, .. } => *enabled,
        }
    }
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct CommandConfig {
    pub template: String,
    pub description: Option<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub subtask: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    pub description: Option<String>,
    /// `primary`, `subagent` or `all`.
    pub mode: Option<String>,
    pub model: Option<String>,
    pub prompt: Option<String>,
    /// `{"bash": false}` turns tools off for this agent.
    pub tools: BTreeMap<String, bool>,
    pub permission: Option<Value>,
    pub steps: Option<usize>,
    pub disable: bool,
}

/// `$XDG_CONFIG_HOME/codeit`, default `~/.config/codeit`.
pub fn config_dir() -> PathBuf {
    xdg_config().join("codeit")
}

pub(crate) fn xdg_config() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => dirs::home_dir().unwrap_or_default().join(".config"),
    }
}

impl Config {
    /// Loads and merges every config file that applies to `cwd` (whose project root is `root`).
    /// Returns the config and the problems found, which the caller should show.
    pub fn load(cwd: &Path, root: &Path) -> (Config, Vec<String>) {
        let mut files = vec![config_dir().join("codeit.json"), config_dir().join("codeit.jsonc")];
        for dir in dirs_between(root, cwd) {
            files.push(dir.join("codeit.json"));
            files.push(dir.join(".codeit").join("codeit.json"));
        }
        let mut merged = Value::Object(Default::default());
        let mut problems = Vec::new();
        for f in files {
            let Ok(text) = std::fs::read_to_string(&f) else { continue };
            match serde_json::from_str::<Value>(&strip_comments(&text)) {
                Ok(v) => merge(&mut merged, v),
                Err(e) => problems.push(format!("{}: {e}", f.display())),
            }
        }
        substitute_env(&mut merged);
        match serde_json::from_value(merged) {
            Ok(c) => (c, problems),
            Err(e) => {
                problems.push(format!("config: {e}"));
                (Config::default(), problems)
            }
        }
    }
}

/// `root`, then each directory below it down to `cwd` (just `cwd` if it is not under `root`).
pub(crate) fn dirs_between(root: &Path, cwd: &Path) -> Vec<PathBuf> {
    let Ok(rel) = cwd.strip_prefix(root) else { return vec![cwd.to_path_buf()] };
    let mut out = vec![root.to_path_buf()];
    let mut cur = root.to_path_buf();
    for c in rel.components() {
        cur.push(c);
        out.push(cur.clone());
    }
    out
}

/// Objects merge key by key, arrays concatenate without duplicates, anything else is replaced.
fn merge(into: &mut Value, from: Value) {
    match (into, from) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                match a.get_mut(&k) {
                    Some(slot) => merge(slot, v),
                    None => {
                        a.insert(k, v);
                    }
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            for v in b {
                if !a.contains(&v) {
                    a.push(v);
                }
            }
        }
        (slot, v) => *slot = v,
    }
}

fn substitute_env(v: &mut Value) {
    match v {
        Value::String(s) if s.contains("{env:") => {
            let mut out = String::new();
            let mut rest = s.as_str();
            while let Some(i) = rest.find("{env:") {
                out.push_str(&rest[..i]);
                match rest[i..].find('}') {
                    Some(j) => {
                        out.push_str(&std::env::var(&rest[i + 5..i + j]).unwrap_or_default());
                        rest = &rest[i + j + 1..];
                    }
                    None => {
                        out.push_str(&rest[i..]);
                        rest = "";
                    }
                }
            }
            out.push_str(rest);
            *s = out;
        }
        Value::Array(a) => a.iter_mut().for_each(substitute_env),
        Value::Object(o) => o.values_mut().for_each(substitute_env),
        _ => {}
    }
}

/// Removes `//` and `/* */` comments and trailing commas outside strings (JSONC).
pub(crate) fn strip_comments(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c as char);
            if c == b'\\' && i + 1 < b.len() {
                out.push(b[i + 1] as char);
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => {
                in_str = true;
                out.push('"');
                i += 1;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            b',' => {
                // Drop a trailing comma before } or ].
                let next = b[i + 1..].iter().find(|c| !c.is_ascii_whitespace());
                if !matches!(next, Some(b'}') | Some(b']')) {
                    out.push(',');
                }
                i += 1;
            }
            _ => {
                // Copy whole UTF-8 sequences.
                let len = utf8_len(c);
                out.push_str(&text[i..(i + len).min(text.len())]);
                i += len;
            }
        }
    }
    out
}

fn utf8_len(first: u8) -> usize {
    match first {
        0..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strips_jsonc() {
        let t = "{\n // c\n \"a\": \"x // not a comment\", /* b */ \"b\": [1, 2,],\n}";
        let v: Value = serde_json::from_str(&strip_comments(t)).unwrap();
        assert_eq!(v, json!({"a": "x // not a comment", "b": [1, 2]}));
    }

    #[test]
    fn merges_and_substitutes() {
        let mut a = json!({"instructions": ["a"], "permission": {"edit": "ask"}});
        merge(&mut a, json!({"instructions": ["b", "a"], "permission": {"bash": "deny"}}));
        assert_eq!(a, json!({"instructions": ["a", "b"], "permission": {"edit": "ask", "bash": "deny"}}));
        unsafe { std::env::set_var("CODEIT_TEST_VAR", "v") };
        let mut v = json!({"h": "Basic {env:CODEIT_TEST_VAR}!"});
        substitute_env(&mut v);
        assert_eq!(v["h"], "Basic v!");
    }

    #[test]
    fn parses_opencode_shapes() {
        let c: Config = serde_json::from_value(json!({
            "mcp": {
                "tracker": {"type": "remote", "url": "https://x", "headers": {"Authorization": "Basic y"}},
                "fs": {"type": "local", "command": ["npx", "srv"], "enabled": false}
            },
            "agent": {"review": {"description": "d", "tools": {"bash": false}}},
            "command": {"hi": {"template": "say hi $ARGUMENTS"}}
        }))
        .unwrap();
        assert!(c.mcp["tracker"].enabled());
        assert!(!c.mcp["fs"].enabled());
        assert!(!c.agent["review"].tools["bash"]);
    }
}
