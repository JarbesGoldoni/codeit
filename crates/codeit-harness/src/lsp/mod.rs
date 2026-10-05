//! Language servers: code intelligence for the `lsp` tool, and errors reported back to the
//! model after it edits a file, as opencode does.
//!
//! Built-in servers are used when their program is on PATH (none are installed for you):
//! rust-analyzer, typescript-language-server, pyright, gopls, jdtls, kotlin-language-server
//! and clangd. The `lsp` config key adds or changes servers in opencode's shape
//! (`{"name": {"command": [...], "extensions": [".x"], "env": {}, "initialization": {},
//! "disabled": true}}`), and `"lsp": false` turns them all off. Each server starts the first
//! time a file it handles is touched, rooted at the nearest folder with one of its project
//! files.

pub mod client;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

pub use client::{Client, Diagnostic, path_of, real, uri};

#[derive(Clone, Debug)]
pub struct Server {
    pub id: String,
    pub command: Vec<String>,
    pub extensions: Vec<String>,
    /// Files that mark a project root for this server (nearest wins); none: the project root.
    pub roots: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub initialization: Option<Value>,
}

/// Whether a server's program can be run.
pub fn installed(program: &str) -> bool {
    which(program).is_some() || Path::new(program).is_file()
}

fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(program)).find(|p| p.is_file())
}

fn builtin() -> Vec<Server> {
    let s = |id: &str, cmd: &[&str], ext: &[&str], roots: &[&str]| Server {
        id: id.into(),
        command: cmd.iter().map(|c| c.to_string()).collect(),
        extensions: ext.iter().map(|e| e.to_string()).collect(),
        roots: roots.iter().map(|r| r.to_string()).collect(),
        env: BTreeMap::new(),
        initialization: None,
    };
    let jdtls_data = codeit_providers::data_dir().join("lsp/jdtls").to_string_lossy().into_owned();
    vec![
        s("rust", &["rust-analyzer"], &[".rs"], &["Cargo.toml"]),
        s(
            "typescript",
            &["typescript-language-server", "--stdio"],
            &[".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts"],
            &["package.json", "tsconfig.json", "jsconfig.json"],
        ),
        s(
            "pyright",
            &["pyright-langserver", "--stdio"],
            &[".py", ".pyi"],
            &["pyproject.toml", "setup.py", "setup.cfg", "requirements.txt", "Pipfile", "pyrightconfig.json"],
        ),
        s("gopls", &["gopls"], &[".go"], &["go.mod", "go.work"]),
        s(
            "jdtls",
            &["jdtls", "-data", &jdtls_data],
            &[".java"],
            &["pom.xml", "build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts"],
        ),
        s(
            "kotlin",
            &["kotlin-language-server"],
            &[".kt", ".kts"],
            &["build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts", "pom.xml"],
        ),
        s(
            "clangd",
            &["clangd"],
            &[".c", ".cpp", ".cc", ".cxx", ".h", ".hpp", ".hh", ".hxx"],
            &["compile_commands.json", "compile_flags.txt", ".clangd", "CMakeLists.txt"],
        ),
    ]
}

/// The servers in effect: the built-ins, changed or extended by the `lsp` config.
pub fn servers(config: Option<&Value>) -> Vec<Server> {
    if config == Some(&Value::Bool(false)) {
        return Vec::new();
    }
    let mut out = builtin();
    let Some(Value::Object(map)) = config else { return out };
    for (id, c) in map {
        if c["disabled"] == true {
            out.retain(|s| &s.id != id);
            continue;
        }
        let strings = |v: &Value| -> Vec<String> {
            v.as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect()
        };
        let existing = out.iter().position(|s| &s.id == id);
        let mut s = existing.map(|i| out[i].clone()).unwrap_or(Server {
            id: id.clone(),
            command: Vec::new(),
            extensions: Vec::new(),
            roots: Vec::new(),
            env: BTreeMap::new(),
            initialization: None,
        });
        if c["command"].is_array() {
            s.command = strings(&c["command"]);
        }
        if c["extensions"].is_array() {
            s.extensions = strings(&c["extensions"]);
        }
        if let Some(env) = c["env"].as_object() {
            s.env = env.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect();
        }
        if !c["initialization"].is_null() {
            s.initialization = Some(c["initialization"].clone());
        }
        match existing {
            Some(i) => out[i] = s,
            None => out.push(s),
        }
    }
    out
}

/// The `languageId` the server expects for a file.
pub fn language(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "py" | "pyi" => "python",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        _ => "plaintext",
    }
}

/// The nearest folder from `file` up to `top` containing one of `markers`; else `top`.
fn root_for(file: &Path, top: &Path, markers: &[String]) -> PathBuf {
    let mut dir = file.parent();
    while let Some(d) = dir {
        if markers.iter().any(|m| d.join(m).exists()) {
            return d.to_path_buf();
        }
        if d == top {
            break;
        }
        dir = d.parent();
    }
    top.to_path_buf()
}

/// Running servers, one per (server, root), started on demand.
#[derive(Default)]
pub struct Lsp {
    clients: tokio::sync::Mutex<HashMap<(String, PathBuf), Arc<Client>>>,
    /// Servers that failed to start, not tried again this session.
    broken: std::sync::Mutex<HashMap<(String, PathBuf), String>>,
}

impl Lsp {
    /// Servers that handle this file, whether or not their program is installed.
    pub fn handles<'a>(servers: &'a [Server], file: &Path) -> Vec<&'a Server> {
        let ext = file.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        servers.iter().filter(|s| s.extensions.contains(&ext)).collect()
    }

    /// Clients for the file, starting them if needed. Servers whose program isn't installed are
    /// skipped; the error says which ones were.
    pub async fn clients(&self, servers: &[Server], top: &Path, file: &Path) -> Result<Vec<Arc<Client>>> {
        let wanted = Self::handles(servers, file);
        if wanted.is_empty() {
            return Err(anyhow!(
                "No language server handles {} files.",
                file.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default()
            ));
        }
        let mut out = Vec::new();
        let mut missing = Vec::new();
        for s in wanted {
            let Some(program) = s.command.first() else { continue };
            if which(program).is_none() && !Path::new(program).is_file() {
                missing.push(program.clone());
                continue;
            }
            let root = root_for(file, top, &s.roots);
            let key = (s.id.clone(), root.clone());
            if let Some(why) = self.broken.lock().unwrap().get(&key) {
                missing.push(format!("{} ({why})", s.id));
                continue;
            }
            let mut clients = self.clients.lock().await;
            if let Some(c) = clients.get(&key) {
                out.push(c.clone());
                continue;
            }
            match Client::start(&s.id, &s.command, &s.env, &root, s.initialization.clone()).await {
                Ok(c) => {
                    let c = Arc::new(c);
                    clients.insert(key, c.clone());
                    out.push(c);
                }
                Err(e) => {
                    self.broken.lock().unwrap().insert(key, e.to_string());
                    missing.push(format!("{} ({e})", s.id));
                }
            }
        }
        if out.is_empty() {
            return Err(anyhow!(
                "No language server could start for this file: {}. Install one, or set one in the `lsp` config.",
                missing.join(", ")
            ));
        }
        Ok(out)
    }

    /// Running servers: `id (root)`.
    pub async fn describe(&self) -> Vec<String> {
        self.clients.lock().await.values().map(|c| format!("{} ({})", c.id, c.root.display())).collect()
    }

    /// After a file changed: the errors servers already running (or startable) report for it,
    /// waiting up to `wait` for fresh ones. Empty when no server handles it.
    pub async fn errors(&self, servers: &[Server], top: &Path, file: &Path, wait: Duration) -> Vec<Diagnostic> {
        let Ok(clients) = self.clients(servers, top, file).await else { return Vec::new() };
        let mut all = Vec::new();
        for c in clients {
            let seen = c.touch(file, language(file));
            all.extend(c.diagnostics(file, seen, wait).await.into_iter().filter(|d| d.severity == 1));
        }
        all
    }
}

/// Errors as lines for the model: `L12:5 message`, at most 20.
pub fn report(errors: &[Diagnostic]) -> Option<String> {
    if errors.is_empty() {
        return None;
    }
    let mut lines: Vec<String> = errors
        .iter()
        .take(20)
        .map(|d| format!("L{}:{} {}", d.line + 1, d.character + 1, d.message.lines().next().unwrap_or_default()))
        .collect();
    if errors.len() > 20 {
        lines.push(format!("... and {} more", errors.len() - 20));
    }
    Some(lines.join("\n"))
}

/// A path as shown to the model: relative to the working directory when inside it, symlinks
/// or not.
fn shown(cwd: &Path, path: &Path) -> String {
    let real_cwd = real(cwd);
    match path.strip_prefix(&real_cwd) {
        Ok(rel) => rel.to_string_lossy().into_owned(),
        Err(_) => crate::util::display(cwd, path),
    }
}

/// A location result as `path:line:col  source line`.
fn location(v: &Value, cwd: &Path) -> Option<String> {
    let (u, range) = match (v["uri"].as_str(), v["targetUri"].as_str()) {
        (Some(u), _) => (u, &v["range"]),
        (None, Some(u)) => (u, &v["targetSelectionRange"]),
        _ => return None,
    };
    let path = path_of(u);
    let line = range["start"]["line"].as_u64()? as usize;
    let col = range["start"]["character"].as_u64()? + 1;
    let text = std::fs::read_to_string(&path).ok().and_then(|t| t.lines().nth(line).map(|l| l.trim().to_string()));
    Some(format!("{}:{}:{col}  {}", shown(cwd, &path), line + 1, text.unwrap_or_default()))
}

const KINDS: &[&str] = &[
    "",
    "file",
    "module",
    "namespace",
    "package",
    "class",
    "method",
    "property",
    "field",
    "constructor",
    "enum",
    "interface",
    "function",
    "variable",
    "constant",
    "string",
    "number",
    "boolean",
    "array",
    "object",
    "key",
    "null",
    "enum member",
    "struct",
    "event",
    "operator",
    "type parameter",
];

fn symbols(list: &[Value], cwd: &Path, depth: usize, out: &mut Vec<String>) {
    for s in list {
        let kind = KINDS.get(s["kind"].as_u64().unwrap_or(0) as usize).copied().unwrap_or("");
        let line =
            s["range"]["start"]["line"].as_u64().or(s["location"]["range"]["start"]["line"].as_u64()).unwrap_or(0) + 1;
        let file = s["location"]["uri"].as_str().map(|u| format!("{}:", shown(cwd, &path_of(u)))).unwrap_or_default();
        out.push(format!("{}{kind} {}  ({file}{line})", "  ".repeat(depth), s["name"].as_str().unwrap_or("?")));
        if let Some(children) = s["children"].as_array() {
            symbols(children, cwd, depth + 1, out);
        }
    }
}

/// An LSP result as text for the model.
pub fn format(operation: &str, result: &Value, cwd: &Path) -> String {
    let items: Vec<Value> = match result {
        Value::Null => Vec::new(),
        Value::Array(a) => a.clone(),
        other => vec![other.clone()],
    };
    if items.is_empty() {
        return format!("No results for {operation}.");
    }
    match operation {
        "hover" => {
            let c = &items[0]["contents"];
            let text = match c {
                Value::String(s) => s.clone(),
                Value::Array(a) => a
                    .iter()
                    .map(|x| x["value"].as_str().or(x.as_str()).unwrap_or("").to_string())
                    .collect::<Vec<_>>()
                    .join("\n\n"),
                _ => c["value"].as_str().unwrap_or("").to_string(),
            };
            if text.trim().is_empty() { "No hover information.".into() } else { text }
        }
        "documentSymbol" | "workspaceSymbol" => {
            let mut out = Vec::new();
            symbols(&items, cwd, 0, &mut out);
            out.truncate(300);
            out.join("\n")
        }
        "incomingCalls" | "outgoingCalls" => items
            .iter()
            .filter_map(|c| {
                let item = if operation == "incomingCalls" { &c["from"] } else { &c["to"] };
                let at = location(&json!({ "uri": item["uri"], "range": item["selectionRange"] }), cwd)?;
                Some(format!("{}  {at}", item["name"].as_str().unwrap_or("?")))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        "prepareCallHierarchy" => items
            .iter()
            .filter_map(|i| {
                Some(format!(
                    "{}  {}",
                    i["name"].as_str()?,
                    location(&json!({ "uri": i["uri"], "range": i["selectionRange"] }), cwd)?
                ))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => {
            let mut lines: Vec<String> = items.iter().filter_map(|v| location(v, cwd)).collect();
            let n = lines.len();
            lines.truncate(200);
            if n > 200 {
                lines.push(format!("... and {} more", n - 200));
            }
            lines.join("\n")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_changes_and_adds_servers() {
        let c = json!({
            "rust": { "command": ["/opt/ra"] },
            "gopls": { "disabled": true },
            "zig": { "command": ["zls"], "extensions": [".zig"] }
        });
        let s = servers(Some(&c));
        let rust = s.iter().find(|s| s.id == "rust").unwrap();
        assert_eq!(
            (rust.command.clone(), rust.extensions.clone()),
            (vec!["/opt/ra".to_string()], vec![".rs".to_string()])
        );
        assert!(!s.iter().any(|s| s.id == "gopls"));
        assert!(s.iter().any(|s| s.id == "zig"));
        assert!(servers(Some(&json!(false))).is_empty());
        assert_eq!(Lsp::handles(&s, Path::new("a/B.java"))[0].id, "jdtls");
    }

    #[test]
    fn finds_the_nearest_root() {
        let d = std::env::temp_dir().join(format!("codeit-lsp-{}", std::process::id()));
        std::fs::create_dir_all(d.join("svc/src")).unwrap();
        std::fs::write(d.join("svc/build.gradle"), "").unwrap();
        let markers = vec!["build.gradle".to_string()];
        assert_eq!(root_for(&d.join("svc/src/A.java"), &d, &markers), d.join("svc"));
        assert_eq!(root_for(&d.join("x.java"), &d, &markers), d);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn formats_results() {
        let d = std::env::temp_dir().join(format!("codeit-lspf-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("a.rs"), "fn main() {}\nfn add() {}\n").unwrap();
        let loc = json!([{ "uri": uri(&d.join("a.rs")), "range": { "start": { "line": 1, "character": 3 }, "end": { "line": 1, "character": 6 } } }]);
        assert_eq!(format("goToDefinition", &loc, &d), "a.rs:2:4  fn add() {}");
        assert_eq!(
            format("hover", &json!({ "contents": { "kind": "markdown", "value": "fn add()" } }), &d),
            "fn add()"
        );
        assert_eq!(format("findReferences", &Value::Null, &d), "No results for findReferences.");
        let syms = json!([{ "name": "Calc", "kind": 23, "range": { "start": { "line": 0 } }, "children": [{ "name": "add", "kind": 6, "range": { "start": { "line": 1 } } }] }]);
        assert_eq!(format("documentSymbol", &syms, &d), "struct Calc  (1)\n  method add  (2)");
        assert_eq!(
            report(&[Diagnostic { severity: 1, line: 4, character: 0, message: "bad\nmore".into() }]).unwrap(),
            "L5:1 bad"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
