//! Permissions: which tool calls run, which ask first, which are refused.
//!
//! A [`Ruleset`] is an ordered list of (permission, pattern, action) rules; the last rule that
//! matches wins, the same semantics as opencode. Permissions are tool names (`bash`, `read`,
//! `webfetch`, ...) plus `edit` (every file change), `external_directory` (paths outside the
//! project) and `doom_loop` (the same call repeated). Patterns are what the call touches: a path,
//! a command, a URL. `*` matches anything (including `/`), `?` one character, and a trailing ` *`
//! is optional, so `git status *` also matches `git status`.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Allow,
    Ask,
    Deny,
}

impl Action {
    fn parse(v: &Value) -> Option<Action> {
        match v.as_str()? {
            "allow" => Some(Action::Allow),
            "ask" => Some(Action::Ask),
            "deny" => Some(Action::Deny),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub permission: String,
    pub pattern: String,
    pub action: Action,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Ruleset(pub Vec<Rule>);

impl Ruleset {
    /// opencode's config shape: `"allow"`, or `{"*": "allow", "bash": {"*": "ask", "git *": "allow"}}`.
    pub fn from_config(v: &Value) -> Ruleset {
        let mut rules = Vec::new();
        let rule = |permission: &str, pattern: &str, action| Rule {
            permission: permission.into(),
            pattern: expand_home(pattern),
            action,
        };
        match v {
            Value::String(_) => {
                if let Some(a) = Action::parse(v) {
                    rules.push(rule("*", "*", a));
                }
            }
            Value::Object(map) => {
                for (permission, value) in map {
                    match value {
                        Value::Object(patterns) => {
                            for (pattern, action) in patterns {
                                if let Some(a) = Action::parse(action) {
                                    rules.push(rule(permission, pattern, a));
                                }
                            }
                        }
                        other => {
                            if let Some(a) = Action::parse(other) {
                                rules.push(rule(permission, "*", a));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        Ruleset(rules)
    }

    pub fn extend(&mut self, other: &Ruleset) {
        self.0.extend(other.0.iter().cloned());
    }

    pub fn push(&mut self, permission: &str, pattern: &str, action: Action) {
        self.0.push(Rule { permission: permission.into(), pattern: pattern.into(), action });
    }

    /// The action for one pattern: the last matching rule's, else ask.
    pub fn evaluate(&self, permission: &str, pattern: &str) -> Action {
        self.0
            .iter()
            .rev()
            .find(|r| wildcard(permission, &r.permission) && wildcard(pattern, &r.pattern))
            .map(|r| r.action)
            .unwrap_or(Action::Ask)
    }

    /// True when the permission is denied outright, so its tool is not offered to the model.
    pub fn disabled(&self, permission: &str) -> bool {
        self.0
            .iter()
            .rev()
            .find(|r| wildcard(permission, &r.permission))
            .is_some_and(|r| r.pattern == "*" && r.action == Action::Deny)
    }
}

fn expand_home(p: &str) -> String {
    match p.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().map(|h| h.join(rest).to_string_lossy().into_owned()).unwrap_or(p.into()),
        None => p.into(),
    }
}

/// Glob-style match: `*` any run of characters, `?` one character; a trailing ` *` is optional.
pub fn wildcard(text: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(head) = pattern.strip_suffix(" *")
        && glob(text.as_bytes(), head.as_bytes())
    {
        return true;
    }
    glob(text.as_bytes(), pattern.as_bytes())
}

fn glob(t: &[u8], p: &[u8]) -> bool {
    let (mut ti, mut pi) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == t[ti]) {
            ti += 1;
            pi += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// The rules every agent starts from, opencode's defaults: everything is allowed except paths
/// outside the project, repeated identical calls, and reading `.env` files, which ask first.
pub fn defaults(allowed_dirs: &[String]) -> Ruleset {
    let mut external = serde_json::Map::new();
    external.insert("*".into(), json!("ask"));
    for d in allowed_dirs {
        external.insert(format!("{}/*", d.trim_end_matches('/')), json!("allow"));
        external.insert(d.clone(), json!("allow"));
    }
    Ruleset::from_config(&json!({
        "*": "allow",
        "doom_loop": "ask",
        "review_comment": "deny",
        "external_directory": external,
        "read": { "*": "allow", "*.env": "ask", "*.env.*": "ask", "*.env.example": "allow" },
    }))
}

/// Rules for the "ask" approval preset: edits, commands and fetches ask first.
pub fn ask_preset() -> Ruleset {
    let mut r = Ruleset::default();
    for p in ["edit", "bash", "webfetch", "websearch"] {
        r.push(p, "*", Action::Ask);
    }
    // Commands that only read are fine.
    for c in SAFE_COMMANDS {
        r.push("bash", &format!("{c} *"), Action::Allow);
    }
    r
}

const SAFE_COMMANDS: &[&str] = &[
    "ls",
    "pwd",
    "cat",
    "head",
    "tail",
    "wc",
    "file",
    "stat",
    "which",
    "echo",
    "rg",
    "grep",
    "find",
    "tree",
    "du",
    "df",
    "git status",
    "git diff",
    "git log",
    "git show",
    "git branch",
    "git blame",
    "git rev-parse",
    "date",
    "env",
    "uname",
];

/// The part of a command an "always allow" covers: `git push origin x` → `git push`, `ls -la` → `ls`.
pub fn command_prefix(command: &str) -> String {
    const TWO_WORDS: &[&str] = &[
        "git",
        "npm",
        "pnpm",
        "yarn",
        "bun",
        "cargo",
        "go",
        "docker",
        "kubectl",
        "gh",
        "pip",
        "uv",
        "poetry",
        "make",
        "dotnet",
        "mvn",
        "gradle",
        "./gradlew",
        "aws",
        "terraform",
        "helm",
        "npx",
        "brew",
        "apt",
        "systemctl",
    ];
    let words: Vec<&str> = command.split_whitespace().filter(|w| !w.contains('=') || w.starts_with('-')).collect();
    match words.as_slice() {
        [] => String::new(),
        [first, second, ..] if TWO_WORDS.contains(first) && !second.starts_with('-') => format!("{first} {second}"),
        [first, ..] => first.to_string(),
    }
}

/// Splits a shell command line into its simple commands (on `&&`, `||`, `;`, `|`, newlines),
/// outside quotes, so each one is checked on its own.
pub fn split_commands(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                cur.push(c);
                if c == q {
                    quote = None;
                } else if c == '\\'
                    && q == '"'
                    && let Some(n) = chars.next()
                {
                    cur.push(n);
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    cur.push(c);
                }
                '\\' => {
                    cur.push(c);
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                }
                ';' | '\n' => out.push(std::mem::take(&mut cur)),
                '&' | '|' => {
                    if chars.peek() == Some(&c) {
                        chars.next();
                    }
                    if c == '&' && cur.ends_with('>') {
                        // `2>&1` is a redirect, not a separator.
                        cur.push(c);
                    } else {
                        out.push(std::mem::take(&mut cur));
                    }
                }
                _ => cur.push(c),
            },
        }
    }
    out.push(cur);
    out.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_rules() {
        assert!(wildcard("git status", "git status *"));
        assert!(wildcard("git status -s", "git status *"));
        assert!(!wildcard("git statusx", "git status *"));
        assert!(wildcard("src/a/.env", "*.env"));
        assert!(wildcard("a.rs", "?.rs"));
        assert!(!wildcard("ab.rs", "?.rs"));
    }

    #[test]
    fn last_match_wins() {
        let r = Ruleset::from_config(&json!({"*": "allow", "bash": {"*": "ask", "git *": "allow"}, "edit": "deny"}));
        assert_eq!(r.evaluate("bash", "rm -rf x"), Action::Ask);
        assert_eq!(r.evaluate("bash", "git log"), Action::Allow);
        assert_eq!(r.evaluate("read", "x"), Action::Allow);
        assert!(r.disabled("edit"));
        assert!(!r.disabled("bash"));
        let d = defaults(&[]);
        assert_eq!(d.evaluate("read", "app/.env"), Action::Ask);
        assert_eq!(d.evaluate("read", ".env.example"), Action::Allow);
    }

    #[test]
    fn splits_and_prefixes_commands() {
        assert_eq!(
            split_commands("cd a && cargo test 2>&1 | tail -5; echo 'a;b'"),
            ["cd a", "cargo test 2>&1", "tail -5", "echo 'a;b'"]
        );
        assert_eq!(command_prefix("git push origin main"), "git push");
        assert_eq!(command_prefix("RUST_LOG=1 cargo test -p x"), "cargo test");
        assert_eq!(command_prefix("ls -la"), "ls");
    }
}
