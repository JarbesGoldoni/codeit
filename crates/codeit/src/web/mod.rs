//! `codeit web`: codeit in a browser tab. A local server runs the agent (the same harness as the
//! TUI) and streams each session to the React app built into the binary (`web/` in the repo)
//! over a WebSocket. It also takes speech to transcribe ([`voice`]) and cookies from the browser
//! extension ([`bridge`]).
//!
//! The server only answers on localhost, and only to its own page (the `Origin` and `Host`
//! headers are checked), since whoever can talk to it can run commands.

mod assets;
mod bridge;
mod code;
mod panel;
mod timeline;
mod voice;

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use axum::Router;
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Request, State as AxState};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use codeit_harness::event::{Ask, Event, Reply as AskReply};
use codeit_harness::session::{Approval, Session};
use codeit_harness::{Harness, Input, Run};
use codeit_providers::{AuthStatus, ModelInfo};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::state::State;
use timeline::{Item, Op, Timeline};

pub const DEFAULT_PORT: u16 = 7317;

/// Built-in commands the web app offers: (name, arguments, what it does).
const BUILTIN: &[(&str, &str, &str)] = &[
    ("/models", "", "pick the model"),
    ("/effort", "[level]", "choose the reasoning effort"),
    ("/agent", "[name]", "switch agent: build or plan"),
    ("/new", "", "start a new session"),
    ("/session", "", "switch sessions; a running one keeps working"),
    ("/undo", "", "undo the last turn and its file changes"),
    ("/compact", "", "summarize the conversation to free context"),
    ("/todos", "clear", "clear the todo list"),
    ("/rename", "<title>", "rename this session"),
    ("/export", "[path]", "save this session as Markdown"),
    ("/approvals", "[auto|ask]", "ask before edits and commands, or not"),
    ("/login", "[provider]", "log in to a provider"),
    ("/logout", "<provider>", "remove a login codeit saved"),
    ("/status", "", "logins, MCP servers, skills, session"),
    ("/help", "", "list commands"),
];

struct Turn {
    /// Tells this turn's events apart from the next one's.
    id: u64,
    cancel: CancellationToken,
    started: Instant,
    model: String,
    agent: String,
    effort: Option<String>,
}

/// A session the server has open: on screen in some tab, or working.
struct Live {
    session: Arc<Mutex<Session>>,
    timeline: Timeline,
    /// Bumped with every change, so a tab drops changes its snapshot already has.
    version: u64,
    turn: Option<Turn>,
    /// Messages sent while it worked, sent in turn when it is done.
    queue: Vec<String>,
    /// (tokens used, usable limit) after the latest request.
    context: Option<(u64, u64)>,
}

struct Pending {
    session: String,
    ask: Ask,
    view: Value,
}

pub struct Server {
    harness: Arc<Harness>,
    /// Hosts the page may be reached at (`localhost:7317`…).
    hosts: Vec<String>,
    out: broadcast::Sender<Arc<str>>,
    saved: Mutex<State>,
    lives: Mutex<HashMap<String, Live>>,
    asks: Mutex<HashMap<u64, Pending>>,
    next: AtomicU64,
    models: Mutex<BTreeMap<&'static str, Result<Vec<ModelInfo>, String>>>,
    status: Mutex<(Option<Instant>, Value)>,
    /// Branch and changes of each folder shown: where codeit started (`""`) and the sessions'
    /// projects, by their path relative to it.
    gits: Mutex<BTreeMap<String, Value>>,
    bridge: bridge::Bridge,
    voice: Arc<voice::Voice>,
}

/// Messages from the app.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum In {
    /// Show a session (`None`: a new one).
    Open {
        session: Option<String>,
    },
    Send {
        session: String,
        text: String,
    },
    Stop {
        session: String,
    },
    /// Take back the last queued message.
    Unqueue {
        session: String,
    },
    Model {
        session: String,
        model: String,
    },
    Effort {
        session: String,
        effort: Option<String>,
    },
    Agent {
        session: String,
        agent: String,
    },
    Approval {
        session: String,
        approval: Approval,
    },
    Permission {
        id: u64,
        reply: String,
        message: Option<String>,
    },
    Question {
        id: u64,
        answers: Option<Vec<Vec<String>>>,
    },
    Panel {
        seq: u64,
        session: String,
        command: String,
        action: panel::WireAction,
    },
    Sessions,
    Rename {
        session: String,
        title: String,
    },
    Delete {
        session: String,
    },
    Logins,
    LoginKey {
        provider: String,
        key: String,
    },
    LoginStart {
        provider: String,
        method: String,
        enterprise: Option<String>,
    },
    Voice {
        model: Option<String>,
        language: Option<String>,
    },
    VoiceDownload {
        model: String,
    },
    BridgeRefresh {
        site: String,
    },
    /// Move a session to a project (a folder relative to where codeit started; `""` for it).
    Project {
        session: String,
        path: String,
    },
    BridgeReload,
}

pub struct Options {
    pub port: u16,
    pub open: bool,
    pub resume: Option<String>,
}

fn parse(args: &[&str]) -> Result<Options> {
    let mut o = Options { port: DEFAULT_PORT, open: true, resume: None };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "-p" | "--port" => {
                o.port = it.next().and_then(|p| p.parse().ok()).ok_or_else(|| anyhow!("--port needs a number"))?
            }
            "--no-open" => o.open = false,
            "-c" | "--continue" => o.resume = Some(String::new()),
            "-s" | "--session" => o.resume = it.next().map(|s| s.to_string()),
            other => bail!("unknown option `{other}` for codeit web"),
        }
    }
    Ok(o)
}

/// `codeit web [--port N] [--no-open] [-c | -s ID]`.
pub async fn run(args: &[&str]) -> Result<()> {
    let opts = parse(args)?;
    let cwd = std::env::current_dir()?;
    let harness = Harness::new(&cwd, crate::providers(), crate::extensions()).await;
    for p in &harness.problems {
        eprintln!("config: {p}");
    }
    let (listener, port) = bind(opts.port).await?;
    let server = Server::new(harness, port);
    server.load_models();
    server.refresh_status(Duration::ZERO);
    server.watch_git();
    server.load_sites();

    let app = Router::new()
        .route("/api/ws", get(ws_app))
        .route("/api/bridge", get(ws_bridge))
        .route("/api/voice", post(transcribe))
        .route("/api/image", post(upload_image))
        .route("/api/code", post(code::api))
        .route("/skin/{*path}", get(skin_file))
        .fallback(assets::serve)
        .layer(middleware::from_fn_with_state(server.clone(), guard))
        .with_state(server.clone());

    let mut url = format!("http://localhost:{port}/");
    if let Some(id) = opts.resume {
        let id = if id.is_empty() {
            Session::list(Some(&server.harness.cwd)).first().map(|m| m.id.clone()).unwrap_or_default()
        } else {
            id
        };
        if !id.is_empty() {
            url.push_str(&format!("#{id}"));
        }
    }
    println!("codeit is running at {url}");
    println!("Folder: {}", server.harness.cwd.display());
    println!("Ctrl+C stops it.");
    if opts.open {
        codeit_harness::util::open_url(&url);
    }
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

/// Listens on localhost at `port`, or the next free one.
async fn bind(port: u16) -> Result<(tokio::net::TcpListener, u16)> {
    let mut last = None;
    for p in port..port.saturating_add(20) {
        match tokio::net::TcpListener::bind(("127.0.0.1", p)).await {
            Ok(l) => return Ok((l, p)),
            Err(e) => last = Some(e),
        }
    }
    Err(anyhow!("no free port from {port}: {}", last.map(|e| e.to_string()).unwrap_or_default()))
}

/// Refuses requests that don't come from the app itself (or, for the bridge, from a browser
/// extension): another site open in the browser could otherwise drive the agent.
async fn guard(AxState(s): AxState<Arc<Server>>, req: Request, next: Next) -> Response {
    let headers = req.headers();
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or_default();
    if !s.hosts.iter().any(|h| h == host) {
        return (StatusCode::FORBIDDEN, "codeit only answers on localhost").into_response();
    }
    let origin = headers.get(header::ORIGIN).and_then(|h| h.to_str().ok());
    let path = req.uri().path();
    let ok = match origin {
        Some(o) if path == "/api/bridge" => o.starts_with("chrome-extension://") || o.starts_with("moz-extension://"),
        Some(o) => o.strip_prefix("http://").is_some_and(|h| s.hosts.iter().any(|x| x == h)),
        // Plain page loads carry no Origin; anything that acts must.
        None => !path.starts_with("/api/"),
    };
    if !ok {
        return (StatusCode::FORBIDDEN, "codeit only answers its own page").into_response();
    }
    next.run(req).await
}

async fn ws_app(AxState(s): AxState<Arc<Server>>, ws: WebSocketUpgrade) -> Response {
    ws.max_message_size(8 << 20).on_upgrade(move |socket| s.client(socket))
}

async fn ws_bridge(AxState(s): AxState<Arc<Server>>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| bridge::client(s, socket))
}

/// Speech to text: the body is 16 kHz mono 16-bit samples (little-endian).
async fn transcribe(AxState(s): AxState<Arc<Server>>, body: Bytes) -> Response {
    let samples: Vec<i16> = body.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect();
    let (model, language) = s.voice_settings();
    match s.voice.transcribe(samples, &model, &language).await {
        Ok(text) => axum::Json(json!({ "text": text })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, axum::Json(json!({ "error": format!("{e:#}") }))).into_response(),
    }
}

/// The plugin's theme files (`crate::skin`).
async fn skin_file(axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    let Some(skin) = crate::skin() else { return StatusCode::NOT_FOUND.into_response() };
    let body: Option<&'static [u8]> = match path.as_str() {
        "theme.json" => skin.theme.map(str::as_bytes),
        "icons.json" => skin.icons.map(str::as_bytes),
        "product-icons.json" => skin.product_icons.map(str::as_bytes),
        p => (skin.file)(p),
    };
    let Some(body) = body else { return StatusCode::NOT_FOUND.into_response() };
    let kind = assets::content_type(&path);
    ([(header::CONTENT_TYPE, kind), (header::CACHE_CONTROL, "no-cache")], body).into_response()
}

/// A pasted or dropped image: saved under the cache, and its path given back for an `@path`.
async fn upload_image(headers: HeaderMap, body: Bytes) -> Response {
    let ext = match headers.get(header::CONTENT_TYPE).and_then(|h| h.to_str().ok()).unwrap_or_default() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => return (StatusCode::UNSUPPORTED_MEDIA_TYPE, "PNG, JPEG, GIF or WebP only").into_response(),
    };
    if body.len() > 5 << 20 {
        return (StatusCode::PAYLOAD_TOO_LARGE, "images up to 5 MB").into_response();
    }
    let dir = codeit_providers::cache_dir().join("paste");
    let path = dir.join(format!("{}-{}.{ext}", codeit_harness::util::now(), uuid::Uuid::new_v4().simple()));
    match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, &body)) {
        Ok(()) => axum::Json(json!({ "path": path })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl Server {
    fn new(harness: Arc<Harness>, port: u16) -> Arc<Server> {
        let (out, _) = broadcast::channel(4096);
        let hosts = ["localhost", "127.0.0.1", "[::1]"].iter().map(|h| format!("{h}:{port}")).collect();
        let notify = out.clone();
        let voice = voice::Voice::new(move |v| {
            let _ = notify.send(json!({ "type": "voice", "voice": v }).to_string().into());
        });
        Arc::new(Server {
            harness,
            hosts,
            out,
            saved: Mutex::new(State::load()),
            lives: Mutex::default(),
            asks: Mutex::default(),
            next: AtomicU64::new(1),
            models: Mutex::default(),
            status: Mutex::new((None, json!([]))),
            gits: Mutex::default(),
            bridge: bridge::Bridge::default(),
            voice: Arc::new(voice),
        })
    }

    fn broadcast(&self, v: Value) {
        let _ = self.out.send(v.to_string().into());
    }

    fn id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    // ── a tab ───────────────────────────────────────────────────────────────

    async fn client(self: Arc<Self>, socket: WebSocket) {
        let (mut sink, mut stream) = socket.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let mut all = self.out.subscribe();
        let _ = tx.send(self.hello().to_string());
        let writer = tokio::spawn(async move {
            loop {
                let text: String = tokio::select! {
                    m = rx.recv() => match m {
                        Some(m) => m,
                        None => break,
                    },
                    m = all.recv() => match m {
                        Ok(m) => m.to_string(),
                        // Too far behind: the tab asks for its session again.
                        Err(broadcast::error::RecvError::Lagged(_)) => json!({"type": "resync"}).to_string(),
                        Err(_) => break,
                    },
                };
                if sink.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
        });
        while let Some(Ok(msg)) = stream.next().await {
            let Message::Text(text) = msg else { continue };
            match serde_json::from_str::<In>(&text) {
                Ok(m) => self.clone().handle(m, &tx).await,
                Err(e) => {
                    let _ = tx
                        .send(json!({"type": "toast", "text": format!("bad message: {e}"), "error": true}).to_string());
                }
            }
        }
        writer.abort();
    }

    fn hello(&self) -> Value {
        let saved = self.saved.lock().unwrap();
        let h = &self.harness;
        let agents: Vec<Value> =
            h.primary_agents().iter().map(|a| json!({"name": a.name, "description": a.description})).collect();
        let project = h.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        json!({
            "type": "hello",
            "version": env!("CARGO_PKG_VERSION"),
            "cwd": home_relative(&h.cwd),
            "project": project,
            "agents": agents,
            "commands": self.commands(),
            "plugins": self.plugins(),
            "models": self.models_json(),
            "settings": {"model": saved.model, "effort": saved.effort, "approval": saved.approval},
            "voice": self.voice.describe(
                saved.voice_model.as_deref().unwrap_or(voice::DEFAULT_MODEL),
                saved.voice_language.as_deref().unwrap_or("auto"),
            ),
            "status": self.status.lock().unwrap().1,
            "gits": *self.gits.lock().unwrap(),
            "workspace": codeit_harness::project::is_workspace(h),
            "skin": crate::skin().map(|s| json!({
                "name": s.name,
                "theme": s.theme.is_some(),
                "icons": s.icons.is_some(),
                "product_icons": s.product_icons.is_some(),
            })),
            "bridge": self.bridge.describe(),
            "problems": h.problems,
            "sessions": self.sessions(),
        })
    }

    async fn handle(self: Arc<Self>, m: In, tx: &mpsc::UnboundedSender<String>) {
        let reply = |v: Value| {
            let _ = tx.send(v.to_string());
        };
        match m {
            In::Open { session } => reply(self.open(session)),
            In::Send { session, text } => self.send(&session, text, tx),
            In::Stop { session } => self.stop(&session),
            In::Unqueue { session } => {
                let text = self.with_live(&session, |l, _| l.queue.pop()).flatten();
                if let Some(text) = text {
                    self.broadcast_turn(&session);
                    reply(json!({"type": "input", "session": session, "text": text}));
                }
            }
            In::Model { session, model } => {
                let info = self.model_info(&model);
                self.with_live(&session, |l, _| {
                    let mut s = l.session.lock().unwrap();
                    s.model = Some(model.clone());
                    if let (Some(info), Some(e)) = (&info, &s.effort)
                        && !info.efforts.contains(e)
                    {
                        s.effort = None;
                    }
                });
                let mut saved = self.saved.lock().unwrap();
                saved.model = Some(model);
                saved.save();
                drop(saved);
                self.broadcast_settings(&session);
            }
            In::Effort { session, effort } => {
                self.with_live(&session, |l, _| l.session.lock().unwrap().effort = effort.clone());
                let mut saved = self.saved.lock().unwrap();
                saved.effort = effort;
                saved.save();
                drop(saved);
                self.broadcast_settings(&session);
            }
            In::Agent { session, agent } => {
                if self.harness.primary_agents().iter().any(|a| a.name == agent) {
                    self.with_live(&session, |l, _| l.session.lock().unwrap().agent = agent);
                    self.broadcast_settings(&session);
                }
            }
            In::Approval { session, approval } => {
                self.with_live(&session, |l, _| l.session.lock().unwrap().approval = approval);
                let mut saved = self.saved.lock().unwrap();
                saved.approval = approval;
                saved.save();
                drop(saved);
                self.broadcast_settings(&session);
            }
            In::Permission { id, reply: r, message } => {
                if let Some(Pending { ask: Ask::Permission(p), .. }) = self.take_ask(id) {
                    let r = match r.as_str() {
                        "once" => AskReply::Once,
                        "always" => AskReply::Always,
                        _ => AskReply::Reject(message.filter(|m| !m.trim().is_empty())),
                    };
                    let _ = p.reply.send(r);
                }
            }
            In::Question { id, answers } => {
                if let Some(Pending { ask: Ask::Question(q), .. }) = self.take_ask(id) {
                    let _ = q.reply.send(answers);
                }
            }
            In::Panel { seq, session, command, action } => {
                let Some(action) = action.action() else { return };
                let Some(ext) =
                    self.harness.extensions.iter().find(|e| e.commands().iter().any(|c| c.panel && c.name == command))
                else {
                    return reply(json!({"type": "panel", "seq": seq, "reply": {"kind": "close"}}));
                };
                let (me, ext, tx) = (self.clone(), ext.clone(), tx.clone());
                tokio::spawn(async move {
                    let r = ext.act(&command, action).await;
                    let _ = tx.send(json!({"type": "panel", "seq": seq, "reply": panel::reply(&r)}).to_string());
                    match r {
                        codeit_harness::extension::Reply::Prompt { text, session: target } => {
                            let sid = match target {
                                Some(t) => me.session_titled(&t.prefix, &t.title),
                                None => session,
                            };
                            let _ = tx.send(me.open(Some(sid.clone())).to_string());
                            if !text.trim().is_empty() {
                                me.send(&sid, text, &tx);
                            }
                        }
                        codeit_harness::extension::Reply::Notice(text) => me.notice(&session, text, false),
                        _ => {}
                    }
                });
            }
            In::Sessions => reply(json!({"type": "sessions", "sessions": self.sessions()})),
            In::Rename { session, title } => self.rename(&session, &title),
            In::Delete { session } => {
                let busy = self.with_live(&session, |l, _| l.turn.is_some()).unwrap_or(false);
                if busy {
                    return reply(json!({"type": "toast", "text": "Stop it before deleting it.", "error": true}));
                }
                self.lives.lock().unwrap().remove(&session);
                Session::delete(&session);
                self.broadcast(json!({"type": "sessions", "sessions": self.sessions()}));
            }
            In::Logins => {
                let me = self.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let list: Vec<Value> = crate::login::choices(&me.harness.providers)
                        .await
                        .into_iter()
                        .map(|c| {
                            json!({
                                "id": c.id, "name": c.name, "hint": c.hint, "logged_in": c.logged_in,
                                "methods": c.methods.iter().map(|m| json!({"id": format!("{m:?}"), "label": m.label()})).collect::<Vec<_>>(),
                                "doc": c.doc, "env": c.env, "how": c.how,
                            })
                        })
                        .collect();
                    let _ = tx.send(json!({"type": "logins", "providers": list}).to_string());
                });
            }
            In::LoginKey { provider, key } => match crate::login::save_key(&provider, &key) {
                Ok(()) => {
                    reply(json!({"type": "login_done", "ok": true, "text": format!("Saved the key for {provider}.")}));
                    self.load_models();
                }
                Err(e) => reply(json!({"type": "login_done", "ok": false, "text": format!("{e:#}")})),
            },
            In::LoginStart { provider, method, enterprise } => self.login(provider, method, enterprise, tx.clone()),
            In::Voice { model, language } => {
                let mut saved = self.saved.lock().unwrap();
                if let Some(m) = model.filter(|m| voice::MODELS.iter().any(|(n, ..)| n == m)) {
                    saved.voice_model = Some(m);
                }
                if let Some(l) = language {
                    saved.voice_language = Some(l);
                }
                saved.save();
                drop(saved);
                let (model, language) = self.voice_settings();
                self.broadcast(json!({"type": "voice", "voice": self.voice.describe(&model, &language)}));
            }
            In::VoiceDownload { model } => {
                let me = self.clone();
                tokio::spawn(async move {
                    let (_, language) = me.voice_settings();
                    let _ = me.voice.ensure(&model, &language).await;
                });
            }
            In::BridgeRefresh { site } => self.bridge.refresh(&site, true),
            In::Project { session, path } => {
                let dir = match self.project_dir(&path) {
                    Ok(d) => d,
                    Err(e) => return reply(json!({"type": "toast", "text": format!("{e:#}"), "error": true})),
                };
                let cwd = self.harness.cwd.clone();
                self.with_live(&session, |l, _| {
                    let mut s = l.session.lock().unwrap();
                    let project = (dir != cwd).then(|| dir.clone());
                    if s.project != project {
                        s.project = project;
                        s.project_moved = true;
                        if !s.entries.is_empty() {
                            let _ = s.save();
                        }
                    }
                });
                self.moved(&session, &dir);
            }
            In::BridgeReload => self.load_sites(),
        }
    }

    fn voice_settings(&self) -> (String, String) {
        let s = self.saved.lock().unwrap();
        (
            s.voice_model.clone().unwrap_or_else(|| voice::DEFAULT_MODEL.into()),
            s.voice_language.clone().unwrap_or_else(|| "auto".into()),
        )
    }

    fn login(
        self: &Arc<Self>,
        provider: String,
        method: String,
        enterprise: Option<String>,
        tx: mpsc::UnboundedSender<String>,
    ) {
        use crate::login::Method;
        let method = match method.as_str() {
            "Copilot" => Method::Copilot,
            "CopilotEnterprise" => Method::CopilotEnterprise,
            "ChatGptBrowser" => Method::ChatGptBrowser,
            "ChatGptDevice" => Method::ChatGptDevice,
            _ => {
                let _ = tx.send(json!({"type": "login_done", "ok": false, "text": "unknown login method"}).to_string());
                return;
            }
        };
        let me = self.clone();
        tokio::spawn(async move {
            let done = |ok: bool, text: String| {
                let _ = tx.send(json!({"type": "login_done", "ok": ok, "text": text}).to_string());
            };
            match crate::login::start(method, enterprise.as_deref()).await {
                Ok(started) => {
                    let _ = tx.send(
                        json!({"type": "login_started", "provider": provider, "url": started.url, "code": started.code})
                            .to_string(),
                    );
                    if started.code.is_empty() {
                        codeit_harness::util::open_url(&started.url);
                    }
                    match started.wait.await {
                        Ok(()) => {
                            done(true, format!("Logged in to {provider}."));
                            me.load_models();
                        }
                        Err(e) => done(false, format!("{e:#}")),
                    }
                }
                Err(e) => done(false, format!("{e:#}")),
            }
        });
    }

    // ── sessions ────────────────────────────────────────────────────────────

    /// Runs `f` on a live session (loading it if it is saved but not open yet).
    fn with_live<R>(&self, id: &str, f: impl FnOnce(&mut Live, &Server) -> R) -> Option<R> {
        let mut lives = self.lives.lock().unwrap();
        if !lives.contains_key(id) {
            let s = Session::load(id).ok()?;
            let timeline = Timeline::from_session(&s, &self.harness, |k| self.model_name(k));
            lives.insert(
                id.to_string(),
                Live {
                    session: Arc::new(Mutex::new(s)),
                    timeline,
                    version: 0,
                    turn: None,
                    queue: Vec::new(),
                    context: None,
                },
            );
        }
        lives.get_mut(id).map(|l| f(l, self))
    }

    fn new_session(&self) -> String {
        let h = &self.harness;
        let saved = self.saved.lock().unwrap();
        let agent = h
            .config
            .default_agent
            .clone()
            .filter(|a| h.primary_agents().iter().any(|x| &x.name == a))
            .unwrap_or_else(|| "build".into());
        let mut s = Session::new(&h.cwd, &agent, saved.model.clone(), saved.effort.clone());
        s.approval = match h.config.approval.as_deref() {
            Some("ask") => Approval::Ask,
            Some("auto") => Approval::Auto,
            _ => saved.approval,
        };
        drop(saved);
        let id = s.id.clone();
        self.lives.lock().unwrap().insert(
            id.clone(),
            Live {
                session: Arc::new(Mutex::new(s)),
                timeline: Timeline::default(),
                version: 0,
                turn: None,
                queue: Vec::new(),
                context: None,
            },
        );
        id
    }

    /// The saved session of this folder whose title starts with `prefix` (a task key), or a new
    /// one titled `title`.
    fn session_titled(&self, prefix: &str, title: &str) -> String {
        let cwd = &self.harness.cwd;
        if let Some(m) = Session::list(Some(cwd)).into_iter().find(|m| m.title.starts_with(prefix)) {
            return m.id;
        }
        let open = self.lives.lock().unwrap().iter().find_map(|(id, l)| {
            let s = l.session.lock().unwrap();
            s.title.starts_with(prefix).then(|| id.clone())
        });
        if let Some(id) = open {
            return id;
        }
        let id = self.new_session();
        self.with_live(&id, |l, _| l.session.lock().unwrap().title = title.to_string());
        id
    }

    /// Everything a tab needs to show a session.
    fn open(&self, id: Option<String>) -> Value {
        let id = match id {
            Some(id) if self.with_live(&id, |_, _| ()).is_some() => id,
            // Deleted since: a new one instead.
            Some(_) | None => self.new_session(),
        };
        let asks: Vec<Value> = self
            .asks
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, p)| p.session == id)
            .map(|(k, p)| json!({"id": k, "ask": p.view}))
            .collect();
        self.with_live(&id, |l, me| {
            let s = l.session.lock().unwrap();
            json!({
                "type": "history",
                "session": id,
                "version": l.version,
                "title": s.title,
                "saved": !s.entries.is_empty(),
                "items": l.timeline.items,
                "busy": l.turn.is_some(),
                "started": l.turn.as_ref().map(|t| now_ms() - t.started.elapsed().as_millis() as u64),
                "queue": l.queue,
                "asks": asks,
                "todos": s.todos,
                "project": me.project_rel(&s.workdir()),
                "context": l.context,
                "settings": me.settings_of(&s),
            })
        })
        .unwrap_or(Value::Null)
    }

    fn settings_of(&self, s: &Session) -> Value {
        json!({"model": s.model, "effort": s.effort, "agent": s.agent, "approval": s.approval})
    }

    fn broadcast_settings(&self, id: &str) {
        let v = self.with_live(id, |l, me| me.settings_of(&l.session.lock().unwrap()));
        if let Some(settings) = v {
            self.broadcast(json!({"type": "settings", "session": id, "settings": settings}));
        }
    }

    fn broadcast_turn(&self, id: &str) {
        let v = self.with_live(id, |l, _| {
            json!({
                "type": "turn",
                "session": id,
                "busy": l.turn.is_some(),
                "started": l.turn.as_ref().map(|t| now_ms() - t.started.elapsed().as_millis() as u64),
                "queue": l.queue,
            })
        });
        if let Some(v) = v {
            self.broadcast(v);
        }
    }

    /// Saved sessions of this folder, newest first, with the ones working or waiting marked.
    fn sessions(&self) -> Value {
        let lives = self.lives.lock().unwrap();
        let asks = self.asks.lock().unwrap();
        let list: Vec<Value> = Session::list(Some(&self.harness.cwd))
            .into_iter()
            .map(|m| {
                let busy = lives.get(&m.id).is_some_and(|l| l.turn.is_some());
                let waiting = asks.values().any(|p| p.session == m.id);
                json!({"id": m.id, "title": m.title, "updated": m.updated, "busy": busy, "waiting": waiting})
            })
            .collect();
        json!(list)
    }

    fn rename(&self, id: &str, title: &str) {
        let title = title.trim();
        if title.is_empty() {
            return;
        }
        let saved = self.with_live(id, |l, _| {
            let mut s = l.session.lock().unwrap();
            s.title = title.to_string();
            !s.entries.is_empty()
        });
        if saved == Some(true) {
            let _ = Session::rename(id, title);
        }
        self.broadcast(json!({"type": "title", "session": id, "title": title}));
        self.broadcast(json!({"type": "sessions", "sessions": self.sessions()}));
    }

    /// Changes a session's timeline and tells every tab.
    fn change(&self, id: &str, f: impl FnOnce(&mut Timeline) -> Vec<Op>) {
        let msg = self.with_live(id, |l, _| {
            let ops = f(&mut l.timeline);
            if ops.is_empty() {
                return None;
            }
            l.version += 1;
            Some(json!({"type": "ops", "session": id, "version": l.version, "ops": ops}))
        });
        if let Some(Some(m)) = msg {
            self.broadcast(m);
        }
    }

    /// Tells the tabs a session works in another folder, and looks at that folder's git now.
    fn moved(self: &Arc<Self>, id: &str, dir: &std::path::Path) {
        let rel = self.project_rel(dir);
        self.broadcast(json!({"type": "project", "session": id, "project": rel}));
        let (me, dir) = (self.clone(), dir.to_path_buf());
        tokio::spawn(async move {
            let v = tokio::task::spawn_blocking(move || git_state(&dir)).await.unwrap_or(Value::Null);
            me.gits.lock().unwrap().insert(rel.clone(), v.clone());
            me.broadcast(json!({"type": "git", "path": rel, "git": v}));
        });
    }

    fn notice(&self, id: &str, text: impl Into<String>, error: bool) {
        let text = text.into();
        self.change(id, |t| vec![t.push(Item::Notice { text, error })]);
    }

    // ── turns ───────────────────────────────────────────────────────────────

    fn send(self: &Arc<Self>, id: &str, text: String, tx: &mpsc::UnboundedSender<String>) {
        let text = text.trim_end().to_string();
        if text.trim().is_empty() {
            return;
        }
        let mut input = Input::Prompt(text.clone());
        if let Some(rest) = text.strip_prefix('/') {
            let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            let args = args.trim();
            if self.builtin(id, name, args, tx) {
                return;
            }
            if self.harness.extensions.iter().any(|e| e.commands().iter().any(|c| c.panel && c.name == name)) {
                let _ = tx.send(json!({"type": "open_panel", "command": name, "args": args}).to_string());
                return;
            }
            if self.harness.commands().iter().any(|c| c.name == name) {
                input = Input::Command { name: name.to_string(), args: args.to_string() };
            } else if !name.contains('/') {
                self.notice(id, format!("Unknown command /{name}. Type /help."), false);
                return;
            }
        }
        let queued = self.with_live(id, |l, _| {
            if l.turn.is_some() {
                l.queue.push(text.clone());
                true
            } else {
                false
            }
        });
        match queued {
            Some(true) => self.broadcast_turn(id),
            Some(false) => self.start(id, input, Some(text)),
            None => {}
        }
    }

    fn start(self: &Arc<Self>, id: &str, input: Input, shown: Option<String>) {
        let started = self.with_live(id, |l, me| {
            let (model, agent) = {
                let s = l.session.lock().unwrap();
                (s.model.clone(), s.agent.clone())
            };
            let Some(model) = model else { return Err("Pick a model first (the model button, or /models).") };
            let info = me.model_info(&model);
            let effort = {
                let mut s = l.session.lock().unwrap();
                if let (Some(info), Some(e)) = (&info, &s.effort)
                    && !info.efforts.contains(e)
                {
                    s.effort = None;
                }
                s.effort.clone()
            };
            let turn = Turn {
                id: me.id(),
                cancel: CancellationToken::new(),
                started: Instant::now(),
                model: info
                    .map(|i| i.name)
                    .unwrap_or_else(|| model.split_once('/').map_or(model.clone(), |m| m.1.into())),
                agent,
                effort,
            };
            let run = Run {
                harness: me.harness.clone(),
                session: l.session.clone(),
                events: mpsc::unbounded_channel().0,
                cancel: turn.cancel.clone(),
                depth: 0,
            };
            let turn_id = turn.id;
            l.turn = Some(turn);
            Ok((run, turn_id))
        });
        let (mut run, turn) = match started {
            Some(Ok(r)) => r,
            Some(Err(e)) => return self.notice(id, e, true),
            None => return,
        };
        if let Some(text) = shown {
            self.change(id, |t| vec![t.push(Item::User { text })]);
        }
        self.broadcast_turn(id);
        self.broadcast(json!({"type": "sessions", "sessions": self.sessions()}));
        let (htx, mut hrx) = mpsc::unbounded_channel();
        run.events = htx;
        let (me, sid) = (self.clone(), id.to_string());
        tokio::spawn(async move {
            let (me2, sid2) = (me.clone(), sid.clone());
            let forward = tokio::spawn(async move {
                while let Some(ev) = hrx.recv().await {
                    me2.event(&sid2, turn, ev);
                }
            });
            run.start(input).await;
            drop(run);
            let _ = forward.await;
            // In case the turn ended without saying so.
            me.finish(&sid, turn);
        });
    }

    fn event(self: &Arc<Self>, id: &str, turn: u64, ev: Event) {
        match ev {
            Event::Ask(ask) => {
                let view = match &ask {
                    Ask::Permission(p) => json!({
                        "kind": "permission",
                        "permission": p.permission,
                        "patterns": p.patterns,
                        "always": p.always,
                        "title": p.title,
                        "detail": p.detail,
                    }),
                    Ask::Question(q) => json!({
                        "kind": "question",
                        "questions": q.questions.iter().map(|q| json!({
                            "question": q.question,
                            "header": q.header,
                            "options": q.options.iter().map(|(l, d)| json!({"label": l, "description": d})).collect::<Vec<_>>(),
                            "multiple": q.multiple,
                        })).collect::<Vec<_>>(),
                    }),
                };
                let n = self.id();
                self.asks.lock().unwrap().insert(n, Pending { session: id.to_string(), ask, view: view.clone() });
                self.broadcast(json!({"type": "ask", "session": id, "id": n, "ask": view}));
                self.broadcast(json!({"type": "sessions", "sessions": self.sessions()}));
            }
            Event::Todos(todos) => self.broadcast(json!({"type": "todos", "session": id, "todos": todos})),
            Event::Project(dir) => self.moved(id, &dir),
            Event::Title(title) => {
                self.broadcast(json!({"type": "title", "session": id, "title": title}));
                self.broadcast(json!({"type": "sessions", "sessions": self.sessions()}));
            }
            Event::Step { context, .. } => {
                if context.is_some() {
                    self.with_live(id, |l, _| l.context = context);
                    self.broadcast(json!({"type": "context", "session": id, "context": context}));
                }
            }
            Event::Done => self.finish(id, turn),
            ev => self.change(id, |t| t.apply(&ev)),
        }
    }

    fn take_ask(&self, id: u64) -> Option<Pending> {
        let p = self.asks.lock().unwrap().remove(&id);
        if p.is_some() {
            self.broadcast(json!({"type": "ask_done", "id": id}));
            self.broadcast(json!({"type": "sessions", "sessions": self.sessions()}));
        }
        p
    }

    /// Drops a session's open questions (they are refused).
    fn drop_asks(&self, id: &str) {
        let ids: Vec<u64> =
            self.asks.lock().unwrap().iter().filter(|(_, p)| p.session == id).map(|(k, _)| *k).collect();
        for k in ids {
            self.take_ask(k);
        }
    }

    fn finish(self: &Arc<Self>, id: &str, turn: u64) {
        let ended = self.with_live(id, |l, _| {
            if l.turn.as_ref().is_none_or(|t| t.id != turn) {
                return None;
            }
            let t = l.turn.take()?;
            let next = (!l.queue.is_empty()).then(|| l.queue.remove(0));
            Some((t, next))
        });
        let Some(Some((t, next))) = ended else { return };
        self.change(id, |tl| {
            tl.end_turn(Item::TurnEnd {
                agent: t.agent,
                model: t.model,
                effort: t.effort,
                secs: Some(t.started.elapsed().as_secs()),
            })
        });
        self.drop_asks(id);
        self.broadcast_turn(id);
        // Picks up commands and skills the turn wrote.
        self.harness.reload();
        self.broadcast(json!({"type": "commands", "commands": self.commands()}));
        self.broadcast(json!({"type": "sessions", "sessions": self.sessions()}));
        self.refresh_status(Duration::from_secs(60));
        if let Some(next) = next {
            let (tx, _) = mpsc::unbounded_channel();
            self.send(id, next, &tx);
        }
    }

    fn stop(&self, id: &str) {
        self.with_live(id, |l, _| {
            l.queue.clear();
            if let Some(t) = &l.turn {
                t.cancel.cancel();
            }
        });
        // A permission waiting for an answer is refused, so the turn can end.
        self.drop_asks(id);
        self.broadcast_turn(id);
    }

    // ── commands ────────────────────────────────────────────────────────────

    /// Built-in and plugin commands, with where each comes from.
    fn commands(&self) -> Value {
        let mut out: Vec<Value> = BUILTIN
            .iter()
            .map(|(n, a, h)| json!({"name": n, "args": a, "help": h, "long": "", "source": "codeit", "panel": false}))
            .collect();
        let mut seen: Vec<String> = BUILTIN.iter().map(|b| b.0.to_string()).collect();
        for e in &self.harness.extensions {
            for c in e.commands() {
                let name = format!("/{}", c.name);
                if seen.contains(&name) {
                    continue;
                }
                seen.push(name.clone());
                out.push(json!({
                    "name": name, "args": "", "help": c.description, "long": c.help,
                    "source": e.name(), "panel": c.panel,
                }));
            }
        }
        for c in self.harness.commands() {
            let name = format!("/{}", c.name);
            if seen.contains(&name) {
                continue;
            }
            seen.push(name.clone());
            out.push(json!({"name": name, "args": "", "help": c.description, "long": "", "source": "prompt", "panel": false}));
        }
        json!(out)
    }

    fn plugins(&self) -> Value {
        json!(
            self.harness
                .extensions
                .iter()
                .map(|e| json!({
                    "name": e.name(),
                    "commands": e.commands().iter().map(|c| format!("/{}", c.name)).collect::<Vec<_>>(),
                }))
                .collect::<Vec<_>>()
        )
    }

    /// Commands the server runs itself; false for the rest.
    fn builtin(self: &Arc<Self>, id: &str, name: &str, args: &str, tx: &mpsc::UnboundedSender<String>) -> bool {
        let idle = self.with_live(id, |l, _| l.turn.is_none()).unwrap_or(true);
        let open = |what: &str| {
            let _ = tx.send(json!({"type": "open", "what": what, "args": args}).to_string());
        };
        match (name, args) {
            ("models" | "session" | "login", _) | ("effort", "") => open(name),
            ("new", _) => open("new"),
            ("effort", level) => {
                let model = self.with_live(id, |l, _| l.session.lock().unwrap().model.clone()).flatten();
                let info = model.as_deref().and_then(|m| self.model_info(m));
                match info {
                    _ if level == "default" || level == "none" => {
                        self.with_live(id, |l, _| l.session.lock().unwrap().effort = None);
                        self.broadcast_settings(id);
                    }
                    Some(m) if m.efforts.iter().any(|e| e == level) => {
                        self.with_live(id, |l, _| l.session.lock().unwrap().effort = Some(level.to_string()));
                        self.broadcast_settings(id);
                    }
                    Some(m) if m.efforts.is_empty() => self.notice(id, format!("{} has no effort levels.", m.name), false),
                    Some(m) => self.notice(id, format!("{} accepts: default, {}", m.name, m.efforts.join(", ")), false),
                    None => self.notice(id, "Pick a model first.", false),
                }
            }
            ("agent", a) => {
                let names: Vec<String> = self.harness.primary_agents().iter().map(|x| x.name.clone()).collect();
                let next = if a.is_empty() {
                    let current = self.with_live(id, |l, _| l.session.lock().unwrap().agent.clone()).unwrap_or_default();
                    names.iter().position(|n| *n == current).map(|i| names[(i + 1) % names.len()].clone())
                } else {
                    names.iter().find(|n| *n == a).cloned()
                };
                match next {
                    Some(n) => {
                        self.with_live(id, |l, _| l.session.lock().unwrap().agent = n);
                        self.broadcast_settings(id);
                    }
                    None => self.notice(id, format!("Agents: {}", names.join(", ")), false),
                }
            }
            ("undo" | "compact", _) if !idle => {
                self.notice(id, "Wait for the current turn to finish (or stop it).", false)
            }
            ("undo", _) => {
                let undone = self.with_live(id, |l, me| {
                    let mut s = l.session.lock().unwrap();
                    let u = s.undo()?;
                    let _ = s.save();
                    l.timeline = Timeline::from_session(&s, &me.harness, |k| me.model_name(k));
                    Some(u)
                });
                match undone.flatten() {
                    Some(u) => {
                        // Every tab showing it starts over (tabs on other sessions stay).
                        let mut history = self.open(Some(id.to_string()));
                        history["refresh"] = json!(true);
                        self.broadcast(history);
                        let _ = tx.send(json!({"type": "input", "session": id, "text": u.prompt}).to_string());
                        let show = |v: &[std::path::PathBuf]| -> Vec<String> {
                            v.iter().map(|f| codeit_harness::util::display(&self.harness.cwd, f)).collect()
                        };
                        let (restored, skipped) = (show(&u.restored), show(&u.skipped));
                        let mut msg = if restored.is_empty() {
                            "Undid the last turn.".to_string()
                        } else {
                            format!("Undid the last turn and restored {}.", restored.join(", "))
                        };
                        if !skipped.is_empty() {
                            msg.push_str(&format!(" Left alone, since they changed after the turn: {}.", skipped.join(", ")));
                        }
                        self.notice(id, msg, false);
                    }
                    None => self.notice(id, "Nothing to undo.", false),
                }
            }
            ("compact", _) => self.start(id, Input::Compact, None),
            ("todos", "clear") => {
                self.with_live(id, |l, _| {
                    let mut s = l.session.lock().unwrap();
                    s.clear_todos();
                    if !s.entries.is_empty() {
                        let _ = s.save();
                    }
                });
                self.broadcast(json!({"type": "todos", "session": id, "todos": []}));
                self.notice(id, "Cleared the todo list; codeit hears about it with your next message.", false);
            }
            ("todos", _) => self.notice(id, "/todos clear empties the todo list.", false),
            ("rename", "") => self.notice(id, "Usage: /rename <new title>", false),
            ("rename", title) => self.rename(id, title),
            ("export", path) => {
                let s = self.with_live(id, |l, _| l.session.lock().unwrap().clone());
                let Some(s) = s else { return true };
                let path = if path.is_empty() {
                    codeit_providers::data_dir().join("exports").join(format!("{}.md", &s.id[..8.min(s.id.len())]))
                } else {
                    codeit_harness::util::resolve(&self.harness.cwd, path)
                };
                let r = path
                    .parent()
                    .map(std::fs::create_dir_all)
                    .unwrap_or(Ok(()))
                    .and_then(|_| std::fs::write(&path, crate::export::markdown(&s)));
                match r {
                    Ok(()) => self.notice(id, format!("Exported the session to {}", path.display()), false),
                    Err(e) => self.notice(id, format!("Couldn't export: {e}"), true),
                }
            }
            ("approvals", "") => {
                let a = self.with_live(id, |l, _| l.session.lock().unwrap().approval).unwrap_or_default();
                self.notice(
                    id,
                    match a {
                        Approval::Auto => "Approvals: auto (tools run without asking, except outside the project). /approvals ask to change.",
                        Approval::Ask => "Approvals: ask (edits, commands and fetches ask first). /approvals auto to change.",
                    },
                    false,
                );
            }
            ("approvals", mode) => {
                let a = if mode == "ask" { Approval::Ask } else { Approval::Auto };
                self.with_live(id, |l, _| l.session.lock().unwrap().approval = a);
                let mut saved = self.saved.lock().unwrap();
                saved.approval = a;
                saved.save();
                drop(saved);
                self.broadcast_settings(id);
            }
            ("logout", "") => self.notice(id, "Usage: /logout <provider>", false),
            ("logout", p) => match crate::login::logout(p) {
                Ok(true) => {
                    self.notice(id, format!("Removed codeit's login for {p}."), false);
                    self.load_models();
                }
                Ok(false) => self.notice(
                    id,
                    format!("codeit had no login saved for {p} (an opencode login or an environment variable, if any, still apply)."),
                    false,
                ),
                Err(e) => self.notice(id, format!("{e:#}"), true),
            },
            ("status", _) => {
                let (me, sid) = (self.clone(), id.to_string());
                tokio::spawn(async move {
                    let text = me.status_text(&sid).await;
                    me.notice(&sid, text, false);
                });
            }
            ("help", _) => {
                let list = self.commands();
                let lines: Vec<String> = list
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|c| {
                        let src = c["source"].as_str().unwrap_or_default();
                        let src = if src == "codeit" { String::new() } else { format!("  [{src}]") };
                        format!("{} {} — {}{src}", c["name"].as_str().unwrap_or_default(), c["args"].as_str().unwrap_or_default(), c["help"].as_str().unwrap_or_default())
                    })
                    .collect();
                self.notice(id, lines.join("\n"), false);
            }
            ("review" | "mcp" | "paste" | "copy" | "exit", _) => {
                self.notice(id, format!("/{name} is only in the terminal app for now."), false)
            }
            _ => return false,
        }
        true
    }

    async fn status_text(&self, id: &str) -> String {
        let h = &self.harness;
        let mut lines = Vec::new();
        let mut out = 0;
        for p in &h.providers {
            match p.status().await {
                AuthStatus::LoggedIn(s) => lines.push(format!("{}: logged in, {s}", p.name())),
                AuthStatus::LoggedOut(_) => out += 1,
            }
        }
        lines.push(format!("({out} more providers not logged in: /login adds one)"));
        let mcp = h.mcp.describe();
        lines.push(if mcp.is_empty() {
            "MCP servers: none".into()
        } else {
            format!("MCP servers: {}", mcp.join("; "))
        });
        let skills = h.skills();
        let skills: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        lines.push(format!("Skills: {}", if skills.is_empty() { "none".into() } else { skills.join(", ") }));
        let files: Vec<String> =
            h.instruction_files().iter().map(|f| codeit_harness::util::display(&h.cwd, f)).collect();
        lines.push(format!("Instructions: {}", if files.is_empty() { "none".into() } else { files.join(", ") }));
        let plugins: Vec<&str> = h.extensions.iter().map(|e| e.name()).collect();
        lines.push(format!("Plugins: {}", if plugins.is_empty() { "none".into() } else { plugins.join(", ") }));
        if let Some(s) = self.with_live(id, |l, _| {
            let s = l.session.lock().unwrap();
            format!(
                "Session: {} ({} messages){}",
                if s.title.is_empty() { "new" } else { &s.title },
                s.entries.len(),
                if s.summary.is_some() { ", summarized" } else { "" }
            )
        }) {
            lines.push(s);
        }
        lines.join("\n")
    }

    // ── models, plugins' status, git ────────────────────────────────────────

    fn load_models(self: &Arc<Self>) {
        for p in self.harness.providers.clone() {
            let me = self.clone();
            tokio::spawn(async move {
                // Providers you aren't logged in to simply have no models.
                let result = match p.status().await {
                    AuthStatus::LoggedOut(_) => Ok(Vec::new()),
                    AuthStatus::LoggedIn(_) => p.models().await.map_err(|e| format!("{e:#}")),
                };
                if let Ok(list) = &result {
                    me.harness.remember_models(list);
                }
                me.models.lock().unwrap().insert(p.id(), result);
                me.broadcast(json!({"type": "models", "models": me.models_json()}));
            });
        }
    }

    fn model_info(&self, key: &str) -> Option<ModelInfo> {
        let (pid, _) = codeit_providers::split_key(key)?;
        let models = self.models.lock().unwrap();
        models.get(pid)?.as_ref().ok()?.iter().find(|m| m.key() == key).cloned()
    }

    fn model_name(&self, key: &str) -> String {
        self.model_info(key).map(|m| m.name).unwrap_or_else(|| key.split_once('/').map_or(key, |k| k.1).to_string())
    }

    fn models_json(&self) -> Value {
        let models = self.models.lock().unwrap();
        let loading = self.harness.providers.len().saturating_sub(models.len());
        let mut list = Vec::new();
        let mut failed = Vec::new();
        for (pid, r) in models.iter() {
            match r {
                Ok(v) => list.extend(v.iter().map(|m| {
                    json!({
                        "key": m.key(), "provider": m.provider, "id": m.id, "name": m.name,
                        "context": m.context, "efforts": m.efforts, "default_effort": m.default_effort, "vision": m.vision,
                    })
                })),
                Err(e) => failed.push(json!({"provider": pid, "error": e})),
            }
        }
        json!({"list": list, "loading": loading, "failed": failed})
    }

    /// Asks the plugins for their footer, unless they were asked less than `min_age` ago.
    fn refresh_status(self: &Arc<Self>, min_age: Duration) {
        if self.harness.extensions.is_empty() {
            return;
        }
        {
            let mut s = self.status.lock().unwrap();
            if s.0.is_some_and(|t| t.elapsed() < min_age) {
                return;
            }
            s.0 = Some(Instant::now());
        }
        let me = self.clone();
        tokio::spawn(async move {
            let mut all = Vec::new();
            for e in &me.harness.extensions {
                let pieces = e.status().await;
                if !pieces.is_empty() {
                    all.push(json!({
                        "plugin": e.name(),
                        "pieces": pieces.iter().map(|(t, tone)| json!({"text": t, "tone": panel::tone(*tone)})).collect::<Vec<_>>(),
                    }));
                }
            }
            let v = json!(all);
            me.status.lock().unwrap().1 = v.clone();
            me.broadcast(json!({"type": "status", "status": v}));
            // Again in 5 minutes.
            let again = me.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(300)).await;
                again.refresh_status(Duration::from_secs(290));
            });
        });
    }

    /// The branch and how many files changed, every few seconds, for where codeit started and
    /// each open session's project; sent when it changes.
    fn watch_git(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                let mut dirs: BTreeMap<String, std::path::PathBuf> = BTreeMap::new();
                dirs.insert(String::new(), me.harness.cwd.clone());
                for l in me.lives.lock().unwrap().values() {
                    let dir = l.session.lock().unwrap().workdir();
                    dirs.insert(me.project_rel(&dir), dir);
                }
                for (rel, dir) in dirs {
                    let v = tokio::task::spawn_blocking(move || git_state(&dir)).await.unwrap_or(Value::Null);
                    let changed = me.gits.lock().unwrap().insert(rel.clone(), v.clone()).as_ref() != Some(&v);
                    if changed {
                        me.broadcast(json!({"type": "git", "path": rel, "git": v}));
                    }
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    }

    fn load_sites(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move { bridge::load_sites(&me).await });
    }
}

/// `~/workspace/app` for a folder under the home folder.
fn home_relative(p: &std::path::Path) -> String {
    match dirs::home_dir().and_then(|h| p.strip_prefix(h).ok().map(|r| r.to_path_buf())) {
        Some(r) if r.as_os_str().is_empty() => "~".into(),
        Some(r) => format!("~/{}", r.display()),
        None => p.display().to_string(),
    }
}

fn git_state(cwd: &std::path::Path) -> Value {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
    };
    let Some(branch) = git(&["rev-parse", "--abbrev-ref", "HEAD"]) else { return Value::Null };
    let (mut added, mut changed) = (0, 0);
    for l in git(&["status", "--porcelain"]).unwrap_or_default().lines() {
        if l.starts_with("??") || l.starts_with('A') {
            added += 1;
        } else {
            changed += 1;
        }
    }
    json!({"branch": branch.trim(), "added": added, "changed": changed})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_options() {
        let o = parse(&["--port", "9000", "--no-open", "-c"]).unwrap();
        assert_eq!(o.port, 9000);
        assert!(!o.open);
        assert_eq!(o.resume.as_deref(), Some(""));
        assert!(parse(&["--bogus"]).is_err());
    }
}
