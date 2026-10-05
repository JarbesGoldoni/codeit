//! Snapshots of the project's files, so `/undo` can also revert what shell commands changed.
//!
//! Like opencode, codeit keeps a separate git repository per project under
//! `~/.local/share/codeit/snapshot/<id>`, with the project as its work tree: it never touches the
//! project's own repository, index or history, and the project's `.gitignore` applies. A turn
//! records a tree before and after it runs; undo restores the files that changed between the
//! two, skipping any you changed since. Only done for git projects, so a home folder is never
//! snapshotted.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

fn store(root: &Path) -> PathBuf {
    // A stable id for the folder: FNV-1a of its path.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in root.to_string_lossy().bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x100000001b3);
    }
    codeit_providers::data_dir().join("snapshot").join(format!("{h:016x}"))
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let dir = store(root);
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(&dir)
        .arg("--work-tree")
        .arg(root)
        .args(["-c", "core.autocrlf=false", "-c", "core.quotepath=false", "-c", "core.longpaths=true"])
        .args(args)
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("couldn't run git")?;
    if !out.status.success() {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Records the project's files and returns the tree id; `None` outside a git project or when
/// git fails (undo then falls back to the edits codeit made itself).
pub fn track(root: &Path) -> Option<String> {
    if !root.join(".git").exists() {
        return None;
    }
    let dir = store(root);
    if !dir.join("HEAD").exists() {
        std::fs::create_dir_all(&dir).ok()?;
        git(root, &["init", "-q"]).ok()?;
    }
    git(root, &["add", "-A", "."]).ok()?;
    git(root, &["write-tree"]).ok().map(|t| t.trim().to_string())
}

/// Paths (relative to the root) that differ between two trees.
pub fn changed(root: &Path, from: &str, to: &str) -> Result<Vec<String>> {
    Ok(git(root, &["diff", "--name-only", "--no-renames", from, to])?.lines().map(String::from).collect())
}

/// The blob id of `path` in `tree`, `None` when it isn't there.
fn blob(root: &Path, tree: &str, path: &str) -> Option<String> {
    git(root, &["rev-parse", "--verify", "--quiet", &format!("{tree}:{path}")]).ok().map(|s| s.trim().to_string())
}

/// What the file is on disk now, as a blob id (`None` when missing).
fn current(root: &Path, path: &str) -> Option<String> {
    if !root.join(path).is_file() {
        return None;
    }
    git(root, &["hash-object", "--", path]).ok().map(|s| s.trim().to_string())
}

pub struct Restore {
    pub restored: Vec<PathBuf>,
    /// Changed again after the turn: left alone.
    pub skipped: Vec<PathBuf>,
}

/// Puts back the files that changed between `before` and `after`, if they are still as `after`
/// left them.
pub fn restore(root: &Path, before: &str, after: &str) -> Result<Restore> {
    let mut out = Restore { restored: Vec::new(), skipped: Vec::new() };
    for path in changed(root, before, after)? {
        let full = root.join(&path);
        if current(root, &path) != blob(root, after, &path) {
            out.skipped.push(full);
            continue;
        }
        match blob(root, before, &path) {
            Some(_) => {
                git(root, &["checkout", before, "--", &path])?;
            }
            None => {
                let _ = std::fs::remove_file(&full);
            }
        }
        out.restored.push(full);
    }
    // The next snapshot shouldn't see the restore as new work.
    let _ = git(root, &["add", "-A", "."]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_what_a_turn_changed_but_not_later_edits() {
        let root = std::env::temp_dir().join(format!("codeit-snap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("a.txt"), "a1").unwrap();
        std::fs::write(root.join("b.txt"), "b1").unwrap();
        std::fs::write(root.join(".gitignore"), "ignored/\n").unwrap();
        crate::test_home();

        let before = track(&root).expect("a git project is tracked");
        // The turn: a command changes a.txt, creates c.txt, edits b.txt.
        std::fs::write(root.join("a.txt"), "a2").unwrap();
        std::fs::write(root.join("c.txt"), "new").unwrap();
        std::fs::write(root.join("b.txt"), "b2").unwrap();
        std::fs::create_dir_all(root.join("ignored")).unwrap();
        std::fs::write(root.join("ignored/x"), "x").unwrap();
        let after = track(&root).unwrap();
        // After the turn, you edit b.txt yourself.
        std::fs::write(root.join("b.txt"), "b3 mine").unwrap();

        let mut changed = changed(&root, &before, &after).unwrap();
        changed.sort();
        assert_eq!(changed, ["a.txt", "b.txt", "c.txt"], "ignored files aren't tracked");
        let r = restore(&root, &before, &after).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "a1");
        assert!(!root.join("c.txt").exists());
        assert_eq!(std::fs::read_to_string(root.join("b.txt")).unwrap(), "b3 mine", "your later edit is kept");
        assert_eq!(r.skipped, vec![root.join("b.txt")]);
        assert_eq!(r.restored.len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }
}
