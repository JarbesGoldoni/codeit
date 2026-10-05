//! Instruction files added to the system prompt: AGENTS.md (or CLAUDE.md) in the project, a
//! global one, and the `instructions` config entries. AGENTS.md files in subfolders are attached
//! to the first read of a file under them instead, so they cost nothing until they matter.

use std::path::{Path, PathBuf};

use crate::util;

const NAMES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

/// The global file: `~/.config/codeit/AGENTS.md`, else opencode's, else `~/.claude/CLAUDE.md`.
pub fn global_file() -> Option<PathBuf> {
    let xdg = crate::config::xdg_config();
    let home = dirs::home_dir().unwrap_or_default();
    [xdg.join("codeit/AGENTS.md"), xdg.join("opencode/AGENTS.md"), home.join(".claude/CLAUDE.md")]
        .into_iter()
        .find(|p| p.is_file())
}

/// AGENTS.md files from the project root down to `cwd`; CLAUDE.md only where no AGENTS.md exists.
pub fn project_files(cwd: &Path, root: &Path) -> Vec<PathBuf> {
    for name in NAMES {
        let found: Vec<PathBuf> =
            crate::config::dirs_between(root, cwd).into_iter().map(|d| d.join(name)).filter(|p| p.is_file()).collect();
        if !found.is_empty() {
            return found;
        }
    }
    Vec::new()
}

/// Local instruction files for the system prompt: the global one, the project's, and the
/// `instructions` config entries that are paths or globs.
pub fn files(cwd: &Path, root: &Path, extra: &[String]) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = global_file().into_iter().collect();
    paths.extend(project_files(cwd, root));
    for item in extra.iter().filter(|i| !is_url(i)) {
        let full = util::resolve(cwd, item);
        let pattern = full.to_string_lossy().into_owned();
        if pattern.contains(['*', '?', '[']) {
            paths.extend(glob_files(&pattern));
        } else if full.is_file() {
            paths.push(full);
        } else {
            // Relative entries may name a file at the project root.
            let at_root = util::resolve(root, item);
            if at_root.is_file() {
                paths.push(at_root);
            }
        }
    }
    let mut seen: Vec<PathBuf> = Vec::new();
    for p in paths {
        if !seen.contains(&p) {
            seen.push(p);
        }
    }
    seen
}

fn is_url(s: &str) -> bool {
    s.starts_with("https://") || s.starts_with("http://")
}

/// The `instructions` entries that are URLs, fetched once (5 s timeout each).
pub async fn fetch_urls(extra: &[String]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for url in extra.iter().filter(|i| is_url(i)) {
        let res = reqwest::Client::new().get(url).timeout(std::time::Duration::from_secs(5)).send().await;
        if let Ok(r) = res
            && r.status().is_success()
            && let Ok(text) = r.text().await
        {
            out.push((url.clone(), text));
        }
    }
    out
}

/// Reads instruction files as (label, content), skipping empty ones.
pub fn read(cwd: &Path, files: &[PathBuf]) -> Vec<(String, String)> {
    files
        .iter()
        .filter_map(|p| {
            let text = std::fs::read_to_string(p).ok()?;
            (!text.trim().is_empty()).then(|| (util::display(cwd, p), text))
        })
        .collect()
}

fn glob_files(pattern: &str) -> Vec<PathBuf> {
    // Walk from the deepest folder without wildcards.
    let base: PathBuf = Path::new(pattern)
        .components()
        .take_while(|c| !c.as_os_str().to_string_lossy().contains(['*', '?', '[']))
        .collect();
    let Ok(glob) = globset::Glob::new(pattern) else { return Vec::new() };
    let m = glob.compile_matcher();
    ignore::WalkBuilder::new(&base)
        .max_depth(Some(8))
        .build()
        .flatten()
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()) && m.is_match(e.path()))
        .map(|e| e.into_path())
        .collect()
}

/// The system prompt section for the loaded instructions.
pub fn prompt(items: &[(String, String)]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let mut s = String::from(
        "# Instructions\nThe user and the project gave these instructions. Follow them; more specific (deeper) files win over general ones.\n",
    );
    for (label, text) in items {
        s.push_str(&format!("\n<instructions from=\"{label}\">\n{}\n</instructions>\n", text.trim()));
    }
    Some(s)
}

/// AGENTS.md files in folders between `file` and `cwd` (exclusive) not yet seen by the model.
pub fn nested(file: &Path, cwd: &Path, seen: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !file.starts_with(cwd) {
        return out;
    }
    let mut dir = file.parent();
    while let Some(d) = dir {
        if d == cwd || !d.starts_with(cwd) {
            break;
        }
        for name in NAMES {
            let p = d.join(name);
            if p.is_file() && p != file {
                if !seen.contains(&p) {
                    out.push(p);
                }
                break;
            }
        }
        dir = d.parent();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_project_and_nested_files() {
        let root = std::env::temp_dir().join(format!("codeit-instr-{}", uuid::Uuid::new_v4()));
        let sub = root.join("app/src");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(root.join("AGENTS.md"), "root rules").unwrap();
        std::fs::write(root.join("app/CLAUDE.md"), "app rules").unwrap();
        std::fs::write(sub.join("x.rs"), "").unwrap();

        assert_eq!(project_files(&root, &root), vec![root.join("AGENTS.md")]);
        let n = nested(&sub.join("x.rs"), &root, &[]);
        assert_eq!(n, vec![root.join("app/CLAUDE.md")]);
        assert!(nested(&sub.join("x.rs"), &root, &n).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
