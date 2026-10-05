//! Prompt commands: `/name args` expands a Markdown template into a prompt.
//!
//! Templates come from `.codeit/commands/*.md`, `~/.config/codeit/commands/*.md`, the opencode and
//! Claude command folders, the `command` config key, and extensions. Frontmatter may set
//! `description`, `agent`, `model` and `subtask` (run it in a subagent). In the template,
//! `$ARGUMENTS` is the whole argument string, `$1`, `$2`... single arguments (the last one used
//! takes the rest), `` !`cmd` `` the output of a shell command, and `@path` attaches a file.

use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::util;

#[derive(Clone, Debug)]
pub struct Command {
    pub name: String,
    pub description: String,
    pub template: String,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub subtask: bool,
}

pub fn builtin() -> Vec<Command> {
    let cmd = |name: &str, description: &str, template: &str| Command {
        name: name.into(),
        description: description.into(),
        template: template.into(),
        agent: None,
        model: None,
        subtask: false,
    };
    vec![
        cmd("init", "create or update AGENTS.md for this project", include_str!("prompts/init.md")),
        cmd("review", "review uncommitted changes, a commit, a branch or a PR", include_str!("prompts/review.md")),
    ]
}

/// Every command, later sources overriding earlier ones with the same name.
pub fn discover(project_dirs: &[PathBuf], config: &Config) -> Vec<Command> {
    let home = dirs::home_dir().unwrap_or_default();
    let xdg = crate::config::xdg_config();
    let mut dirs = vec![
        home.join(".claude/commands"),
        xdg.join("opencode/command"),
        xdg.join("opencode/commands"),
        xdg.join("codeit/commands"),
    ];
    for d in project_dirs {
        for sub in [".claude/commands", ".opencode/command", ".opencode/commands", ".codeit/commands"] {
            dirs.push(d.join(sub));
        }
    }
    let mut out = builtin();
    let add = |c: Command, out: &mut Vec<Command>| {
        out.retain(|o| o.name != c.name);
        out.push(c);
    };
    for dir in dirs {
        for c in from_dir(&dir) {
            add(c, &mut out);
        }
    }
    for (name, c) in &config.command {
        add(
            Command {
                name: name.clone(),
                description: c.description.clone().unwrap_or_default(),
                template: c.template.clone(),
                agent: c.agent.clone(),
                model: c.model.clone(),
                subtask: c.subtask.unwrap_or(false),
            },
            &mut out,
        );
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Commands in a folder; files in subfolders are named `sub/name`.
pub fn from_dir(dir: &Path) -> Vec<Command> {
    let mut out = Vec::new();
    if !dir.is_dir() {
        return out;
    }
    for entry in ignore::WalkBuilder::new(dir).hidden(false).max_depth(Some(3)).build().flatten() {
        let p = entry.path();
        if p.extension().is_none_or(|e| e != "md") || !p.is_file() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(p) else { continue };
        let rel = p.strip_prefix(dir).unwrap_or(p).with_extension("");
        let name = rel.to_string_lossy().replace('\\', "/");
        out.push(parse(&name, &text));
    }
    out
}

pub fn parse(name: &str, text: &str) -> Command {
    let (f, body) = util::frontmatter(text);
    Command {
        name: name.into(),
        description: f.get("description").cloned().unwrap_or_default(),
        template: body,
        agent: f.get("agent").cloned(),
        model: f.get("model").cloned(),
        subtask: f.get("subtask").is_some_and(|v| v == "true"),
    }
}

/// Splits arguments on spaces, keeping quoted ones together (quotes removed).
pub fn split_args(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote = None;
    let mut started = false;
    for c in args.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                started = true;
            }
            None if c.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            None => {
                cur.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(cur);
    }
    out
}

/// Fills `$ARGUMENTS` and `$1`.. in a template. Arguments with no placeholder are appended.
pub fn render(template: &str, args: &str) -> String {
    let list = split_args(args);
    let re = regex::Regex::new(r"\$(\d+)").expect("valid regex");
    let last = re.captures_iter(template).filter_map(|c| c[1].parse::<usize>().ok()).max().unwrap_or(0);
    let mut out = re
        .replace_all(template, |c: &regex::Captures| {
            let n: usize = c[1].parse().unwrap_or(0);
            if n == 0 || n > list.len() {
                return String::new();
            }
            if n == last { list[n - 1..].join(" ") } else { list[n - 1].clone() }
        })
        .into_owned();
    let uses_args = template.contains("$ARGUMENTS");
    out = out.replace("$ARGUMENTS", args.trim());
    if last == 0 && !uses_args && !args.trim().is_empty() {
        out = format!("{}\n\n{}", out.trim_end(), args.trim());
    }
    out
}

/// Runs each `` !`cmd` `` in the text and puts its output in its place.
pub async fn run_shell(text: &str, cwd: &Path) -> String {
    let re = regex::Regex::new(r"!`([^`]+)`").expect("valid regex");
    let mut out = String::new();
    let mut last = 0;
    for c in re.captures_iter(text) {
        let m = c.get(0).expect("whole match");
        out.push_str(&text[last..m.start()]);
        let result = tokio::process::Command::new("bash").arg("-c").arg(&c[1]).current_dir(cwd).output().await;
        let output = match result {
            Ok(o) => {
                let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
                s.push_str(&String::from_utf8_lossy(&o.stderr));
                util::clean_terminal(s.trim_end())
            }
            Err(e) => format!("(failed to run `{}`: {e})", &c[1]),
        };
        out.push_str(&output);
        last = m.end();
    }
    out.push_str(&text[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_placeholders() {
        assert_eq!(render("Task: $ARGUMENTS", "ABC-1 now"), "Task: ABC-1 now");
        assert_eq!(render("a=$1 rest=$2", "x \"y z\" w"), "a=x rest=y z w");
        assert_eq!(render("Review.", "main"), "Review.\n\nmain");
        assert_eq!(render("Review $1.", ""), "Review .");
    }

    #[test]
    fn parses_command_files() {
        let c = parse("task", "---\ndescription: Work on a task\nagent: build\n---\n\nDo $ARGUMENTS\n");
        assert_eq!(c.description, "Work on a task");
        assert_eq!(c.agent.as_deref(), Some("build"));
        assert_eq!(c.template, "Do $ARGUMENTS\n");
    }

    #[tokio::test]
    async fn substitutes_shell_output() {
        let out = run_shell("branch: !`echo main`.", Path::new(".")).await;
        assert_eq!(out, "branch: main.");
    }
}
