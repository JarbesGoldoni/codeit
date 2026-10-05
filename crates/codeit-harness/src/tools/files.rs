//! read and write.

use std::path::Path;

use anyhow::{Result, bail};
use async_trait::async_trait;
use codeit_providers::ToolSpec;
use serde_json::{Value, json};

use super::{Ctx, Output, Tool, arg, opt_u64, spec};
use crate::session::mtime;
use crate::{Harness, instructions, util};

const MAX_LINES: usize = 2000;
const MAX_BYTES: usize = 50 * 1024;
const MAX_LINE: usize = 2000;
const MAX_ENTRIES: usize = 500;

pub struct Read;

#[async_trait]
impl Tool for Read {
    fn name(&self) -> &str {
        "read"
    }

    fn spec(&self, _: &Harness) -> ToolSpec {
        spec(
            "read",
            "Read a file, or list a directory. Lines come back as `N: content` (N is the line number, not part of the file). \
Reads up to 2000 lines; use offset and limit for other parts of large files, after finding the right place with grep. \
Read several files at once by calling this tool in parallel.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File or directory path" },
                    "offset": { "type": "integer", "description": "First line to read (1-based)" },
                    "limit": { "type": "integer", "description": "Number of lines to read" }
                },
                "required": ["path"]
            }),
        )
    }

    fn read_only(&self) -> bool {
        true
    }

    fn title(&self, input: &Value, cwd: &Path) -> String {
        let path = input["path"].as_str().map(|p| util::display(cwd, &util::resolve(cwd, p))).unwrap_or_default();
        match (opt_u64(input, "offset"), opt_u64(input, "limit")) {
            (Some(o), Some(l)) => format!("{path}:{o}-{}", o + l.saturating_sub(1)),
            (Some(o), None) => format!("{path}:{o}-"),
            _ => path,
        }
    }

    async fn run(&self, input: Value, ctx: &Ctx) -> Result<Output> {
        let path = ctx.resolve(arg(&input, "path")?);
        ctx.check_path(&path, "read", None).await?;
        if path.is_dir() {
            return Ok(Output::text(list_dir(&path)?));
        }
        if !path.exists() {
            bail!("{} does not exist.{}", ctx.display(&path), suggest(&path));
        }
        // Images go to the model as images.
        if super::image_type(&path).is_some() {
            let (kind, data) = super::read_image(&path)?;
            return Ok(Output {
                content: format!("Image {} ({kind}), attached.", ctx.display(&path)),
                images: vec![(kind, data)],
                ..Default::default()
            });
        }
        let offset = opt_u64(&input, "offset").unwrap_or(1).max(1) as usize;
        let limit = opt_u64(&input, "limit").map(|l| l as usize).unwrap_or(MAX_LINES).clamp(1, MAX_LINES);

        // Same range of an unchanged file, still in context: point at the earlier result.
        {
            let s = ctx.session.lock().unwrap();
            if let Some(mark) = s.files.get(&path)
                && mark.mtime == mtime(&path)
                && let Some((o, l, call)) = &mark.read
                && *o == offset
                && *l == limit
                && s.result_visible(call)
            {
                return Ok(Output::text(format!(
                    "{} is unchanged since you last read these lines; that earlier result is still current.",
                    ctx.display(&path)
                )));
            }
        }

        let mut text = read_for_model(&path, offset, limit)?;
        let nested = {
            let s = ctx.session.lock().unwrap();
            let mut seen = s.instructions.clone();
            seen.extend(ctx.harness.instruction_files());
            instructions::nested(&path, ctx.cwd(), &seen)
        };
        for p in &nested {
            if let Ok(rules) = std::fs::read_to_string(p) {
                text.push_str(&format!(
                    "\n\n<instructions from=\"{}\">\nThese instructions apply to files under this folder.\n{}\n</instructions>",
                    ctx.display(p),
                    rules.trim()
                ));
            }
        }
        let mut s = ctx.session.lock().unwrap();
        s.instructions.extend(nested);
        s.mark_known(&path, Some((offset, limit, ctx.call_id.clone())));
        Ok(Output::text(text))
    }
}

/// A file with line numbers, from `offset` (1-based) for at most `limit` lines and 50 KB.
pub fn read_for_model(path: &Path, offset: usize, limit: usize) -> Result<String> {
    let bytes = std::fs::read(path)?;
    if bytes.iter().take(8192).any(|b| *b == 0) {
        bail!("{} is a binary file ({} bytes); it can't be shown as text.", path.display(), bytes.len());
    }
    let text = String::from_utf8_lossy(&bytes);
    if text.is_empty() {
        return Ok("(empty file)".into());
    }
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    if offset > total {
        bail!("offset {offset} is past the end of the file ({total} lines).");
    }
    let mut out = String::new();
    let mut last = offset - 1;
    for (i, line) in lines.iter().enumerate().skip(offset - 1).take(limit) {
        let line = util::cut_line(line, MAX_LINE);
        if out.len() + line.len() > MAX_BYTES {
            break;
        }
        out.push_str(&format!("{}: {line}\n", i + 1));
        last = i + 1;
    }
    if last < total {
        out.push_str(&format!("(File has {total} lines; showed {offset}-{last}. Use offset={} to read on.)", last + 1));
    } else {
        out.pop();
    }
    Ok(out)
}

fn list_dir(dir: &Path) -> Result<String> {
    let mut entries: Vec<String> = std::fs::read_dir(dir)?
        .flatten()
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.file_type().is_ok_and(|t| t.is_dir()) { format!("{name}/") } else { name }
        })
        .collect();
    entries.sort();
    let total = entries.len();
    entries.truncate(MAX_ENTRIES);
    let mut out = entries.join("\n");
    if total > MAX_ENTRIES {
        out.push_str(&format!("\n({total} entries; showed {MAX_ENTRIES}. Use glob to narrow down.)"));
    }
    if total == 0 {
        out = "(empty directory)".into();
    }
    Ok(out)
}

/// "Did you mean" for a missing file: siblings with a similar name.
fn suggest(path: &Path) -> String {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else { return String::new() };
    let name = name.to_string_lossy().to_lowercase();
    let stem = name.split('.').next().unwrap_or(&name).to_string();
    let Ok(read) = std::fs::read_dir(dir) else {
        return " Its folder doesn't exist either.".into();
    };
    let close: Vec<String> = read
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            let l = n.to_lowercase();
            !stem.is_empty() && (l.contains(&stem) || stem.contains(l.split('.').next().unwrap_or("\u{0}")))
        })
        .take(5)
        .collect();
    if close.is_empty() { String::new() } else { format!(" Did you mean: {}?", close.join(", ")) }
}

pub struct Write;

#[async_trait]
impl Tool for Write {
    fn name(&self) -> &str {
        "write"
    }

    fn permission(&self) -> &str {
        "edit"
    }

    fn spec(&self, _: &Harness) -> ToolSpec {
        spec(
            "write",
            "Create a file, or replace a file's entire content. Parent folders are created. \
To change part of an existing file use edit instead; an existing file must have been read first.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string", "description": "The complete new content" }
                },
                "required": ["path", "content"]
            }),
        )
    }

    fn title(&self, input: &Value, cwd: &Path) -> String {
        input["path"].as_str().map(|p| util::display(cwd, &util::resolve(cwd, p))).unwrap_or_default()
    }

    async fn run(&self, input: Value, ctx: &Ctx) -> Result<Output> {
        let path = ctx.resolve(arg(&input, "path")?);
        let content = arg(&input, "content")?;
        if path.is_dir() {
            bail!("{} is a directory.", ctx.display(&path));
        }
        let old = if path.exists() {
            check_fresh(ctx, &path)?;
            Some(std::fs::read_to_string(&path)?)
        } else {
            None
        };
        let shown = ctx.display(&path);
        let diff = super::edit::diff(&shown, old.as_deref().unwrap_or(""), content);
        ctx.check_path(&path, "edit", Some(diff.clone())).await?;
        ctx.session.lock().unwrap().record_change(ctx.entry, &path);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, content)?;
        ctx.session.lock().unwrap().mark_known(&path, None);
        let lines = content.lines().count();
        let verb = if old.is_some() { "Overwrote" } else { "Created" };
        Ok(Output {
            content: format!("{verb} {shown} ({lines} lines)."),
            display: None,
            diff: Some(diff),
            touched: vec![path.clone()],
            ..Default::default()
        })
    }
}

/// An existing file may only be changed after the model has seen its current version.
pub fn check_fresh(ctx: &Ctx, path: &Path) -> Result<()> {
    let s = ctx.session.lock().unwrap();
    match s.files.get(path) {
        None => bail!("Read {} before changing it.", ctx.display(path)),
        Some(m) if m.mtime != mtime(path) => {
            bail!("{} changed on disk since you last read it. Read it again before changing it.", ctx.display(path))
        }
        Some(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_lines_and_pages() {
        let dir = std::env::temp_dir().join(format!("codeit-read-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.txt");
        std::fs::write(&f, "one\ntwo\nthree\n").unwrap();
        assert_eq!(read_for_model(&f, 1, 2000).unwrap(), "1: one\n2: two\n3: three");
        assert_eq!(
            read_for_model(&f, 2, 1).unwrap(),
            "2: two\n(File has 3 lines; showed 2-2. Use offset=3 to read on.)"
        );
        assert!(read_for_model(&f, 9, 1).is_err());
        std::fs::write(dir.join("b.bin"), [0u8, 1, 2]).unwrap();
        assert!(read_for_model(&dir.join("b.bin"), 1, 10).is_err());
        assert_eq!(list_dir(&dir).unwrap(), "a.txt\nb.bin");
        assert_eq!(suggest(&dir.join("a.md")), " Did you mean: a.txt?");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
