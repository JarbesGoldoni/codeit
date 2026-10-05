//! apply_patch: the file-oriented patch format GPT models are trained on (from Codex).
//! Context lines are located with decreasing strictness (exact, then ignoring trailing
//! whitespace, then all surrounding whitespace, then typographic punctuation).

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use codeit_providers::ToolSpec;
use serde_json::{Value, json};

use super::{Ctx, Output, Tool, arg, spec};
use crate::Harness;

const DESCRIPTION: &str = r#"Edit files with a patch. The patch is plain text in this envelope:

*** Begin Patch
[one or more file operations]
*** End Patch

Operations:
*** Add File: <path>      then every line of the new file, each prefixed with +
*** Delete File: <path>
*** Update File: <path>   optionally followed by *** Move to: <new path>, then hunks

A hunk starts with @@ (optionally followed by a line such as a function or class header that locates it), then
lines prefixed with ' ' (context), '-' (removed) or '+' (added). Give about 3 lines of context above and below
each change; use several @@ hunks for changes in different places of a file.

Example:
*** Begin Patch
*** Add File: hello.txt
+Hello world
*** Update File: src/app.py
*** Move to: src/main.py
@@ def greet():
-    print("Hi")
+    print("Hello, world!")
*** Delete File: obsolete.txt
*** End Patch

Paths are relative to the working directory. Every line in a hunk needs its prefix, also in new files."#;

pub struct ApplyPatch;

#[async_trait]
impl Tool for ApplyPatch {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn permission(&self) -> &str {
        "edit"
    }

    fn spec(&self, _: &Harness) -> ToolSpec {
        spec(
            "apply_patch",
            DESCRIPTION,
            json!({
                "type": "object",
                "properties": { "input": { "type": "string", "description": "The entire patch, from *** Begin Patch to *** End Patch" } },
                "required": ["input"]
            }),
        )
    }

    fn title(&self, input: &Value, _: &Path) -> String {
        let text = input["input"].as_str().unwrap_or_default();
        let files: Vec<&str> = text
            .lines()
            .filter_map(|l| {
                l.strip_prefix("*** Update File: ")
                    .or_else(|| l.strip_prefix("*** Add File: "))
                    .or_else(|| l.strip_prefix("*** Delete File: "))
            })
            .map(str::trim)
            .collect();
        files.join(", ")
    }

    async fn run(&self, input: Value, ctx: &Ctx) -> Result<Output> {
        let text = arg(&input, "input").or_else(|_| arg(&input, "patch"))?;
        let hunks = parse_patch(text)?;
        let changes = plan(&hunks, ctx.cwd())?;

        let mut diff = String::new();
        for c in &changes {
            let shown = ctx.display(&c.path);
            diff.push_str(&super::edit::diff(&shown, c.old.as_deref().unwrap_or(""), c.new.as_deref().unwrap_or("")));
        }
        for c in &changes {
            ctx.check_path(&c.path, "edit", Some(diff.clone())).await?;
            if let Some(to) = &c.move_to {
                ctx.check_path(to, "edit", None).await?;
            }
        }
        let mut summary = vec!["Success. Updated the following files:".to_string()];
        for c in &changes {
            {
                let mut s = ctx.session.lock().unwrap();
                s.record_change(ctx.entry, &c.path);
                if let Some(to) = &c.move_to {
                    s.record_change(ctx.entry, to);
                }
            }
            let target = c.move_to.as_ref().unwrap_or(&c.path);
            match &c.new {
                Some(text) => {
                    if let Some(d) = target.parent() {
                        std::fs::create_dir_all(d)?;
                    }
                    std::fs::write(target, text)?;
                    if c.move_to.is_some() {
                        std::fs::remove_file(&c.path)?;
                    }
                    ctx.session.lock().unwrap().mark_known(target, None);
                    let tag = if c.old.is_none() { "A" } else { "M" };
                    summary.push(format!("{tag} {}", ctx.display(target)));
                }
                None => {
                    std::fs::remove_file(&c.path)?;
                    summary.push(format!("D {}", ctx.display(&c.path)));
                }
            }
        }
        let touched =
            changes.iter().filter(|c| c.new.is_some()).map(|c| c.move_to.clone().unwrap_or(c.path.clone())).collect();
        Ok(Output { content: summary.join("\n"), display: None, diff: Some(diff), touched, ..Default::default() })
    }
}

#[derive(Debug, PartialEq)]
pub enum Hunk {
    Add { path: String, contents: String },
    Delete { path: String },
    Update { path: String, move_to: Option<String>, chunks: Vec<Chunk> },
}

#[derive(Debug, Default, PartialEq)]
pub struct Chunk {
    /// The `@@ line` that locates the chunk.
    pub context: Option<String>,
    pub old: Vec<String>,
    pub new: Vec<String>,
    /// `*** End of File`: the chunk is at the end of the file.
    pub eof: bool,
}

pub fn parse_patch(text: &str) -> Result<Vec<Hunk>> {
    let lines: Vec<&str> = text.trim().lines().map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    // Tolerate a heredoc wrapper (`apply_patch <<'EOF' ... EOF`).
    let start = lines
        .iter()
        .position(|l| l.trim() == "*** Begin Patch")
        .ok_or_else(|| anyhow!("The patch must start with `*** Begin Patch`."))?;
    let end = lines.iter().rposition(|l| l.trim() == "*** End Patch").unwrap_or(lines.len());
    if end <= start {
        bail!("The patch must end with `*** End Patch`.");
    }
    let body = &lines[start + 1..end];
    let mut hunks = Vec::new();
    let mut i = 0;
    while i < body.len() {
        let line = body[i];
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            i += 1;
            let mut contents = String::new();
            while i < body.len() && !body[i].starts_with("*** ") {
                let l = body[i];
                let l = l.strip_prefix('+').ok_or_else(|| {
                    anyhow!("line {}: every line of an added file must start with `+`: {l:?}", start + i + 2)
                })?;
                contents.push_str(l);
                contents.push('\n');
                i += 1;
            }
            hunks.push(Hunk::Add { path: path.trim().into(), contents });
        } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
            hunks.push(Hunk::Delete { path: path.trim().into() });
            i += 1;
        } else if let Some(path) = line.strip_prefix("*** Update File: ") {
            i += 1;
            let mut move_to = None;
            if let Some(to) = body.get(i).and_then(|l| l.strip_prefix("*** Move to: ")) {
                move_to = Some(to.trim().to_string());
                i += 1;
            }
            let mut chunks: Vec<Chunk> = Vec::new();
            while i < body.len() {
                let l = body[i];
                if l.starts_with("*** ") && l.trim() != "*** End of File" {
                    break;
                }
                if l.trim() == "*** End of File" {
                    if let Some(c) = chunks.last_mut() {
                        c.eof = true;
                    }
                } else if let Some(ctx) = l.strip_prefix("@@") {
                    let ctx = ctx.trim();
                    chunks.push(Chunk { context: (!ctx.is_empty()).then(|| ctx.to_string()), ..Default::default() });
                } else {
                    if chunks.is_empty() {
                        chunks.push(Chunk::default());
                    }
                    let c = chunks.last_mut().expect("a chunk");
                    match l.chars().next() {
                        Some('+') => c.new.push(l[1..].into()),
                        Some('-') => c.old.push(l[1..].into()),
                        Some(' ') => {
                            c.old.push(l[1..].into());
                            c.new.push(l[1..].into());
                        }
                        None => {
                            c.old.push(String::new());
                            c.new.push(String::new());
                        }
                        _ => bail!(
                            "line {}: in {path}, hunk lines must start with ' ', '-' or '+': {l:?}",
                            start + i + 2
                        ),
                    }
                }
                i += 1;
            }
            chunks.retain(|c| !(c.old.is_empty() && c.new.is_empty()));
            if chunks.is_empty() && move_to.is_none() {
                bail!("The update of {path} has no changes.");
            }
            hunks.push(Hunk::Update { path: path.trim().into(), move_to, chunks });
        } else if line.trim().is_empty() {
            i += 1;
        } else {
            bail!(
                "line {}: expected `*** Add File:`, `*** Delete File:` or `*** Update File:`, found {line:?}",
                start + i + 2
            );
        }
    }
    if hunks.is_empty() {
        bail!("The patch has no file operations.");
    }
    Ok(hunks)
}

/// One file's change, computed before anything is written.
#[derive(Debug)]
struct Change {
    path: PathBuf,
    old: Option<String>,
    /// `None`: delete.
    new: Option<String>,
    move_to: Option<PathBuf>,
}

fn plan(hunks: &[Hunk], cwd: &Path) -> Result<Vec<Change>> {
    let resolve = |p: &str| crate::util::resolve(cwd, p);
    let mut out = Vec::new();
    for h in hunks {
        match h {
            Hunk::Add { path, contents } => {
                let p = resolve(path);
                let old = std::fs::read_to_string(&p).ok();
                out.push(Change { path: p, old, new: Some(contents.clone()), move_to: None });
            }
            Hunk::Delete { path } => {
                let p = resolve(path);
                let old = std::fs::read_to_string(&p).map_err(|_| anyhow!("Can't delete {path}: it doesn't exist."))?;
                out.push(Change { path: p, old: Some(old), new: None, move_to: None });
            }
            Hunk::Update { path, move_to, chunks } => {
                let p = resolve(path);
                let old = std::fs::read_to_string(&p).map_err(|_| anyhow!("Can't update {path}: it doesn't exist."))?;
                let new = apply_chunks(&old, chunks, path)?;
                out.push(Change { path: p, old: Some(old), new: Some(new), move_to: move_to.as_deref().map(resolve) });
            }
        }
    }
    Ok(out)
}

/// Applies a patch to files on disk directly (for tests and tools outside a session).
pub fn apply_patch(text: &str, cwd: &Path) -> Result<Vec<PathBuf>> {
    let changes = plan(&parse_patch(text)?, cwd)?;
    let mut touched = Vec::new();
    for c in changes {
        let target = c.move_to.clone().unwrap_or(c.path.clone());
        match c.new {
            Some(text) => {
                if let Some(d) = target.parent() {
                    std::fs::create_dir_all(d)?;
                }
                std::fs::write(&target, text)?;
                if c.move_to.is_some() {
                    std::fs::remove_file(&c.path)?;
                }
            }
            None => std::fs::remove_file(&c.path)?,
        }
        touched.push(target);
    }
    Ok(touched)
}

fn apply_chunks(original: &str, chunks: &[Chunk], path: &str) -> Result<String> {
    let crlf = original.contains("\r\n");
    let normalized = original.replace("\r\n", "\n");
    let mut lines: Vec<String> = normalized.split('\n').map(String::from).collect();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let mut replacements: Vec<(usize, usize, Vec<String>)> = Vec::new();
    let mut at = 0;
    for c in chunks {
        if let Some(ctx) = &c.context {
            match seek(&lines, std::slice::from_ref(ctx), at, false) {
                Some(i) => at = i + 1,
                None => bail!("Couldn't find the context line `{ctx}` in {path}."),
            }
        }
        if c.old.is_empty() {
            replacements.push((lines.len(), 0, c.new.clone()));
            continue;
        }
        let mut old: &[String] = &c.old;
        let mut new: &[String] = &c.new;
        let mut found = seek(&lines, old, at, c.eof);
        if found.is_none() && old.last().is_some_and(String::is_empty) {
            old = &old[..old.len() - 1];
            if new.last().is_some_and(String::is_empty) {
                new = &new[..new.len() - 1];
            }
            found = seek(&lines, old, at, c.eof);
        }
        let Some(i) = found else {
            bail!("Couldn't find these lines in {path} (read the file and copy them exactly):\n{}", c.old.join("\n"));
        };
        replacements.push((i, old.len(), new.to_vec()));
        at = i + old.len();
    }
    replacements.sort_by_key(|r| r.0);
    for (start, len, new) in replacements.into_iter().rev() {
        lines.splice(start..(start + len).min(lines.len()), new);
    }
    let mut out = lines.join("\n");
    out.push('\n');
    Ok(if crlf { out.replace('\n', "\r\n") } else { out })
}

/// Finds `pattern` in `lines` at or after `start`, with decreasing strictness.
fn seek(lines: &[String], pattern: &[String], start: usize, eof: bool) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start);
    }
    if pattern.len() > lines.len() {
        return None;
    }
    let last = lines.len() - pattern.len();
    let from = if eof { last } else { start };
    type Eq<'a> = &'a dyn Fn(&str, &str) -> bool;
    let tests: [Eq; 4] =
        [&|a, b| a == b, &|a, b| a.trim_end() == b.trim_end(), &|a, b| a.trim() == b.trim(), &|a, b| {
            normalize_punct(a.trim()) == normalize_punct(b.trim())
        }];
    for eq in tests {
        for begin in [from, start] {
            for i in begin.min(last + 1)..=last {
                if pattern.iter().enumerate().all(|(k, p)| eq(&lines[i + k], p)) {
                    return Some(i);
                }
            }
            if !eof {
                break;
            }
        }
    }
    None
}

fn normalize_punct(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("codeit-patch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn adds_updates_moves_and_deletes() {
        let d = temp();
        std::fs::write(d.join("app.py"), "def greet():\n    print(\"Hi\")\n\nx = 1\n").unwrap();
        std::fs::write(d.join("old.txt"), "bye\n").unwrap();
        let patch = "*** Begin Patch\n*** Add File: new/hello.txt\n+Hello\n+world\n*** Update File: app.py\n*** Move to: main.py\n@@ def greet():\n-    print(\"Hi\")\n+    print(\"Hello\")\n*** Delete File: old.txt\n*** End Patch\n";
        apply_patch(patch, &d).unwrap();
        assert_eq!(std::fs::read_to_string(d.join("new/hello.txt")).unwrap(), "Hello\nworld\n");
        assert_eq!(
            std::fs::read_to_string(d.join("main.py")).unwrap(),
            "def greet():\n    print(\"Hello\")\n\nx = 1\n"
        );
        assert!(!d.join("app.py").exists());
        assert!(!d.join("old.txt").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn matches_loosely_and_keeps_crlf() {
        let out = apply_chunks(
            "a\r\nb  \r\nc\r\n",
            &[Chunk { context: None, old: vec!["b".into()], new: vec!["B".into()], eof: false }],
            "f",
        )
        .unwrap();
        assert_eq!(out, "a\r\nB\r\nc\r\n");
        let out = apply_chunks(
            "say \u{201C}hi\u{201D}\n",
            &[Chunk { old: vec!["say \"hi\"".into()], new: vec!["say yo".into()], ..Default::default() }],
            "f",
        )
        .unwrap();
        assert_eq!(out, "say yo\n");
    }

    #[test]
    fn reports_bad_patches() {
        assert!(parse_patch("nothing").is_err());
        let err = parse_patch("*** Begin Patch\n*** Update File: a\n@@\n?bad\n*** End Patch").unwrap_err();
        assert!(err.to_string().contains("must start with"), "{err}");
        let err = apply_chunks("a\n", &[Chunk { old: vec!["zzz".into()], new: vec![], ..Default::default() }], "f");
        assert!(err.unwrap_err().to_string().contains("Couldn't find"));
    }

    #[test]
    fn uses_context_and_end_of_file() {
        let src = "fn a() {\n    x();\n}\nfn b() {\n    x();\n}\n";
        let chunks = vec![Chunk {
            context: Some("fn b() {".into()),
            old: vec!["    x();".into()],
            new: vec!["    y();".into()],
            eof: false,
        }];
        assert_eq!(apply_chunks(src, &chunks, "f").unwrap(), "fn a() {\n    x();\n}\nfn b() {\n    y();\n}\n");
        let chunks =
            vec![Chunk { old: vec!["    x();".into(), "}".into()], new: vec!["}".into()], eof: true, context: None }];
        assert_eq!(apply_chunks(src, &chunks, "f").unwrap(), "fn a() {\n    x();\n}\nfn b() {\n}\n");
    }
}
