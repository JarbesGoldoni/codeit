//! One language server process, spoken to over stdio (JSON-RPC with Content-Length framing).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

/// A diagnostic as the server reported it.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    /// 1 error, 2 warning, 3 information, 4 hint.
    pub severity: u64,
    /// 0-based.
    pub line: u64,
    pub character: u64,
    pub message: String,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;
/// uri → (latest diagnostics, how many times they were published).
type Published = Arc<Mutex<HashMap<String, (Vec<Diagnostic>, u64)>>>;

pub struct Client {
    pub id: String,
    pub root: PathBuf,
    tx: mpsc::UnboundedSender<Vec<u8>>,
    pending: Pending,
    next: AtomicU64,
    /// uri → (latest diagnostics, how many times they were published).
    diagnostics: Published,
    /// uri → version, for files opened on the server.
    opened: Mutex<HashMap<String, i64>>,
    _child: tokio::process::Child,
}

fn frame(msg: &Value) -> Vec<u8> {
    let body = msg.to_string();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend(body.into_bytes());
    out
}

/// `file://` URI of a path, percent-encoding what URIs don't allow.
pub fn uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The path with symlinks resolved, as servers report it (macOS's /var is /private/var).
pub fn real(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// The path of a `file://` URI.
pub fn path_of(uri: &str) -> PathBuf {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    let bytes = raw.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(v) = u8::from_str_radix(&raw[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    PathBuf::from(String::from_utf8_lossy(&out).into_owned())
}

impl Client {
    /// Starts the server and runs the initialize handshake.
    pub async fn start(
        id: &str,
        command: &[String],
        env: &std::collections::BTreeMap<String, String>,
        root: &Path,
        initialization: Option<Value>,
    ) -> Result<Client> {
        let (program, args) = command.split_first().ok_or_else(|| anyhow!("empty command for {id}"))?;
        let mut child = tokio::process::Command::new(program)
            .args(args)
            .envs(env)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| anyhow!("couldn't start {program}: {e}"))?;
        let mut stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");

        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            while let Some(bytes) = rx.recv().await {
                if stdin.write_all(&bytes).await.is_err() || stdin.flush().await.is_err() {
                    break;
                }
            }
        });

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let diagnostics: Published = Arc::default();
        {
            let (pending, diagnostics, tx) = (pending.clone(), diagnostics.clone(), tx.clone());
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout);
                loop {
                    // Headers, then the body.
                    let mut len = 0usize;
                    loop {
                        let mut line = String::new();
                        match reader.read_line(&mut line).await {
                            Ok(0) | Err(_) => return,
                            Ok(_) => {}
                        }
                        let line = line.trim();
                        if line.is_empty() {
                            break;
                        }
                        if let Some(v) = line.strip_prefix("Content-Length:") {
                            len = v.trim().parse().unwrap_or(0);
                        }
                    }
                    let mut body = vec![0u8; len];
                    if reader.read_exact(&mut body).await.is_err() {
                        return;
                    }
                    let Ok(msg) = serde_json::from_slice::<Value>(&body) else { continue };
                    handle(&msg, &pending, &diagnostics, &tx);
                }
            });
        }

        let client = Client {
            id: id.to_string(),
            root: root.to_path_buf(),
            tx,
            pending,
            next: AtomicU64::new(1),
            diagnostics,
            opened: Mutex::new(HashMap::new()),
            _child: child,
        };
        let root_uri = uri(&real(root));
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        client
            .request(
                "initialize",
                json!({
                    "processId": std::process::id(),
                    "rootUri": root_uri,
                    "rootPath": root,
                    "workspaceFolders": [{ "uri": root_uri, "name": name }],
                    "initializationOptions": initialization.unwrap_or(Value::Null),
                    "capabilities": {
                        "window": { "workDoneProgress": true },
                        "workspace": { "configuration": true, "workspaceFolders": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                        "textDocument": {
                            "synchronization": { "didOpen": true, "didChange": true },
                            "publishDiagnostics": { "versionSupport": true },
                            "hover": { "contentFormat": ["markdown", "plaintext"] },
                            "definition": {}, "references": {}, "implementation": {},
                            "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                            "callHierarchy": {},
                        },
                    },
                }),
                Duration::from_secs(45),
            )
            .await
            .map_err(|e| anyhow!("{id} didn't start: {e}"))?;
        client.notify("initialized", json!({}));
        Ok(client)
    }

    pub fn notify(&self, method: &str, params: Value) {
        let _ = self.tx.send(frame(&json!({ "jsonrpc": "2.0", "method": method, "params": params })));
    }

    pub async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (done, wait) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, done);
        self.tx
            .send(frame(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })))
            .map_err(|_| anyhow!("{} has stopped", self.id))?;
        match tokio::time::timeout(timeout, wait).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => bail!("{}: {e}", self.id),
            Ok(Err(_)) => bail!("{} has stopped", self.id),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                bail!("{} didn't answer {method} within {}s", self.id, timeout.as_secs())
            }
        }
    }

    /// Opens the file on the server, or sends its new content. Returns how many times the
    /// server had published diagnostics for it before, to wait for the next ones.
    pub fn touch(&self, path: &Path, language: &str) -> u64 {
        let u = uri(&real(path));
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let seen = self.diagnostics.lock().unwrap().get(&u).map(|d| d.1).unwrap_or(0);
        let mut opened = self.opened.lock().unwrap();
        match opened.get_mut(&u) {
            Some(v) => {
                *v += 1;
                self.notify(
                    "textDocument/didChange",
                    json!({ "textDocument": { "uri": u, "version": *v }, "contentChanges": [{ "text": text }] }),
                );
            }
            None => {
                opened.insert(u.clone(), 0);
                self.notify(
                    "textDocument/didOpen",
                    json!({ "textDocument": { "uri": u, "languageId": language, "version": 0, "text": text } }),
                );
            }
        }
        seen
    }

    /// The file's diagnostics, after waiting (up to `wait`) for a publication newer than `seen`.
    pub async fn diagnostics(&self, path: &Path, seen: u64, wait: Duration) -> Vec<Diagnostic> {
        let u = uri(&real(path));
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            if let Some((d, n)) = self.diagnostics.lock().unwrap().get(&u)
                && *n > seen
            {
                return d.clone();
            }
            if tokio::time::Instant::now() >= deadline {
                return self.diagnostics.lock().unwrap().get(&u).map(|d| d.0.clone()).unwrap_or_default();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

/// Responses, diagnostics, and the requests servers make of the client.
fn handle(msg: &Value, pending: &Pending, diagnostics: &Published, tx: &mpsc::UnboundedSender<Vec<u8>>) {
    let method = msg["method"].as_str();
    match (method, msg.get("id")) {
        // A response to one of ours.
        (None, Some(id)) => {
            if let Some(done) = id.as_u64().and_then(|id| pending.lock().unwrap().remove(&id)) {
                let r = match msg.get("error") {
                    Some(e) => Err(e["message"].as_str().unwrap_or("error").to_string()),
                    None => Ok(msg["result"].clone()),
                };
                let _ = done.send(r);
            }
        }
        // A request from the server: answer what's needed to keep it going.
        (Some(m), Some(id)) => {
            let result = match m {
                "workspace/configuration" => {
                    let n = msg["params"]["items"].as_array().map(Vec::len).unwrap_or(0);
                    Value::Array(vec![Value::Null; n])
                }
                "workspace/workspaceFolders" => Value::Array(Vec::new()),
                _ => Value::Null,
            };
            let _ = tx.send(frame(&json!({ "jsonrpc": "2.0", "id": id, "result": result })));
        }
        (Some("textDocument/publishDiagnostics"), None) => {
            let p = &msg["params"];
            let Some(u) = p["uri"].as_str() else { return };
            let list = p["diagnostics"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|d| Diagnostic {
                    severity: d["severity"].as_u64().unwrap_or(1),
                    line: d["range"]["start"]["line"].as_u64().unwrap_or(0),
                    character: d["range"]["start"]["character"].as_u64().unwrap_or(0),
                    message: d["message"].as_str().unwrap_or_default().to_string(),
                })
                .collect();
            let mut all = diagnostics.lock().unwrap();
            let n = all.get(u).map(|d| d.1).unwrap_or(0) + 1;
            // Keyed the way we send URIs, whatever encoding the server used.
            all.insert(uri(&real(&path_of(u))), (list, n));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_paths_and_uris() {
        let p = Path::new("/home/me/my project/a#b.rs");
        assert_eq!(uri(p), "file:///home/me/my%20project/a%23b.rs");
        assert_eq!(path_of(&uri(p)), p);
        assert_eq!(path_of("file:///x/y%3Az.rs"), Path::new("/x/y:z.rs"));
    }
}
