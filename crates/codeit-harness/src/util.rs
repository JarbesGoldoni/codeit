//! Small helpers shared by the harness: frontmatter, output trimming, paths, token estimates.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// Rough token count (4 characters per token), enough for budgeting the context.
pub fn tokens(text: &str) -> u64 {
    text.len().div_ceil(4) as u64
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Splits `---\nkey: value\n---\nbody` into its fields and body. Handles the YAML a skill or
/// command file uses: plain and quoted scalars, `>`/`|` block scalars, and `[a, b]` lists
/// (returned as the raw text inside the brackets).
pub fn frontmatter(text: &str) -> (BTreeMap<String, String>, String) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut fields = BTreeMap::new();
    let Some(rest) = text.strip_prefix("---\n").or_else(|| text.strip_prefix("---\r\n")) else {
        return (fields, text.to_string());
    };
    let Some(end) = rest.find("\n---") else { return (fields, text.to_string()) };
    let head = &rest[..end];
    let body = rest[end + 4..].trim_start_matches(['-']).trim_start_matches(['\r', '\n']).to_string();

    let lines: Vec<&str> = head.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        if line.starts_with([' ', '\t']) || line.trim_start().starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else { continue };
        let key = key.trim().to_string();
        let value = value.trim();
        let value = if matches!(value, ">" | "|" | ">-" | "|-" | ">+" | "|+") {
            let mut block = Vec::new();
            while i < lines.len() && (lines[i].starts_with([' ', '\t']) || lines[i].trim().is_empty()) {
                block.push(lines[i].trim());
                i += 1;
            }
            if value.starts_with('>') {
                block.join(" ").trim().to_string()
            } else {
                block.join("\n").trim().to_string()
            }
        } else if let Some(list) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
            list.to_string()
        } else {
            unquote(value)
        };
        fields.insert(key, value);
    }
    (fields, body)
}

fn unquote(v: &str) -> String {
    for q in ['"', '\''] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return v[1..v.len() - 1].replace("\\\"", "\"").replace("''", "'");
        }
    }
    v.to_string()
}

/// Removes ANSI escape sequences and resolves carriage returns (progress bars keep their last
/// state), so command output costs fewer tokens.
pub fn clean_terminal(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.next() {
                Some('[') => {
                    // CSI: parameters, then one final byte in @..~
                    for n in chars.by_ref() {
                        if ('@'..='~').contains(&n) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC: until BEL or ESC \
                    while let Some(n) = chars.next() {
                        if n == '\u{7}' || (n == '\u{1b}' && chars.peek() == Some(&'\\')) {
                            if n == '\u{1b}' {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        out.push(c);
    }
    if !out.contains('\r') {
        return out;
    }
    out.split('\n')
        .map(|line| {
            let line = line.strip_suffix('\r').unwrap_or(line);
            line.rsplit('\r').next().unwrap_or(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Keeps output within `max_lines` and `max_bytes`: the first part and (mostly) the end, where
/// errors and results usually are. Returns the text and whether anything was cut.
pub fn head_tail(text: &str, max_lines: usize, max_bytes: usize) -> (String, bool) {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max_lines && text.len() <= max_bytes {
        return (text.to_string(), false);
    }
    let head_budget = max_bytes * 2 / 5;
    let tail_budget = max_bytes - head_budget;
    let head_lines = max_lines * 2 / 5;
    let tail_lines = max_lines - head_lines;

    let mut head = Vec::new();
    let mut size = 0;
    for l in lines.iter().take(head_lines) {
        let l = cut_line(l, 2000);
        if size + l.len() + 1 > head_budget {
            break;
        }
        size += l.len() + 1;
        head.push(l);
    }
    let mut tail = Vec::new();
    let mut size = 0;
    for l in lines.iter().skip(head.len()).rev().take(tail_lines) {
        let l = cut_line(l, 2000);
        if size + l.len() + 1 > tail_budget {
            break;
        }
        size += l.len() + 1;
        tail.push(l);
    }
    tail.reverse();
    let omitted = lines.len() - head.len() - tail.len();
    let mut out = head.join("\n");
    out.push_str(&format!("\n\n... {omitted} lines omitted ...\n\n"));
    out.push_str(&tail.join("\n"));
    (out, true)
}

/// Cuts a line at `max` characters, saying how much was dropped.
pub fn cut_line(line: &str, max: usize) -> String {
    match line.char_indices().nth(max) {
        Some((i, _)) => format!("{}… ({} more chars)", &line[..i], line[i..].chars().count()),
        None => line.to_string(),
    }
}

/// Lexically normalizes a path (`a/./b/../c` → `a/c`) without touching the filesystem.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// `~/x` and relative paths become absolute.
pub fn resolve(cwd: &Path, path: &str) -> PathBuf {
    let p = match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(path),
    };
    normalize(&if p.is_absolute() { p } else { cwd.join(p) })
}

/// The path relative to `cwd` when it is inside it, else absolute. Shorter paths, fewer tokens.
pub fn display(cwd: &Path, path: &Path) -> String {
    match path.strip_prefix(cwd) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".into(),
        Ok(rel) => rel.to_string_lossy().into_owned(),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

/// The git worktree root above `cwd`, if any.
pub fn git_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors().find(|d| d.join(".git").exists()).map(Path::to_path_buf)
}

/// A one-line title from the first prompt, cut at a word boundary.
pub fn title(prompt: &str) -> String {
    let line = prompt.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("New session");
    if line.chars().count() <= 60 {
        return line.to_string();
    }
    let cut: String = line.chars().take(60).collect();
    match cut.rfind(' ') {
        Some(i) if i > 30 => format!("{}…", &cut[..i]),
        _ => format!("{cut}…"),
    }
}

/// Standard base64 with padding.
pub fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            out.push(if i <= chunk.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

fn is_wsl() -> bool {
    std::fs::read_to_string("/proc/version").is_ok_and(|v| v.to_lowercase().contains("microsoft"))
}

/// Opens an http(s) URL in the browser (the Windows one under WSL), or with the program in
/// `CODEIT_BROWSER`. With `CODEIT_NO_BROWSER` set, does nothing: the caller shows the URL anyway.
pub fn open_url(url: &str) {
    if !url.starts_with("https://") && !url.starts_with("http://") || std::env::var_os("CODEIT_NO_BROWSER").is_some() {
        return;
    }
    // CODEIT_BROWSER: a program to open URLs with instead of the system's browser.
    if let Ok(b) = std::env::var("CODEIT_BROWSER")
        && !b.is_empty()
    {
        let _ = std::process::Command::new(b)
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        return;
    }
    let spawn = |p: &str, args: &[&str]| {
        let _ = std::process::Command::new(p)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    };
    if cfg!(target_os = "macos") {
        spawn("open", &[url]);
    } else if cfg!(windows) {
        spawn("cmd", &["/c", "start", "", url]);
    } else if is_wsl() {
        let ps = "/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe";
        let ps = if std::path::Path::new(ps).exists() { ps } else { "powershell.exe" };
        // Single quotes for PowerShell, so `&` and `#` in the URL survive.
        spawn(ps, &["-NoProfile", "-Command", &format!("Start-Process '{}'", url.replace('\'', "''"))]);
    } else {
        spawn("xdg-open", &[url]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter() {
        let (f, body) = frontmatter(
            "---\nname: x\ndescription: >\n  long\n  text\nneeds: [a, b]\nq: \"quoted: yes\"\n---\n\nBody\n",
        );
        assert_eq!(f["name"], "x");
        assert_eq!(f["description"], "long text");
        assert_eq!(f["needs"], "a, b");
        assert_eq!(f["q"], "quoted: yes");
        assert_eq!(body, "Body\n");
        assert_eq!(frontmatter("no front").1, "no front");
    }

    #[test]
    fn cleans_terminal_output() {
        assert_eq!(clean_terminal("\u{1b}[31mred\u{1b}[0m ok"), "red ok");
        assert_eq!(clean_terminal("10%\r50%\r100%\ndone\r\n"), "100%\ndone\n");
    }

    #[test]
    fn keeps_head_and_tail() {
        let text: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let (out, cut) = head_tail(&text, 10, 10_000);
        assert!(cut);
        assert!(out.starts_with("line 0\n"));
        assert!(out.contains("90 lines omitted"));
        assert!(out.ends_with("line 99"));
    }

    #[test]
    fn titles_and_paths() {
        assert_eq!(title("\n fix the bug\nmore"), "fix the bug");
        assert!(title(&"word ".repeat(30)).ends_with('…'));
        assert_eq!(normalize(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
        assert_eq!(display(Path::new("/p"), Path::new("/p/src/x.rs")), "src/x.rs");
        assert_eq!(display(Path::new("/p"), Path::new("/q/x")), "/q/x");
    }
}
