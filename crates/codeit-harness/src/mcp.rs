//! MCP (Model Context Protocol) servers: local ones over stdio, remote ones over streamable
//! HTTP, falling back to the older SSE transport as opencode does. Remote servers that require
//! OAuth are logged in to with `/mcp login <name>` (see `mcp_oauth`). Each server's tools are
//! offered to the model as `<server>_<tool>`, and its instructions go into the system prompt.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use codeit_providers::ToolSpec;
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;

use crate::Harness;
use crate::config::McpConfig;
use crate::tools::{Ctx, Output, Tool};

const PROTOCOL: &str = "2025-06-18";

pub enum Status {
    Connecting,
    Ready(Arc<Client>),
    Failed(String),
    /// The server wants an OAuth login: `/mcp login <name>`. Holds its `resource_metadata` hint.
    NeedsLogin(Option<String>),
    Disabled,
}

/// A remote server answered 401 and no usable token is stored.
#[derive(Debug)]
pub struct NeedsLogin(pub Option<String>);

impl std::fmt::Display for NeedsLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the server needs a login")
    }
}

impl std::error::Error for NeedsLogin {}

/// Every configured server and its state.
#[derive(Default)]
pub struct Mcp {
    pub servers: Mutex<Vec<(String, Status)>>,
    configs: Mutex<BTreeMap<String, McpConfig>>,
    cwd: Mutex<PathBuf>,
}

impl Mcp {
    /// Starts connecting to every enabled server in the background.
    pub fn start(self: &Arc<Self>, configs: &BTreeMap<String, McpConfig>, cwd: &Path) {
        *self.configs.lock().unwrap() = configs.clone();
        *self.cwd.lock().unwrap() = cwd.to_path_buf();
        for (name, cfg) in configs {
            if !cfg.enabled() {
                self.servers.lock().unwrap().push((name.clone(), Status::Disabled));
                continue;
            }
            self.servers.lock().unwrap().push((name.clone(), Status::Connecting));
            let me = self.clone();
            let name = name.clone();
            tokio::spawn(async move { me.connect(&name).await });
        }
    }

    fn set(&self, name: &str, status: Status) {
        let mut servers = self.servers.lock().unwrap();
        match servers.iter_mut().find(|(n, _)| n == name) {
            Some(slot) => slot.1 = status,
            None => servers.push((name.to_string(), status)),
        }
    }

    /// (Re)connects one server and records how it went.
    pub async fn connect(&self, name: &str) {
        let Some(cfg) = self.configs.lock().unwrap().get(name).cloned() else { return };
        let cwd = self.cwd.lock().unwrap().clone();
        self.set(name, Status::Connecting);
        let status = match Client::connect(name, &cfg, &cwd).await {
            Ok(c) => Status::Ready(Arc::new(c)),
            Err(e) => match e.downcast_ref::<NeedsLogin>() {
                Some(n) => Status::NeedsLogin(n.0.clone()),
                None => Status::Failed(format!("{e:#}")),
            },
        };
        // Turned off while it was connecting: drop the connection.
        if matches!(self.status_of(name), Some(Status::Disabled)) {
            return;
        }
        self.set(name, status);
    }

    fn status_of(&self, name: &str) -> Option<Status> {
        let servers = self.servers.lock().unwrap();
        servers.iter().find(|(n, _)| n == name).map(|(_, s)| match s {
            Status::Connecting => Status::Connecting,
            Status::Ready(c) => Status::Ready(c.clone()),
            Status::Failed(e) => Status::Failed(e.clone()),
            Status::NeedsLogin(h) => Status::NeedsLogin(h.clone()),
            Status::Disabled => Status::Disabled,
        })
    }

    /// Turns a server off (closing its connection) or back on, for this run; true if it is on now.
    pub fn toggle(self: &Arc<Self>, name: &str) -> bool {
        if matches!(self.status_of(name), Some(Status::Disabled)) {
            self.set(name, Status::Connecting);
            let (me, name) = (self.clone(), name.to_string());
            tokio::spawn(async move { me.connect(&name).await });
            true
        } else {
            self.set(name, Status::Disabled);
            false
        }
    }

    /// (name, state, on) per server, for the `/mcp` list.
    pub fn list(&self) -> Vec<(String, String, bool)> {
        self.servers
            .lock()
            .unwrap()
            .iter()
            .map(|(name, s)| {
                let state = match s {
                    Status::Connecting => "connecting…".to_string(),
                    Status::Ready(c) => format!("{} tools", c.tools.len()),
                    Status::Failed(e) => format!("failed: {}", e.lines().next().unwrap_or_default()),
                    Status::NeedsLogin(_) => format!("needs a login: /mcp login {name}"),
                    Status::Disabled => "off".to_string(),
                };
                (name.clone(), state, !matches!(s, Status::Disabled))
            })
            .collect()
    }

    /// The URL of a remote server.
    pub fn url(&self, name: &str) -> Option<String> {
        match self.configs.lock().unwrap().get(name)? {
            McpConfig::Remote { url, .. } => Some(url.clone()),
            McpConfig::Local { .. } => None,
        }
    }

    /// Starts an OAuth login to a remote server; returns the page to open and the pending login.
    pub async fn login(&self, name: &str) -> Result<crate::mcp_oauth::Login> {
        let url = self.url(name).ok_or_else(|| anyhow!("{name} isn't a remote MCP server"))?;
        let hint = match self.servers.lock().unwrap().iter().find(|(n, _)| n == name) {
            Some((_, Status::NeedsLogin(h))) => h.clone(),
            _ => None,
        };
        crate::mcp_oauth::login(&url, hint.as_deref()).await
    }

    /// Server names, for `/mcp`.
    pub fn names(&self) -> Vec<String> {
        self.configs.lock().unwrap().keys().cloned().collect()
    }

    pub fn ready(&self) -> Vec<Arc<Client>> {
        self.servers
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(_, s)| if let Status::Ready(c) = s { Some(c.clone()) } else { None })
            .collect()
    }

    /// One line per server, for `/status`.
    pub fn describe(&self) -> Vec<String> {
        self.servers
            .lock()
            .unwrap()
            .iter()
            .map(|(name, s)| match s {
                Status::Connecting => format!("{name}: connecting..."),
                Status::Ready(c) => format!("{name}: {} tools", c.tools.len()),
                Status::Failed(e) => format!("{name}: failed: {e}"),
                Status::NeedsLogin(_) => format!("{name}: needs a login, run /mcp login {name}"),
                Status::Disabled => format!("{name}: disabled"),
            })
            .collect()
    }

    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut out: Vec<Arc<dyn Tool>> = Vec::new();
        let mut used: Vec<String> = Vec::new();
        for c in self.ready() {
            for t in &c.tools {
                let mut name = wire_name(&c.name, &t.name);
                for i in 2.. {
                    if !used.contains(&name) {
                        break;
                    }
                    name = format!("{}_{i}", &name[..name.len().min(60)]);
                }
                used.push(name.clone());
                out.push(Arc::new(McpTool { client: c.clone(), wire: name, def: t.clone() }));
            }
        }
        out
    }

    /// The servers' own instructions, for the system prompt.
    pub fn prompt(&self) -> Option<String> {
        let parts: Vec<String> = self
            .ready()
            .iter()
            .filter_map(|c| {
                c.instructions.as_ref().map(|i| format!("<server name=\"{}\">\n{}\n</server>", c.name, i.trim()))
            })
            .collect();
        if parts.is_empty() { None } else { Some(format!("# MCP server instructions\n{}", parts.join("\n"))) }
    }
}

/// `server_tool`, limited to the characters and length every provider accepts.
pub fn wire_name(server: &str, tool: &str) -> String {
    let raw = format!("{server}_{tool}");
    let mut s: String =
        raw.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    s.truncate(64);
    s
}

#[derive(Clone, Debug)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

enum Transport {
    Stdio {
        stdin: tokio::sync::Mutex<tokio::process::ChildStdin>,
        pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
        _child: tokio::process::Child,
    },
    Http {
        url: String,
        headers: BTreeMap<String, String>,
        session: Mutex<Option<String>>,
        /// No Authorization header configured: use an OAuth token when there is one.
        oauth: bool,
    },
    /// The older HTTP+SSE transport: replies arrive on the event stream; messages are POSTed
    /// to the endpoint the stream announced.
    Sse {
        post_url: String,
        headers: BTreeMap<String, String>,
        pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
        _reader: tokio::task::JoinHandle<()>,
    },
}

pub struct Client {
    pub name: String,
    pub instructions: Option<String>,
    pub tools: Vec<ToolDef>,
    transport: Transport,
    next_id: AtomicU64,
    timeout: Duration,
}

impl Client {
    pub async fn connect(name: &str, cfg: &McpConfig, cwd: &Path) -> Result<Client> {
        let (transport, timeout) = match cfg {
            McpConfig::Local { command, environment, timeout, .. } => {
                let (prog, args) = command.split_first().ok_or_else(|| anyhow!("empty command"))?;
                let mut child = tokio::process::Command::new(prog)
                    .args(args)
                    .envs(environment)
                    .current_dir(cwd)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|e| anyhow!("can't start `{prog}`: {e}"))?;
                let stdin = child.stdin.take().expect("piped");
                let stdout = child.stdout.take().expect("piped");
                let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>> = Default::default();
                let p = pending.clone();
                tokio::spawn(async move {
                    let mut lines = BufReader::new(stdout).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
                        if let Some(id) = msg["id"].as_u64()
                            && (msg.get("result").is_some() || msg.get("error").is_some())
                            && let Some(tx) = p.lock().unwrap().remove(&id)
                        {
                            let _ = tx.send(msg);
                        }
                    }
                });
                let t = Transport::Stdio { stdin: tokio::sync::Mutex::new(stdin), pending, _child: child };
                (t, timeout.unwrap_or(30_000))
            }
            McpConfig::Remote { url, headers, timeout, .. } => {
                let oauth = !headers.keys().any(|k| k.eq_ignore_ascii_case("authorization"));
                let http =
                    Transport::Http { url: url.clone(), headers: headers.clone(), session: Mutex::new(None), oauth };
                let timeout = timeout.unwrap_or(30_000);
                // Servers whose URL ends in /sse speak the older transport; others get streamable
                // HTTP first and SSE when they refuse it.
                if url.trim_end_matches('/').ends_with("/sse") {
                    (sse(url, headers, oauth).await?, timeout)
                } else {
                    match Self::init(name, http, timeout).await {
                        Ok(c) => return Ok(c),
                        Err(e) if e.downcast_ref::<NeedsLogin>().is_some() => return Err(e),
                        Err(e) => {
                            let text = format!("{e:#}");
                            if !["HTTP 404", "HTTP 405", "HTTP 400", "HTTP 406"].iter().any(|c| text.contains(c)) {
                                return Err(e);
                            }
                            (sse(url, headers, oauth).await.map_err(|s| anyhow!("{text}; SSE: {s:#}"))?, timeout)
                        }
                    }
                }
            }
        };
        Self::init(name, transport, timeout).await
    }

    /// The initialize handshake and the tool list.
    async fn init(name: &str, transport: Transport, timeout: u64) -> Result<Client> {
        let mut client = Client {
            name: name.into(),
            instructions: None,
            tools: Vec::new(),
            transport,
            next_id: AtomicU64::new(1),
            timeout: Duration::from_millis(timeout),
        };
        let init = client
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL,
                    "capabilities": {},
                    "clientInfo": { "name": "codeit", "version": env!("CARGO_PKG_VERSION") }
                }),
                client.timeout,
            )
            .await?;
        client.instructions = init["instructions"].as_str().filter(|s| !s.trim().is_empty()).map(String::from);
        client.notify("notifications/initialized").await?;
        let mut cursor: Option<String> = None;
        loop {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let page = client.request("tools/list", params, client.timeout).await?;
            for t in page["tools"].as_array().into_iter().flatten() {
                let Some(n) = t["name"].as_str() else { continue };
                client.tools.push(ToolDef {
                    name: n.into(),
                    description: t["description"].as_str().unwrap_or_default().into(),
                    schema: t["inputSchema"].clone(),
                });
            }
            cursor = page["nextCursor"].as_str().map(String::from);
            if cursor.is_none() {
                break;
            }
        }
        Ok(client)
    }

    async fn notify(&self, method: &str) -> Result<()> {
        let msg = json!({ "jsonrpc": "2.0", "method": method });
        match &self.transport {
            Transport::Stdio { stdin, .. } => {
                let mut w = stdin.lock().await;
                w.write_all(format!("{msg}\n").as_bytes()).await?;
                w.flush().await?;
            }
            Transport::Http { .. } => {
                self.post(&msg).await?;
            }
            Transport::Sse { post_url, headers, .. } => {
                sse_post(post_url, headers, &msg).await?;
            }
        }
        Ok(())
    }

    pub async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let reply = match &self.transport {
            Transport::Stdio { stdin, pending, .. } => {
                let (tx, rx) = oneshot::channel();
                pending.lock().unwrap().insert(id, tx);
                {
                    let mut w = stdin.lock().await;
                    w.write_all(format!("{msg}\n").as_bytes()).await?;
                    w.flush().await?;
                }
                match tokio::time::timeout(timeout, rx).await {
                    Ok(Ok(v)) => v,
                    Ok(Err(_)) => bail!("{}: the server exited", self.name),
                    Err(_) => {
                        pending.lock().unwrap().remove(&id);
                        bail!("{}: {method} timed out", self.name)
                    }
                }
            }
            Transport::Http { .. } => tokio::time::timeout(timeout, self.post(&msg))
                .await
                .map_err(|_| anyhow!("{}: {method} timed out", self.name))??
                .ok_or_else(|| anyhow!("{}: empty reply to {method}", self.name))?,
            Transport::Sse { post_url, headers, pending, .. } => {
                let (tx, rx) = oneshot::channel();
                pending.lock().unwrap().insert(id, tx);
                sse_post(post_url, headers, &msg).await?;
                match tokio::time::timeout(timeout, rx).await {
                    Ok(Ok(v)) => v,
                    Ok(Err(_)) => bail!("{}: the event stream closed", self.name),
                    Err(_) => {
                        pending.lock().unwrap().remove(&id);
                        bail!("{}: {method} timed out", self.name)
                    }
                }
            }
        };
        if let Some(e) = reply.get("error") {
            bail!("{}: {}", self.name, e["message"].as_str().unwrap_or(&e.to_string()));
        }
        Ok(reply["result"].clone())
    }

    /// Streamable HTTP: POST one message; the reply is JSON or an SSE stream.
    async fn post(&self, msg: &Value) -> Result<Option<Value>> {
        let Transport::Http { url, headers, session, oauth } = &self.transport else { unreachable!() };
        let send = |bearer: Option<String>| {
            let mut req = reqwest::Client::new()
                .post(url)
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .header("mcp-protocol-version", PROTOCOL);
            for (k, v) in headers {
                req = req.header(k, v);
            }
            if let Some(b) = bearer {
                req = req.header("authorization", format!("Bearer {b}"));
            }
            if let Some(s) = session.lock().unwrap().clone() {
                req = req.header("mcp-session-id", s);
            }
            req.json(msg).send()
        };
        let token = if *oauth { crate::mcp_oauth::token(url).await } else { None };
        let mut res = send(token.clone()).await?;
        if res.status().as_u16() == 401 && *oauth {
            let hint = res
                .headers()
                .get("www-authenticate")
                .and_then(|v| v.to_str().ok())
                .and_then(crate::mcp_oauth::resource_metadata);
            // A stored login may only need refreshing.
            match token.is_some().then_some(()).map(|_| crate::mcp_oauth::refresh(url)) {
                Some(r) => match r.await {
                    Ok(fresh) => res = send(Some(fresh)).await?,
                    Err(_) => return Err(NeedsLogin(hint).into()),
                },
                None => return Err(NeedsLogin(hint).into()),
            }
            if res.status().as_u16() == 401 {
                return Err(NeedsLogin(hint).into());
            }
        }
        if let Some(s) = res.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
            *session.lock().unwrap() = Some(s.to_string());
        }
        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            bail!("{}: HTTP {status}: {}", self.name, text.chars().take(300).collect::<String>());
        }
        let Some(id) = msg.get("id").cloned() else { return Ok(None) };
        let kind = res.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        if !kind.contains("event-stream") {
            let text = res.text().await?;
            if text.trim().is_empty() {
                return Ok(None);
            }
            return Ok(Some(serde_json::from_str(&text)?));
        }
        // SSE: read events until the reply with our id.
        let mut buf = String::new();
        let mut stream = res.bytes_stream();
        while let Some(chunk) = stream.next().await {
            buf.push_str(&String::from_utf8_lossy(&chunk?));
            while let Some(end) = buf.find("\n\n") {
                let event: String = buf.drain(..end + 2).collect();
                let data: String = event
                    .lines()
                    .filter_map(|l| l.strip_prefix("data:"))
                    .map(|d| d.strip_prefix(' ').unwrap_or(d))
                    .collect::<Vec<_>>()
                    .join("\n");
                if let Ok(v) = serde_json::from_str::<Value>(&data)
                    && v["id"] == id
                {
                    return Ok(Some(v));
                }
            }
        }
        bail!("{}: the stream ended without a reply", self.name)
    }

    /// Calls a tool: (text, images as (media type, base64), is an error).
    pub async fn call(&self, tool: &str, args: Value) -> Result<(String, Vec<(String, String)>, bool)> {
        let args = if args.is_object() { args } else { json!({}) };
        let r =
            self.request("tools/call", json!({ "name": tool, "arguments": args }), Duration::from_secs(600)).await?;
        let mut parts = Vec::new();
        let mut images = Vec::new();
        for c in r["content"].as_array().into_iter().flatten() {
            match c["type"].as_str() {
                Some("text") => parts.push(c["text"].as_str().unwrap_or_default().to_string()),
                Some("image") if c["data"].is_string() => {
                    images.push((
                        c["mimeType"].as_str().unwrap_or("image/png").to_string(),
                        c["data"].as_str().unwrap_or_default().to_string(),
                    ));
                    parts.push("[image attached]".into());
                }
                Some("resource") => parts.push(
                    c["resource"]["text"]
                        .as_str()
                        .map(String::from)
                        .unwrap_or_else(|| c["resource"]["uri"].to_string()),
                ),
                Some(other) => {
                    parts.push(format!("[{other} content: {}]", c["mimeType"].as_str().unwrap_or("unknown")))
                }
                None => {}
            }
        }
        if parts.is_empty() && !r["structuredContent"].is_null() {
            parts.push(r["structuredContent"].to_string());
        }
        Ok((parts.join("\n"), images, r["isError"] == true))
    }
}

/// Opens the older transport's event stream and waits for the endpoint it announces.
async fn sse(url: &str, headers: &BTreeMap<String, String>, oauth: bool) -> Result<Transport> {
    let mut req = reqwest::Client::new().get(url).header("accept", "text/event-stream");
    for (k, v) in headers {
        req = req.header(k, v);
    }
    if oauth && let Some(t) = crate::mcp_oauth::token(url).await {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let res = req.send().await?;
    if res.status().as_u16() == 401 && oauth {
        let hint = res
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
            .and_then(crate::mcp_oauth::resource_metadata);
        return Err(NeedsLogin(hint).into());
    }
    if !res.status().is_success() {
        bail!("SSE stream: HTTP {}", res.status());
    }
    let mut stream = res.bytes_stream();
    let mut buf = String::new();
    let endpoint = loop {
        let Some(chunk) = tokio::time::timeout(Duration::from_secs(15), stream.next()).await.ok().flatten() else {
            bail!("the SSE stream didn't announce an endpoint");
        };
        buf.push_str(&String::from_utf8_lossy(&chunk?).replace("\r\n", "\n"));
        if let Some(e) = take_events(&mut buf).into_iter().find(|(kind, _)| kind == "endpoint") {
            break e.1;
        }
    };
    let post_url = if endpoint.starts_with("http") {
        endpoint
    } else {
        let rest = url.split_once("://").map(|x| x.1).unwrap_or("");
        let origin = &url[..url.len() - rest.len() + rest.find('/').unwrap_or(rest.len())];
        format!("{origin}{}", if endpoint.starts_with('/') { endpoint } else { format!("/{endpoint}") })
    };
    let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>> = Default::default();
    let p = pending.clone();
    let reader = tokio::spawn(async move {
        while let Some(Ok(chunk)) = stream.next().await {
            buf.push_str(&String::from_utf8_lossy(&chunk).replace("\r\n", "\n"));
            for (_, data) in take_events(&mut buf) {
                if let Ok(msg) = serde_json::from_str::<Value>(&data)
                    && let Some(id) = msg["id"].as_u64()
                    && let Some(tx) = p.lock().unwrap().remove(&id)
                {
                    let _ = tx.send(msg);
                }
            }
        }
    });
    let mut headers = headers.clone();
    if oauth && let Some(t) = crate::mcp_oauth::token(url).await {
        headers.insert("authorization".into(), format!("Bearer {t}"));
    }
    Ok(Transport::Sse { post_url, headers, pending, _reader: reader })
}

/// Complete events at the start of `buf`, as (event name, data); removes them from `buf`.
fn take_events(buf: &mut String) -> Vec<(String, String)> {
    let mut out = Vec::new();
    while let Some(end) = buf.find("\n\n") {
        let event: String = buf.drain(..end + 2).collect();
        let mut kind = "message".to_string();
        let mut data = Vec::new();
        for l in event.lines() {
            if let Some(k) = l.strip_prefix("event:") {
                kind = k.trim().to_string();
            } else if let Some(d) = l.strip_prefix("data:") {
                data.push(d.strip_prefix(' ').unwrap_or(d).to_string());
            }
        }
        if !data.is_empty() {
            out.push((kind, data.join("\n")));
        }
    }
    out
}

async fn sse_post(url: &str, headers: &BTreeMap<String, String>, msg: &Value) -> Result<()> {
    let mut req = reqwest::Client::new().post(url).header("content-type", "application/json");
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let res = req.json(msg).send().await?;
    if !res.status().is_success() {
        bail!("HTTP {} posting to {url}", res.status());
    }
    Ok(())
}

struct McpTool {
    client: Arc<Client>,
    wire: String,
    def: ToolDef,
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.wire
    }

    fn spec(&self, _: &Harness) -> ToolSpec {
        let mut schema = self.def.schema.clone();
        if !schema.is_object() {
            schema = json!({ "type": "object", "properties": {} });
        }
        if schema.get("type").is_none() {
            schema["type"] = json!("object");
        }
        ToolSpec { name: self.wire.clone(), description: self.def.description.clone(), parameters: schema }
    }

    fn title(&self, input: &Value, _: &Path) -> String {
        let args = input.to_string();
        let args: String = args.chars().take(80).collect();
        format!("{} {args}", self.def.name)
    }

    async fn run(&self, input: Value, ctx: &Ctx) -> Result<Output> {
        ctx.ask(
            &self.wire,
            &["*".into()],
            &["*".into()],
            format!("{} ({})", self.def.name, self.client.name),
            Some(input.to_string()),
        )
        .await?;
        let (text, images, error) = tokio::select! {
            r = self.client.call(&self.def.name, input) => r?,
            _ = ctx.cancel.cancelled() => bail!("Interrupted by the user."),
        };
        if error {
            bail!("{text}");
        }
        Ok(Output {
            content: ctx.limit(if text.is_empty() { "(no output)" } else { &text }),
            images,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_tools_for_every_provider() {
        assert_eq!(wire_name("tracker", "getIssue"), "tracker_getIssue");
        assert_eq!(wire_name("my.server", "do it"), "my_server_do_it");
        assert_eq!(wire_name(&"x".repeat(70), "t").len(), 64);
    }

    #[test]
    fn splits_sse_events() {
        let mut buf =
            "event: endpoint\ndata: /messages?sid=1\n\nevent: message\ndata: {\"id\":1}\n\npartial".to_string();
        let ev = take_events(&mut buf);
        assert_eq!(
            ev,
            [
                ("endpoint".to_string(), "/messages?sid=1".to_string()),
                ("message".to_string(), "{\"id\":1}".to_string())
            ]
        );
        assert_eq!(buf, "partial");
    }
}
