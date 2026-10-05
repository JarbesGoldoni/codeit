//! edit: exact string replacement, with fallbacks for the near misses models make (indentation,
//! trailing spaces, escaped characters), and a pointer to the closest text when nothing matches.

use std::path::Path;

use anyhow::{Result, bail};
use async_trait::async_trait;
use codeit_providers::ToolSpec;
use serde_json::{Value, json};

use super::files::check_fresh;
use super::{Ctx, Output, Tool, arg, spec};
use crate::{Harness, util};

pub struct Edit;

#[async_trait]
impl Tool for Edit {
    fn name(&self) -> &str {
        "edit"
    }

    fn permission(&self) -> &str {
        "edit"
    }

    fn spec(&self, _: &Harness) -> ToolSpec {
        spec(
            "edit",
            "Replace exact text in a file. Read the file first. old_string must match the file exactly, \
without the `N: ` line-number prefix of read output, and must be unique in the file: include enough surrounding \
lines, or set replace_all to change every occurrence (renames). For several changes to one file, pass them all in \
`edits` in one call. An empty old_string creates a new file with new_string as content. The result shows the edited \
lines, so there is no need to read the file again.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_string": { "type": "string", "description": "Text to replace" },
                    "new_string": { "type": "string", "description": "Replacement text" },
                    "replace_all": { "type": "boolean", "description": "Replace every occurrence" },
                    "edits": {
                        "type": "array",
                        "description": "Several replacements, applied in order (instead of old_string/new_string)",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_string": { "type": "string" },
                                "new_string": { "type": "string" },
                                "replace_all": { "type": "boolean" }
                            },
                            "required": ["old_string", "new_string"]
                        }
                    }
                },
                "required": ["path"]
            }),
        )
    }

    fn title(&self, input: &Value, cwd: &Path) -> String {
        input["path"].as_str().map(|p| util::display(cwd, &util::resolve(cwd, p))).unwrap_or_default()
    }

    async fn run(&self, input: Value, ctx: &Ctx) -> Result<Output> {
        let path = ctx.resolve(arg(&input, "path")?);
        let shown = ctx.display(&path);
        let mut edits: Vec<(String, String, bool)> = Vec::new();
        if let Some(list) = input["edits"].as_array() {
            for e in list {
                edits.push((arg(e, "old_string")?.into(), arg(e, "new_string")?.into(), e["replace_all"] == true));
            }
        }
        if let (Some(o), Some(n)) = (input["old_string"].as_str(), input["new_string"].as_str()) {
            edits.insert(0, (o.into(), n.into(), input["replace_all"] == true));
        }
        if edits.is_empty() {
            bail!("pass old_string and new_string, or a list of edits");
        }

        // An empty old_string on a missing file creates it.
        if !path.exists() {
            if edits.len() == 1 && edits[0].0.is_empty() {
                let content = edits[0].1.clone();
                let diff = diff(&shown, "", &content);
                ctx.check_path(&path, "edit", Some(diff.clone())).await?;
                ctx.session.lock().unwrap().record_change(ctx.entry, &path);
                if let Some(d) = path.parent() {
                    std::fs::create_dir_all(d)?;
                }
                std::fs::write(&path, &content)?;
                ctx.session.lock().unwrap().mark_known(&path, None);
                return Ok(Output {
                    content: format!("Created {shown} ({} lines).", content.lines().count()),
                    display: None,
                    diff: Some(diff),
                    images: Vec::new(),
                    touched: vec![path.clone()],
                });
            }
            bail!("{shown} does not exist. To create it, use write (or an empty old_string).");
        }
        check_fresh(ctx, &path)?;
        let original = std::fs::read_to_string(&path)?;
        let crlf = original.contains("\r\n");
        let mut content = original.replace("\r\n", "\n");
        let mut changed: Vec<(usize, usize)> = Vec::new();
        let mut count = 0;
        for (i, (old, new, all)) in edits.iter().enumerate() {
            let label = if edits.len() > 1 { format!("edit {}: ", i + 1) } else { String::new() };
            if old.is_empty() {
                bail!("{label}old_string is empty; the file already exists (use write to replace all of it).");
            }
            if old == new {
                bail!("{label}old_string and new_string are the same.");
            }
            let (next, n, range) = replace(&content, &old.replace("\r\n", "\n"), &new.replace("\r\n", "\n"), *all)
                .map_err(|e| anyhow::anyhow!("{label}{e}"))?;
            content = next;
            count += n;
            changed.push(range);
        }
        let diff = diff(&shown, &original.replace("\r\n", "\n"), &content);
        ctx.check_path(&path, "edit", Some(diff.clone())).await?;
        ctx.session.lock().unwrap().record_change(ctx.entry, &path);
        let out = if crlf { content.replace('\n', "\r\n") } else { content.clone() };
        std::fs::write(&path, out)?;
        ctx.session.lock().unwrap().mark_known(&path, None);

        let summary =
            if count == 1 { format!("Edited {shown}.") } else { format!("Edited {shown} ({count} replacements).") };
        let snippet = if edits.len() == 1 && count == 1 { snippet(&content, changed[0]) } else { None };
        let text = match snippet {
            Some(s) => format!("{summary} The changed lines now read:\n{s}"),
            None => summary,
        };
        Ok(Output { content: text, display: None, diff: Some(diff), touched: vec![path.clone()], ..Default::default() })
    }
}

/// A unified diff for display.
pub fn diff(path: &str, old: &str, new: &str) -> String {
    similar::TextDiff::from_lines(old, new).unified_diff().context_radius(3).header(path, path).to_string()
}

/// The changed line range with 3 lines of context, numbered like read output (at most 40 lines).
fn snippet(content: &str, (start, end): (usize, usize)) -> Option<String> {
    let first = content[..start].matches('\n').count();
    let last = first + content[start..end.max(start)].matches('\n').count();
    if last - first > 34 {
        return None;
    }
    let lines: Vec<&str> = content.lines().collect();
    let from = first.saturating_sub(3);
    let to = (last + 4).min(lines.len());
    Some(
        lines[from..to]
            .iter()
            .enumerate()
            .map(|(i, l)| format!("{}: {l}", from + i + 1))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Replaces `old` with `new` in `content`. Tries an exact match first, then the fallbacks.
/// Returns the new content, the number of replacements, and the byte range of the last one.
pub fn replace(content: &str, old: &str, new: &str, all: bool) -> Result<(String, usize, (usize, usize))> {
    let exact = content.matches(old).count();
    if exact > 0 {
        if exact > 1 && !all {
            bail!(
                "old_string occurs {exact} times. Include more surrounding lines to pick one, or set replace_all to change them all."
            );
        }
        let start = content.find(old).unwrap_or(0);
        let range = if all { (0, content.len()) } else { (start, start + new.len()) };
        return Ok((content.replace(old, new), exact, range));
    }
    for strategy in [line_trimmed as Finder, indentation_free, whitespace_free, unescaped, block_anchor] {
        let found = strategy(content, old);
        if found.is_empty() {
            continue;
        }
        if found.len() > 1 && !all {
            bail!(
                "old_string did not match exactly and matches {} places loosely. Copy the exact text from the file, with more context.",
                found.len()
            );
        }
        let mut out = String::new();
        let mut last = 0;
        let mut range = (0, 0);
        for (s, e) in &found {
            out.push_str(&content[last..*s]);
            let replacement = reindent(&content[*s..*e], old, new);
            range = (out.len(), out.len() + replacement.len());
            out.push_str(&replacement);
            last = *e;
        }
        out.push_str(&content[last..]);
        return Ok((out, found.len(), range));
    }
    bail!("old_string was not found.{}", closest(content, old))
}

type Finder = fn(&str, &str) -> Vec<(usize, usize)>;

/// Byte offset of each line start, plus the end.
fn line_starts(content: &str) -> Vec<usize> {
    let mut v = vec![0];
    v.extend(content.match_indices('\n').map(|(i, _)| i + 1));
    if *v.last().unwrap() != content.len() {
        v.push(content.len());
    }
    v
}

/// Blocks of whole lines whose lines match `old`'s lines under `eq`.
fn match_lines(content: &str, old: &str, eq: impl Fn(&str, &str) -> bool) -> Vec<(usize, usize)> {
    let want: Vec<&str> = old.trim_end_matches('\n').split('\n').collect();
    let starts = line_starts(content);
    let lines: Vec<&str> = content.split('\n').collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + want.len() <= lines.len() {
        if want.iter().enumerate().all(|(k, w)| eq(lines[i + k], w)) {
            let s = starts[i];
            let last = i + want.len() - 1;
            // End of the last line, without its newline.
            let e = (starts[last] + lines[last].len()).min(content.len());
            out.push((s, e));
            i += want.len();
        } else {
            i += 1;
        }
    }
    out
}

fn line_trimmed(content: &str, old: &str) -> Vec<(usize, usize)> {
    match_lines(content, old, |a, b| a.trim() == b.trim())
}

fn indentation_free(content: &str, old: &str) -> Vec<(usize, usize)> {
    match_lines(content, old, |a, b| a.trim_start() == b.trim_start())
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn whitespace_free(content: &str, old: &str) -> Vec<(usize, usize)> {
    match_lines(content, old, |a, b| collapse(a) == collapse(b))
}

/// The model escaped characters that are literal in the file (`\n`, `\"`, `\t`).
fn unescaped(content: &str, old: &str) -> Vec<(usize, usize)> {
    if !old.contains('\\') {
        return Vec::new();
    }
    let u =
        old.replace("\\n", "\n").replace("\\t", "\t").replace("\\\"", "\"").replace("\\'", "'").replace("\\\\", "\\");
    if u == old {
        return Vec::new();
    }
    content.match_indices(&u).map(|(i, m)| (i, i + m.len())).collect()
}

/// Blocks of 3+ lines whose first and last lines match and whose middle is very similar.
fn block_anchor(content: &str, old: &str) -> Vec<(usize, usize)> {
    let want: Vec<&str> = old.trim_end_matches('\n').split('\n').collect();
    if want.len() < 3 {
        return Vec::new();
    }
    let lines: Vec<&str> = content.split('\n').collect();
    let starts = line_starts(content);
    let (first, last) = (want[0].trim(), want[want.len() - 1].trim());
    let mut out = Vec::new();
    for i in 0..lines.len() {
        if lines[i].trim() != first {
            continue;
        }
        // Allow the block to be a little shorter or longer.
        let lo = i + want.len().saturating_sub(2).max(2) - 1;
        let hi = (i + want.len() + 2).min(lines.len());
        for j in lo..hi {
            if j >= lines.len() || lines[j].trim() != last {
                continue;
            }
            let a = lines[i + 1..j].iter().map(|l| l.trim()).collect::<Vec<_>>().join("\n");
            let b = want[1..want.len() - 1].iter().map(|l| l.trim()).collect::<Vec<_>>().join("\n");
            if similarity(&a, &b) >= 0.85 {
                out.push((starts[i], starts[j] + lines[j].len()));
                break;
            }
        }
    }
    out
}

fn similarity(a: &str, b: &str) -> f64 {
    let max = a.chars().count().max(b.chars().count());
    if max == 0 {
        return 1.0;
    }
    1.0 - levenshtein(a, b) as f64 / max as f64
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

fn indent(line: &str) -> &str {
    &line[..line.len() - line.trim_start().len()]
}

/// When the match was found despite different indentation, shifts `new` by the same amount,
/// so the replacement lines up with the file.
fn reindent(actual: &str, old: &str, new: &str) -> String {
    let first = |s: &str| s.lines().find(|l| !l.trim().is_empty()).map(|l| indent(l).to_string());
    let (Some(have), Some(gave)) = (first(actual), first(old)) else { return new.to_string() };
    if have == gave {
        return new.to_string();
    }
    new.split('\n')
        .map(|l| {
            if l.trim().is_empty() {
                l.to_string()
            } else if let Some(rest) = l.strip_prefix(gave.as_str()) {
                format!("{have}{rest}")
            } else if gave.is_empty() {
                format!("{have}{l}")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Shows the lines most like old_string's first line, so the model can fix its text without
/// reading the whole file again.
fn closest(content: &str, old: &str) -> String {
    let Some(want) = old.lines().map(str::trim).find(|l| !l.is_empty()) else { return String::new() };
    let lines: Vec<&str> = content.lines().collect();
    let best = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| (i, similarity(l.trim(), want)))
        .max_by(|a, b| a.1.total_cmp(&b.1));
    match best {
        Some((i, score)) if score > 0.5 => {
            let n = old.lines().count().max(1);
            let to = (i + n + 1).min(lines.len());
            let from = i.saturating_sub(1);
            let shown: Vec<String> = (from..to).map(|k| format!("{}: {}", k + 1, lines[k])).collect();
            format!(" The closest text in the file is at line {}:\n{}", i + 1, shown.join("\n"))
        }
        _ => " Read the file again to get its exact current text.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_and_unique() {
        let (out, n, _) = replace("a b a", "b", "c", false).unwrap();
        assert_eq!((out.as_str(), n), ("a c a", 1));
        assert!(replace("a b a", "a", "c", false).unwrap_err().to_string().contains("occurs 2 times"));
        assert_eq!(replace("a b a", "a", "c", true).unwrap().0, "c b c");
    }

    #[test]
    fn tolerates_indentation_and_reindents() {
        let file = "fn main() {\n    if x {\n        go();\n    }\n}\n";
        // The model dropped the indentation.
        let (out, _, _) = replace(file, "if x {\n    go();\n}", "if y {\n    stop();\n}", false).unwrap();
        assert_eq!(out, "fn main() {\n    if y {\n        stop();\n    }\n}\n");
        // Trailing whitespace differences.
        let (out, _, _) = replace("a  \nb\n", "a\nb", "c\nd", false).unwrap();
        assert_eq!(out, "c\nd\n");
    }

    #[test]
    fn unescapes_and_anchors() {
        let (out, _, _) = replace("say \"hi\"\n", "say \\\"hi\\\"", "say \"yo\"", false).unwrap();
        assert_eq!(out, "say \"yo\"\n");
        let file = "start\n  one\n  two  three\nend\n";
        let (out, _, _) = replace(file, "start\n  one\n  two three\nend", "X", false).unwrap();
        assert_eq!(out, "X\n");
    }

    #[test]
    fn points_at_the_closest_line() {
        let err = replace("let alpha = 1;\nlet beta = 2;\n", "let betta = 2;", "x", false).unwrap_err().to_string();
        assert!(err.contains("line 2"), "{err}");
    }

    #[test]
    fn snippet_shows_context() {
        let content = "1\n2\n3\n4\nX\n6\n7\n8\n9\n";
        let start = content.find('X').unwrap();
        assert_eq!(snippet(content, (start, start + 1)).unwrap(), "2: 2\n3: 3\n4: 4\n5: X\n6: 6\n7: 7\n8: 8");
    }
}
