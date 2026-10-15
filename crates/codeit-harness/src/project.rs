//! Projects: when codeit starts in a folder that holds several (a workspace, not a git
//! repository itself), each session picks the one it works in, with the `project` tool or in
//! the interface. Commands then run there and relative paths start there
//! ([`Session::workdir`](crate::session::Session::workdir)).

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use async_trait::async_trait;
use codeit_providers::ToolSpec;
use serde_json::{Value, json};

use crate::event::Event;
use crate::tools::{Ctx, Output, Tool, spec};
use crate::{Harness, util};

pub struct Project {
    pub path: PathBuf,
    /// The checked-out branch, or the commit when detached.
    pub branch: Option<String>,
}

/// The git repositories in `root`: the folders right inside it, and inside the ones that aren't
/// repositories themselves (one level down). Hidden folders are skipped.
pub fn list(root: &Path) -> Vec<Project> {
    let mut out = Vec::new();
    for dir in subdirs(root) {
        if dir.join(".git").exists() {
            out.push(dir);
        } else {
            out.extend(subdirs(&dir).into_iter().filter(|d| d.join(".git").exists()));
        }
    }
    out.into_iter().map(|path| Project { branch: branch(&path), path }).collect()
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter(|e| e.file_name() != "node_modules" && e.file_name() != "target")
        .map(|e| e.path())
        .collect();
    v.sort();
    v.truncate(300);
    v
}

/// The branch checked out in the repository at or above `dir`, read from its `HEAD`.
pub fn branch(dir: &Path) -> Option<String> {
    let root = util::git_root(dir)?;
    let mut git = root.join(".git");
    if git.is_file() {
        // A worktree: `.git` points at its git directory.
        let text = std::fs::read_to_string(&git).ok()?;
        git = util::resolve(&root, text.trim().strip_prefix("gitdir:")?.trim());
    }
    let head = std::fs::read_to_string(git.join("HEAD")).ok()?;
    let head = head.trim();
    Some(match head.strip_prefix("ref: refs/heads/") {
        Some(b) => b.to_string(),
        None => head.chars().take(8).collect(),
    })
}

/// Where the session is now, for the model.
pub fn describe(dir: &Path) -> String {
    match branch(dir) {
        Some(b) => format!("{} (git, branch {b})", dir.display()),
        None => format!("{} (not a git repository)", dir.display()),
    }
}

/// Told to the model with the next message after the user moved the session.
pub fn moved(dir: &Path) -> String {
    format!(
        "<system-reminder>The user moved this session to {}. Commands now run there and relative paths start there.</system-reminder>",
        describe(dir)
    )
}

/// Whether the folder codeit started in is a workspace of projects rather than one project.
pub fn is_workspace(h: &Harness) -> bool {
    util::git_root(&h.cwd).is_none() && !list(&h.cwd).is_empty()
}

pub struct ProjectTool;

#[async_trait]
impl Tool for ProjectTool {
    fn name(&self) -> &str {
        "project"
    }

    fn spec(&self, h: &Harness) -> ToolSpec {
        spec(
            "project",
            &format!(
                "The working directory {} holds several projects. Before working on one, move the session to it: \
commands then run there and relative paths start there, and the user sees the project and branch. Without `path`, \
lists the projects with their branches.",
                h.cwd.display()
            ),
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "The project folder, relative to that directory or absolute" }
                }
            }),
        )
    }

    fn title(&self, input: &Value, _: &Path) -> String {
        input["path"].as_str().unwrap_or("list").to_string()
    }

    async fn run(&self, input: Value, ctx: &Ctx) -> Result<Output> {
        let root = &ctx.harness.cwd;
        let Some(p) = input["path"].as_str().filter(|p| !p.trim().is_empty()) else {
            let lines: Vec<String> = list(root)
                .iter()
                .map(|p| {
                    let name = util::display(root, &p.path);
                    match &p.branch {
                        Some(b) => format!("{name} ({b})"),
                        None => name,
                    }
                })
                .collect();
            if lines.is_empty() {
                return Ok(Output::text(format!("No git repositories in {}.", root.display())));
            }
            return Ok(Output::text(lines.join("\n")));
        };
        let dir = util::resolve(root, p.trim());
        if !dir.starts_with(root) {
            bail!("{} is outside {}; pick a project inside it.", dir.display(), root.display());
        }
        if !dir.is_dir() {
            bail!("{} isn't a folder. Call project without a path to list them.", dir.display());
        }
        ctx.session.lock().unwrap().project = (&dir != root).then(|| dir.clone());
        let _ = ctx.events.send(Event::Project(dir.clone()));
        Ok(Output::text(format!("Now working in {}.", describe(&dir))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_repositories_one_level_down() {
        let d = std::env::temp_dir().join(format!("codeit-projects-{}", uuid::Uuid::new_v4()));
        for repo in ["a", "group/b", "plain/c/d"] {
            std::fs::create_dir_all(d.join(repo).join(".git")).unwrap();
        }
        std::fs::write(d.join("a/.git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::create_dir_all(d.join(".hidden/.git")).unwrap();
        let found: Vec<(String, Option<String>)> =
            list(&d).into_iter().map(|p| (util::display(&d, &p.path), p.branch)).collect();
        assert_eq!(found, vec![("a".to_string(), Some("main".to_string())), ("group/b".to_string(), None)]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
