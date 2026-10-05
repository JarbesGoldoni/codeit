//! Code reviews: a diff (your uncommitted changes, a branch, a commit or a GitHub PR), and
//! comments on its lines, like a pull request review.
//!
//! The `review` agent reads the changes and leaves comments with the `review_comment` tool,
//! which only accepts lines that are in the diff. You can add your own, and mark each one
//! agreed or dismissed. Reviews are saved per folder and target under
//! `~/.local/share/codeit/reviews/`, so reopening one keeps the comments and your verdicts.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use codeit_providers::ToolSpec;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::tools::{Ctx, Output, Tool, arg, spec};
use crate::{Harness, util};

/// What is being reviewed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum Target {
    /// Uncommitted changes, untracked files included.
    Working,
    /// A branch: its changes since it left the current one (`git diff <branch>...HEAD`
    /// reviews the current branch against it).
    Branch(String),
    Commit(String),
    /// A GitHub pull request, through `gh`.
    Pr(String),
}

impl Target {
    /// Reads `/review` arguments: nothing, `pr 12`, `#12`, a PR URL, a branch or a commit.
    pub fn parse(args: &str, cwd: &Path) -> Target {
        let a = args.trim();
        if a.is_empty() {
            return Target::Working;
        }
        if let Some(n) = a.strip_prefix("pr ").or_else(|| a.strip_prefix('#')) {
            return Target::Pr(n.trim().to_string());
        }
        if a.contains("/pull/") {
            return Target::Pr(a.to_string());
        }
        let is_branch = ["refs/heads/", "refs/remotes/"].iter().any(|p| {
            std::process::Command::new("git")
                .args(["show-ref", "--verify", "--quiet", &format!("{p}{a}")])
                .current_dir(cwd)
                .status()
                .is_ok_and(|s| s.success())
        });
        if is_branch { Target::Branch(a.to_string()) } else { Target::Commit(a.to_string()) }
    }

    pub fn label(&self) -> String {
        match self {
            Target::Working => "uncommitted changes".into(),
            Target::Branch(b) => format!("changes since {b}"),
            Target::Commit(c) => format!("commit {c}"),
            Target::Pr(p) => format!("PR {p}"),
        }
    }

    /// The command that shows the same diff, for the agent.
    pub fn command(&self) -> String {
        match self {
            Target::Working => "git diff HEAD (and the untracked files listed)".into(),
            Target::Branch(b) => format!("git diff {b}...HEAD"),
            Target::Commit(c) => format!("git show {c}"),
            Target::Pr(p) => format!("gh pr diff {p}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiffLine {
    /// `' '`, `'+'` or `'-'`.
    pub kind: char,
    pub text: String,
    pub old: Option<u32>,
    pub new: Option<u32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Hunk {
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: String,
    /// `M`odified, `A`dded, `D`eleted, `R`enamed.
    pub status: char,
    pub added: usize,
    pub removed: usize,
    pub binary: bool,
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    /// Lines a comment may point at: lines of the new version shown in the diff (or, for a
    /// deleted file, of the old one).
    pub fn commentable(&self, line: u32) -> bool {
        self.hunks
            .iter()
            .flat_map(|h| &h.lines)
            .any(|l| if self.status == 'D' { l.old == Some(line) } else { l.new == Some(line) })
    }

    /// `12-30, 41-44`: the new-side line ranges in the diff, for error messages.
    pub fn ranges(&self) -> String {
        self.hunks
            .iter()
            .filter_map(|h| {
                let nums: Vec<u32> =
                    h.lines.iter().filter_map(|l| if self.status == 'D' { l.old } else { l.new }).collect();
                Some(format!("{}-{}", nums.first()?, nums.last()?))
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    #[default]
    Open,
    Agreed,
    Dismissed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub path: String,
    pub line: u32,
    /// `bug`, `risk`, `question` or `nit`; `note` for yours.
    pub severity: String,
    pub body: String,
    /// `codeit` or `you`.
    pub author: String,
    #[serde(default)]
    pub verdict: Verdict,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Review {
    pub id: String,
    pub cwd: PathBuf,
    pub target: Target,
    /// The PR's title and author, when it is one.
    #[serde(default)]
    pub title: String,
    #[serde(skip)]
    pub files: Vec<FileDiff>,
    pub comments: Vec<Comment>,
    /// The agent's overall assessment from its last run.
    #[serde(default)]
    pub summary: Option<String>,
    pub updated: u64,
}

fn dir() -> PathBuf {
    codeit_providers::data_dir().join("reviews")
}

fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    run(cwd, "git", args)
}

fn run(cwd: &Path, program: &str, args: &[&str]) -> Result<String> {
    let out = std::process::Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env("GIT_PAGER", "cat")
        .env("NO_COLOR", "1")
        .output()
        .with_context(|| format!("couldn't run {program}"))?;
    if !out.status.success() {
        bail!("{program} {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The diff of a target, parsed.
pub fn load_diff(cwd: &Path, target: &Target) -> Result<(Vec<FileDiff>, String)> {
    let flags = ["--no-color", "--no-ext-diff", "-M"];
    let (text, title) = match target {
        Target::Working => {
            let mut args = vec!["diff", "HEAD"];
            args.extend(flags);
            // A repository without commits has no HEAD: show what's staged.
            let mut text = git(cwd, &args).or_else(|_| git(cwd, &["diff", "--cached", "--no-color"]))?;
            for path in git(cwd, &["ls-files", "--others", "--exclude-standard"])?.lines() {
                text.push_str(&untracked(cwd, path));
            }
            (text, String::new())
        }
        Target::Branch(b) => {
            let range = format!("{b}...HEAD");
            let mut args = vec!["diff", range.as_str()];
            args.extend(flags);
            (git(cwd, &args)?, String::new())
        }
        Target::Commit(c) => {
            let mut args = vec!["show", "--format=", c.as_str()];
            args.extend(flags);
            let subject = git(cwd, &["log", "-1", "--format=%s (%an)", c]).unwrap_or_default();
            (git(cwd, &args)?, subject.trim().to_string())
        }
        Target::Pr(p) => {
            let text = run(cwd, "gh", &["pr", "diff", p, "--color=never"])?;
            let meta = run(cwd, "gh", &["pr", "view", p, "--json", "title,author,url"]).unwrap_or_default();
            let v: Value = serde_json::from_str(&meta).unwrap_or_default();
            let title = format!(
                "{} ({})",
                v["title"].as_str().unwrap_or_default(),
                v["author"]["login"].as_str().unwrap_or_default()
            );
            (text, title)
        }
    };
    Ok((parse(&text), title))
}

/// An untracked file as a diff that adds every line (text files up to 1 MB).
fn untracked(cwd: &Path, path: &str) -> String {
    let full = cwd.join(path);
    match std::fs::read(&full) {
        Ok(bytes) if bytes.len() <= 1_000_000 && !bytes.contains(&0) => {
            let text = String::from_utf8_lossy(&bytes);
            let n = text.lines().count();
            let mut d = format!("diff --git a/{path} b/{path}\nnew file mode 100644\n--- /dev/null\n+++ b/{path}\n");
            if n > 0 {
                d.push_str(&format!("@@ -0,0 +1,{n} @@\n"));
                for l in text.lines() {
                    d.push('+');
                    d.push_str(l);
                    d.push('\n');
                }
            }
            d
        }
        _ => {
            format!("diff --git a/{path} b/{path}\nnew file mode 100644\nBinary files /dev/null and b/{path} differ\n")
        }
    }
}

/// Parses `git diff` output into files and hunks with line numbers on both sides.
pub fn parse(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let (mut old, mut new) = (0u32, 0u32);
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // `a/x b/x`: the new path is after the last " b/".
            let path = rest.rfind(" b/").map(|i| &rest[i + 3..]).unwrap_or(rest).to_string();
            files.push(FileDiff { path, status: 'M', ..Default::default() });
            continue;
        }
        let Some(f) = files.last_mut() else { continue };
        if line.starts_with("new file mode") {
            f.status = 'A';
        } else if line.starts_with("deleted file mode") {
            f.status = 'D';
        } else if line.starts_with("rename from") {
            f.status = 'R';
        } else if line.starts_with("Binary files") {
            f.binary = true;
        } else if let Some(h) = line.strip_prefix("@@ ") {
            let nums: Vec<u32> =
                h.split_whitespace().take(2).filter_map(|p| p.get(1..)?.split(',').next()?.parse().ok()).collect();
            if let [o, n] = nums[..] {
                old = o;
                new = n;
            }
            f.hunks.push(Hunk { header: line.to_string(), lines: Vec::new() });
        } else if let Some(h) = f.hunks.last_mut() {
            let (kind, body) = match line.chars().next() {
                Some(c @ ('+' | '-' | ' ')) => (c, &line[1..]),
                Some('\\') => continue, // "\ No newline at end of file"
                _ => (' ', line),
            };
            let (o, n) = match kind {
                '+' => {
                    f.added += 1;
                    new += 1;
                    (None, Some(new - 1))
                }
                '-' => {
                    f.removed += 1;
                    old += 1;
                    (Some(old - 1), None)
                }
                _ => {
                    old += 1;
                    new += 1;
                    (Some(old - 1), Some(new - 1))
                }
            };
            h.lines.push(DiffLine { kind, text: body.to_string(), old: o, new: n });
        }
    }
    files
}

impl Review {
    /// Opens the saved review of this target in this folder, or starts one, and loads its diff.
    pub fn open(cwd: &Path, target: Target) -> Result<Review> {
        let (files, title) = load_diff(cwd, &target)?;
        let saved = std::fs::read_dir(dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| serde_json::from_slice::<Review>(&std::fs::read(e.path()).ok()?).ok())
            .find(|r| r.cwd == cwd && r.target == target);
        let mut r = saved.unwrap_or_else(|| Review {
            id: uuid::Uuid::new_v4().to_string(),
            cwd: cwd.to_path_buf(),
            target,
            title: String::new(),
            files: Vec::new(),
            comments: Vec::new(),
            summary: None,
            updated: util::now(),
        });
        r.title = title;
        r.files = files;
        Ok(r)
    }

    pub fn save(&mut self) -> Result<()> {
        self.updated = util::now();
        std::fs::create_dir_all(dir())?;
        std::fs::write(dir().join(format!("{}.json", self.id)), serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn file(&self, path: &str) -> Option<&FileDiff> {
        self.files.iter().find(|f| f.path == path)
    }

    /// Adds a comment after checking it points at a line of the diff.
    pub fn add(&mut self, path: &str, line: u32, severity: &str, body: &str, author: &str) -> Result<Comment> {
        let Some(f) = self.file(path) else {
            let names: Vec<&str> = self.files.iter().map(|f| f.path.as_str()).collect();
            bail!("{path} is not in this diff. Changed files: {}.", names.join(", "));
        };
        if !f.commentable(line) {
            bail!("Line {line} of {path} is not in the diff. Comment on a line shown in it: {}.", f.ranges());
        }
        let c = Comment {
            id: uuid::Uuid::new_v4().to_string()[..8].to_string(),
            path: path.to_string(),
            line,
            severity: severity.to_string(),
            body: body.trim().to_string(),
            author: author.to_string(),
            verdict: Verdict::Open,
        };
        self.comments.push(c.clone());
        let _ = self.save();
        Ok(c)
    }

    /// The first message of a review run: what to review and how.
    pub fn request(&self) -> String {
        let mut files = String::new();
        for f in &self.files {
            files.push_str(&format!("- {} ({}, +{} -{})\n", f.path, f.status, f.added, f.removed));
        }
        let mut existing = String::new();
        for c in self.comments.iter().filter(|c| c.verdict != Verdict::Dismissed) {
            existing.push_str(&format!("- {}:{} [{}] {}\n", c.path, c.line, c.author, util::cut_line(&c.body, 200)));
        }
        let pr_note = if matches!(self.target, Target::Pr(_)) {
            "\nThe PR's code may not be checked out here: rely on `gh pr diff`, and `gh pr view` or `gh api` for more context, rather than the local files.\n"
        } else {
            ""
        };
        format!(
            "Review {}{}.\n\nChanged files:\n{files}\nSee the diff with `{}`.{pr_note}\n{}{}",
            self.target.label(),
            if self.title.is_empty() { String::new() } else { format!(": {}", self.title) },
            self.target.command(),
            if existing.is_empty() {
                String::new()
            } else {
                format!("Comments already made (don't repeat them):\n{existing}\n")
            },
            include_str!("prompts/review_run.md"),
        )
    }

    /// The review as Markdown: agreed comments first, then the open ones.
    pub fn markdown(&self) -> String {
        let mut out = format!("# Review: {}\n\n", self.target.label());
        if !self.title.is_empty() {
            out.push_str(&format!("{}\n\n", self.title));
        }
        if let Some(s) = &self.summary {
            out.push_str(&format!("{s}\n\n"));
        }
        for (verdict, head) in [(Verdict::Agreed, "Agreed"), (Verdict::Open, "Open")] {
            let list: Vec<&Comment> = self.comments.iter().filter(|c| c.verdict == verdict).collect();
            if list.is_empty() {
                continue;
            }
            out.push_str(&format!("## {head}\n\n"));
            for c in list {
                out.push_str(&format!("- **{}:{}** ({}, {}): {}\n", c.path, c.line, c.severity, c.author, c.body));
            }
            out.push('\n');
        }
        out
    }
}

/// The `review_comment` tool: only offered while a review runs, to the review agent.
pub struct ReviewComment;

#[async_trait]
impl Tool for ReviewComment {
    fn name(&self) -> &str {
        "review_comment"
    }

    fn spec(&self, _: &Harness) -> ToolSpec {
        spec(
            "review_comment",
            "Leave a review comment on one line of the diff under review, like a pull request comment. Use the \
line number in the new version of the file (a line shown in the diff; for a deleted file, the old one). One \
comment per problem; say what is wrong, when it breaks, and the fix.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path as listed in the review" },
                    "line": { "type": "integer" },
                    "severity": { "type": "string", "enum": ["bug", "risk", "question", "nit"] },
                    "body": { "type": "string" }
                },
                "required": ["path", "line", "severity", "body"]
            }),
        )
    }

    fn title(&self, input: &Value, _: &Path) -> String {
        format!("{}:{}", input["path"].as_str().unwrap_or_default(), input["line"].as_u64().unwrap_or_default())
    }

    async fn run(&self, input: Value, ctx: &Ctx) -> Result<Output> {
        let Some(review) = ctx.harness.review() else { bail!("No review is open.") };
        let path = arg(&input, "path")?.trim_start_matches("./");
        let line = input["line"].as_u64().context("line must be a number")? as u32;
        let severity = input["severity"].as_str().unwrap_or("risk");
        let body = arg(&input, "body")?;
        let c = review.lock().unwrap().add(path, line, severity, body, "codeit")?;
        Ok(Output::text(format!("Comment {} added on {}:{}.", c.id, c.path, c.line)))
    }
}

pub type Shared = Arc<Mutex<Review>>;

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "diff --git a/calc.py b/calc.py
index 1..2 100644
--- a/calc.py
+++ b/calc.py
@@ -1,3 +1,3 @@
 def add(a, b):
-    return a - b
+    return a + b
 x = 1
diff --git a/new.py b/new.py
new file mode 100644
--- /dev/null
+++ b/new.py
@@ -0,0 +1,2 @@
+print(1)
+print(2)
";

    #[test]
    fn parses_files_hunks_and_line_numbers() {
        let files = parse(DIFF);
        assert_eq!(files.len(), 2);
        let f = &files[0];
        assert_eq!((f.path.as_str(), f.status, f.added, f.removed), ("calc.py", 'M', 1, 1));
        let nums: Vec<(char, Option<u32>, Option<u32>)> =
            f.hunks[0].lines.iter().map(|l| (l.kind, l.old, l.new)).collect();
        assert_eq!(
            nums,
            [(' ', Some(1), Some(1)), ('-', Some(2), None), ('+', None, Some(2)), (' ', Some(3), Some(3))]
        );
        assert_eq!(files[1].status, 'A');
        assert!(files[1].commentable(2) && !files[1].commentable(3));
        assert_eq!(files[0].ranges(), "1-3");
    }

    #[test]
    fn comments_must_point_at_the_diff() {
        crate::test_home();
        let mut r = Review {
            id: "t".into(),
            cwd: PathBuf::from("/nonexistent"),
            target: Target::Working,
            title: String::new(),
            files: parse(DIFF),
            comments: Vec::new(),
            summary: None,
            updated: 0,
        };
        let err = r.add("calc.py", 9, "bug", "x", "codeit").unwrap_err().to_string();
        assert!(err.contains("Line 9 of calc.py is not in the diff") && err.contains("1-3"), "{err}");
        assert!(r.add("other.py", 1, "bug", "x", "codeit").is_err());
        let c = r.add("calc.py", 2, "bug", "  sign flipped ", "codeit").unwrap();
        assert_eq!((c.line, c.body.as_str(), c.verdict), (2, "sign flipped", Verdict::Open));
    }
}
