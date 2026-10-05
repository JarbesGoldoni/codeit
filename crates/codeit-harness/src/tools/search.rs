//! grep and glob, built in (no ripgrep needed). Both skip .git and what .gitignore ignores, and
//! print paths relative to the working directory, grouped, to keep results short.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use codeit_providers::ToolSpec;
use serde_json::{Value, json};

use super::{Ctx, Output, Tool, arg, spec};
use crate::{Harness, util};

const MAX_MATCHES: usize = 100;
const MAX_FILES: usize = 200;
const MAX_LINE: usize = 250;

fn walker(root: &Path) -> ignore::Walk {
    ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .require_git(false)
        .filter_entry(|e| e.file_name() != ".git")
        .sort_by_file_path(|a, b| a.cmp(b))
        .build()
}

/// Matches a glob the way ripgrep does: without `/` it matches the file name at any depth.
fn matcher(pattern: &str) -> Result<(globset::GlobMatcher, bool)> {
    let by_path = pattern.contains('/');
    let glob = globset::GlobBuilder::new(pattern)
        .literal_separator(by_path)
        .build()
        .map_err(|e| anyhow!("invalid glob `{pattern}`: {e}"))?;
    Ok((glob.compile_matcher(), by_path))
}

fn glob_match(m: &(globset::GlobMatcher, bool), rel: &Path) -> bool {
    if m.1 { m.0.is_match(rel) } else { rel.file_name().is_some_and(|n| m.0.is_match(n)) }
}

async fn search_root(ctx: &Ctx, input: &Value) -> Result<PathBuf> {
    let root = match input["path"].as_str() {
        Some(p) if !p.is_empty() => ctx.resolve(p),
        _ => ctx.cwd().to_path_buf(),
    };
    if !root.exists() {
        bail!("{} does not exist.", ctx.display(&root));
    }
    ctx.check_path(&root, "read", None).await?;
    Ok(root)
}

pub struct Grep;

#[async_trait]
impl Tool for Grep {
    fn name(&self) -> &str {
        "grep"
    }

    fn spec(&self, _: &Harness) -> ToolSpec {
        spec(
            "grep",
            "Search file contents with a regular expression (Rust regex syntax; `(?i)` for case-insensitive). \
Returns matching lines grouped by file, with line numbers. Filter files with include (a glob like `*.rs` or `*.{ts,tsx}`). \
Skips .gitignored files.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regular expression" },
                    "path": { "type": "string", "description": "File or directory to search (default: the working directory)" },
                    "include": { "type": "string", "description": "Only files matching this glob" }
                },
                "required": ["pattern"]
            }),
        )
    }

    fn read_only(&self) -> bool {
        true
    }

    fn title(&self, input: &Value, cwd: &Path) -> String {
        let mut t = format!("\"{}\"", input["pattern"].as_str().unwrap_or_default());
        if let Some(p) = input["path"].as_str() {
            t.push_str(&format!(" in {}", util::display(cwd, &util::resolve(cwd, p))));
        }
        if let Some(i) = input["include"].as_str() {
            t.push_str(&format!(" ({i})"));
        }
        t
    }

    async fn run(&self, input: Value, ctx: &Ctx) -> Result<Output> {
        let pattern = arg(&input, "pattern")?.to_string();
        let re = regex::Regex::new(&pattern).map_err(|e| anyhow!("invalid regex: {e}"))?;
        let include = input["include"].as_str().map(matcher).transpose()?;
        let root = search_root(ctx, &input).await?;
        let cwd = ctx.cwd().to_path_buf();
        let cancel = ctx.cancel.clone();
        let text = tokio::task::spawn_blocking(move || grep(&re, &root, include.as_ref(), &cwd, &cancel)).await??;
        Ok(Output::text(text))
    }
}

fn grep(
    re: &regex::Regex,
    root: &Path,
    include: Option<&(globset::GlobMatcher, bool)>,
    cwd: &Path,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<String> {
    let mut out = String::new();
    let mut shown = 0;
    let mut total = 0;
    let mut files = 0;
    for entry in walker(root).flatten() {
        if cancel.is_cancelled() {
            break;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let rel = path.strip_prefix(root).unwrap_or(path);
        if include.is_some_and(|m| !glob_match(m, rel)) {
            continue;
        }
        if entry.metadata().is_ok_and(|m| m.len() > 5_000_000) {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else { continue };
        if bytes.iter().take(8192).any(|b| *b == 0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let mut header = false;
        for (i, line) in text.lines().enumerate() {
            if !re.is_match(line) {
                continue;
            }
            total += 1;
            if shown >= MAX_MATCHES {
                continue;
            }
            if !header {
                out.push_str(&util::display(cwd, path));
                out.push('\n');
                header = true;
                files += 1;
            }
            out.push_str(&format!("  {}: {}\n", i + 1, util::cut_line(line.trim_end(), MAX_LINE)));
            shown += 1;
        }
    }
    if total == 0 {
        return Ok("No matches.".into());
    }
    if total > shown {
        out.push_str(&format!(
            "({total} matches; showed the first {shown} in {files} files. Narrow the pattern, path or include.)"
        ));
    }
    Ok(out.trim_end().to_string())
}

pub struct Glob;

#[async_trait]
impl Tool for Glob {
    fn name(&self) -> &str {
        "glob"
    }

    fn spec(&self, _: &Harness) -> ToolSpec {
        spec(
            "glob",
            "Find files by name pattern, like `**/*.rs`, `src/**/test_*.py` or `Cargo.toml` (a pattern without / \
matches file names at any depth). Returns paths, most recently modified first. Skips .gitignored files.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string", "description": "Directory to search (default: the working directory)" }
                },
                "required": ["pattern"]
            }),
        )
    }

    fn read_only(&self) -> bool {
        true
    }

    fn title(&self, input: &Value, cwd: &Path) -> String {
        let mut t = input["pattern"].as_str().unwrap_or_default().to_string();
        if let Some(p) = input["path"].as_str() {
            t.push_str(&format!(" in {}", util::display(cwd, &util::resolve(cwd, p))));
        }
        t
    }

    async fn run(&self, input: Value, ctx: &Ctx) -> Result<Output> {
        let m = matcher(arg(&input, "pattern")?)?;
        let root = search_root(ctx, &input).await?;
        let cwd = ctx.cwd().to_path_buf();
        let text = tokio::task::spawn_blocking(move || {
            let mut found: Vec<(std::time::SystemTime, PathBuf)> = walker(&root)
                .flatten()
                .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
                .filter(|e| glob_match(&m, e.path().strip_prefix(&root).unwrap_or(e.path())))
                .map(|e| {
                    let t = e.metadata().ok().and_then(|m| m.modified().ok()).unwrap_or(std::time::UNIX_EPOCH);
                    (t, e.into_path())
                })
                .collect();
            found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            let total = found.len();
            let mut out: Vec<String> = found.iter().take(MAX_FILES).map(|(_, p)| util::display(&cwd, p)).collect();
            if total == 0 {
                return "No files found.".to_string();
            }
            if total > MAX_FILES {
                out.push(format!("({total} files; showed {MAX_FILES}. Use a narrower pattern or path.)"));
            }
            out.join("\n")
        })
        .await?;
        Ok(Output::text(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greps_grouped_and_respects_gitignore() {
        let d = std::env::temp_dir().join(format!("codeit-grep-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::create_dir_all(d.join("target")).unwrap();
        std::fs::write(d.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(d.join("src/a.rs"), "fn foo() {}\nfn bar() { foo() }\n").unwrap();
        std::fs::write(d.join("target/x.rs"), "fn foo() {}\n").unwrap();
        let re = regex::Regex::new("foo").unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        let out = grep(&re, &d, None, &d, &cancel).unwrap();
        assert_eq!(out, "src/a.rs\n  1: fn foo() {}\n  2: fn bar() { foo() }");
        let only_md = matcher("*.md").unwrap();
        assert_eq!(grep(&re, &d, Some(&only_md), &d, &cancel).unwrap(), "No matches.");
        let m = matcher("*.rs").unwrap();
        assert!(glob_match(&m, Path::new("src/deep/a.rs")));
        let m = matcher("src/*.rs").unwrap();
        assert!(!glob_match(&m, Path::new("src/deep/a.rs")));
        let _ = std::fs::remove_dir_all(&d);
    }
}
