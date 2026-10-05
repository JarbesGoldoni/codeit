//! TUI state and event handling. Drawing lives in `ui.rs`.

use std::cell::Cell;
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use codeit_harness::event::{Ask, Event as HEvent, PermissionAsk, Question, QuestionAsk, Reply};
use codeit_harness::session::{Approval, Session, SessionMeta, Todo};
use codeit_harness::{Harness, Input, Run};
use codeit_providers::{AuthStatus, ModelInfo, Part, Role, Usage};
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::state::State;

pub enum AppEvent {
    Term(Event),
    Tick,
    Models(&'static str, Result<Vec<ModelInfo>, String>),
    Harness(u64, HEvent),
    /// The providers to log in to, for the /login popup.
    LoginChoices(Vec<crate::login::Choice>),
    /// A browser login started: (provider name, URL, code to enter there or empty).
    LoginStarted(String, String, String),
    /// A login finished: the provider's name, or the error.
    LoginDone(Result<String, String>),
    /// What the side bubble shows that takes a while to find out.
    Side(SideUpdate),
    /// An MCP server's OAuth login: Ok(server) when done, or the error.
    McpLogin(Result<String, String>),
    Status(Vec<String>),
    /// An event of the review agent's run.
    Review(u64, HEvent),
    /// An extension's answer for its panel.
    Panel(u64, codeit_harness::extension::Reply),
    /// What the extensions show in the footer.
    ExtStatus(Vec<(String, codeit_harness::extension::Tone)>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolState {
    /// The model is still writing the call's arguments.
    Pending,
    Running,
    Done,
    Failed,
}

pub struct ToolItem {
    pub id: String,
    pub name: String,
    pub title: String,
    pub state: ToolState,
    pub output: String,
    pub diff: Option<String>,
    /// A subagent's latest steps.
    pub progress: Vec<String>,
    pub started: Option<Instant>,
    /// How long it ran, once finished.
    pub elapsed: Option<Duration>,
    /// What the model got instead of the full output, when it was condensed.
    pub digest: Option<String>,
    /// Lines in the full output.
    pub lines: Option<usize>,
}

impl ToolItem {
    pub(super) fn new(id: String, name: String, title: String, state: ToolState) -> Self {
        let started = (state == ToolState::Running).then(Instant::now);
        Self {
            id,
            name,
            title,
            state,
            output: String::new(),
            diff: None,
            progress: Vec::new(),
            started,
            elapsed: None,
            digest: None,
            lines: None,
        }
    }

    pub fn running(&self) -> bool {
        matches!(self.state, ToolState::Pending | ToolState::Running)
    }
}

pub enum Item {
    User(String),
    Assistant {
        text: String,
        reasoning: String,
        done: bool,
    },
    Tool(ToolItem),
    Todos(Vec<Todo>),
    /// End of a turn: who answered and how long it took.
    TurnEnd(TurnInfo),
    Notice(String),
    Error(String),
}

/// Who answered a turn, for the label in the corner of the answer.
#[derive(Clone, Debug)]
pub struct TurnInfo {
    pub agent: String,
    pub model: String,
    pub effort: Option<String>,
    pub secs: Option<u64>,
}

/// The side bubble's slow facts: git.
#[derive(Clone, Debug, Default)]
pub struct Side {
    pub branch: Option<String>,
    /// Files added and changed since the last commit.
    pub changes: (usize, usize),
}

pub enum SideUpdate {
    Git(Option<String>, (usize, usize)),
}

pub enum Catalog {
    Loading,
    Ready(Vec<ModelInfo>),
    Failed(String),
}

/// What each model is used for (`/models`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelRole {
    /// The build agent: the main model.
    Code,
    /// The plan agent and code reviews.
    Think,
}

impl ModelRole {
    pub const ALL: [ModelRole; 2] = [ModelRole::Code, ModelRole::Think];
    pub fn name(self) -> &'static str {
        match self {
            ModelRole::Code => "code",
            ModelRole::Think => "think",
        }
    }
    pub fn purpose(self) -> &'static str {
        match self {
            ModelRole::Code => "build agent: does the work",
            ModelRole::Think => "plan agent and code reviews",
        }
    }
}

pub enum PickerKind {
    Model(ModelRole),
    Roles,
    Session(Vec<SessionMeta>),
    /// The providers to log in to (loading while `None`).
    Login(Option<Vec<crate::login::Choice>>),
    /// How to log in to one provider.
    LoginMethod(crate::login::Choice),
    /// Typing a provider's API key (the filter holds it, hidden).
    Key(crate::login::Choice),
    /// Typing the GitHub Enterprise domain.
    Enterprise,
    /// The current model's reasoning effort levels.
    Effort,
}

pub struct Picker {
    pub kind: PickerKind,
    pub filter: String,
    pub selected: usize,
}

/// A row of a picker: (label, detail, marked).
pub type Row = (String, String, bool);

pub enum Dialog {
    Permission {
        ask: PermissionAsk,
        selected: usize,
        /// Typing the reason for a "no".
        feedback: Option<String>,
    },
    Question {
        questions: Vec<Question>,
        reply: tokio::sync::oneshot::Sender<Option<Vec<Vec<String>>>>,
        index: usize,
        selected: usize,
        chosen: Vec<usize>,
        answers: Vec<Vec<String>>,
        /// Typing a custom answer.
        typing: Option<String>,
    },
}

pub struct Command {
    pub name: String,
    pub args: String,
    pub help: String,
    /// Longer help, shown when this is the only command matching what is typed.
    pub long: String,
    /// The extension whose panel it opens.
    pub ext: Option<Arc<dyn codeit_harness::extension::Extension>>,
}

const BUILTIN: &[(&str, &str, &str)] = &[
    ("/models", "", "pick the code and think models"),
    ("/effort", "[level]", "choose the reasoning effort (ctrl+t cycles)"),
    ("/agent", "[name]", "switch agent: build or plan (tab cycles)"),
    ("/new", "", "start a new session"),
    ("/session", "", "resume an earlier session"),
    ("/undo", "", "undo the last turn and its file changes"),
    ("/compact", "", "summarize the conversation to free context"),
    ("/review", "[branch|commit|pr N]", "review a diff with codeit, comments on lines like a PR"),
    ("/paste", "", "attach the image in the clipboard (also ctrl+v / alt+v)"),
    ("/copy", "", "copy codeit's last answer to the clipboard"),
    ("/mcp", "[login|logout|reconnect name]", "MCP servers; log in to one that needs OAuth"),
    ("/rename", "<title>", "rename this session"),
    ("/export", "[path]", "save this session as Markdown"),
    ("/approvals", "[auto|ask]", "ask before edits and commands, or not"),
    ("/login", "[provider]", "log in to a provider (opencode's list)"),
    ("/logout", "<provider>", "remove a login codeit saved"),
    ("/status", "", "logins, MCP servers, skills, session"),
    ("/help", "", "list commands and keys"),
    ("/exit", "", "exit codeit"),
];

/// The built-in commands, then extension panels and prompt commands.
fn command_list(harness: &Harness) -> Vec<Command> {
    let mut commands: Vec<Command> = BUILTIN
        .iter()
        .map(|(n, a, h)| Command {
            name: n.to_string(),
            args: a.to_string(),
            help: h.to_string(),
            long: String::new(),
            ext: None,
        })
        .collect();
    for e in &harness.extensions {
        for c in e.commands().into_iter().filter(|c| c.panel) {
            commands.push(Command {
                name: format!("/{}", c.name),
                args: String::new(),
                help: c.description,
                long: c.help,
                ext: Some(e.clone()),
            });
        }
    }
    for c in &harness.commands() {
        if !commands.iter().any(|b| b.name == format!("/{}", c.name)) {
            commands.push(Command {
                name: format!("/{}", c.name),
                args: String::new(),
                help: c.description.clone(),
                long: String::new(),
                ext: None,
            });
        }
    }
    // Help that extensions give for their prompt commands.
    for e in &harness.extensions {
        for c in e.commands().into_iter().filter(|c| !c.panel) {
            if let Some(cmd) = commands.iter_mut().find(|x| x.name == format!("/{}", c.name)) {
                cmd.long = c.help;
            }
        }
    }
    commands
}

pub struct Turn {
    pub id: u64,
    pub cancel: CancellationToken,
    pub started: Instant,
    pub usage: Usage,
    /// The model's name, the effort and the agent, for the answer's label.
    pub model: String,
    pub effort: Option<String>,
    pub agent: String,
}

pub struct App {
    pub tx: UnboundedSender<AppEvent>,
    pub harness: Arc<Harness>,
    pub session: Arc<Mutex<Session>>,
    pub catalogs: BTreeMap<&'static str, Catalog>,
    pub cwd: String,
    pub model: Option<ModelInfo>,
    pub effort: Option<String>,
    pub agent: String,
    pub approval: Approval,
    saved: State,
    pub items: Vec<Item>,
    pub input: String,
    /// Cursor position in `input`, in bytes (always on a char boundary).
    pub cursor: usize,
    /// The slash command picked with ↑/↓, and the input it was picked for (typing resets it).
    command_pick: Option<(String, usize)>,
    /// Lines scrolled up from the bottom of the history. Drawing adjusts it to keep the
    /// selected action in view.
    pub scroll: Cell<u16>,
    /// Columns of the conversation at the last draw (↑/↓ need its rows).
    pub width: Cell<usize>,
    /// The action (or folded group) picked with ↑/↓; `None` while typing. See `chat::Row`.
    pub selected: Option<String>,
    /// How much of the actions to show (ctrl+o): 0 folded, 1 list, 2 open.
    pub level: usize,
    /// Actions shown in full, and folded groups opened.
    pub open: HashSet<String>,
    pub side: Side,
    pub picker: Option<Picker>,
    pub dialog: Option<Dialog>,
    /// Questions waiting behind the one on screen.
    asks: Vec<Ask>,
    pub turn: Option<Turn>,
    pub(super) next_turn: u64,
    /// Editing one of your earlier messages: sending it first rewinds this many turns.
    pub rewind: Option<usize>,
    last_esc: Option<Instant>,
    /// The review screen, when open.
    pub review: Option<super::review::ReviewView>,
    /// An extension's panel, when open.
    pub panel: Option<super::panel::PanelView>,
    /// Messages typed while a turn runs; sent when it ends.
    pub queued: Vec<String>,
    /// (tokens used, usable) of the latest request.
    pub context: Option<(u64, u64)>,
    pub commands: Vec<Command>,
    /// What the extensions show in the footer, and when it was last asked for.
    pub ext_status: Vec<(String, codeit_harness::extension::Tone)>,
    ext_status_at: Option<Instant>,
    pub tick: usize,
    pub quit: bool,
}

impl App {
    pub fn new(tx: UnboundedSender<AppEvent>, harness: Arc<Harness>, resume: Option<Session>) -> Self {
        let cwd = harness.cwd.display().to_string();
        let catalogs = harness.providers.iter().map(|p| (p.id(), Catalog::Loading)).collect();
        let saved = State::load();
        let session = resume.unwrap_or_else(|| {
            let agent = harness
                .config
                .default_agent
                .clone()
                .filter(|a| harness.primary_agents().iter().any(|x| &x.name == a))
                .unwrap_or_else(|| "build".into());
            let mut s = Session::new(&harness.cwd, &agent, saved.model.clone(), saved.effort.clone());
            s.approval = match harness.config.approval.as_deref() {
                Some("ask") => Approval::Ask,
                Some("auto") => Approval::Auto,
                _ => saved.approval,
            };
            s
        });
        let commands = command_list(&harness);
        let level = saved.actions.min(2);
        let mut app = Self {
            tx,
            agent: session.agent.clone(),
            approval: session.approval,
            effort: session.effort.clone(),
            session: Arc::new(Mutex::new(session)),
            harness,
            catalogs,
            cwd,
            model: None,
            saved,
            items: Vec::new(),
            input: String::new(),
            cursor: 0,
            command_pick: None,
            scroll: Cell::new(0),
            width: Cell::new(80),
            selected: None,
            level,
            open: HashSet::new(),
            side: Side::default(),
            picker: None,
            dialog: None,
            asks: Vec::new(),
            turn: None,
            next_turn: 0,
            review: None,
            panel: None,
            rewind: None,
            last_esc: None,
            queued: Vec::new(),
            context: None,
            commands,
            ext_status: Vec::new(),
            ext_status_at: None,
            tick: 0,
            quit: false,
        };
        for p in app.harness.problems.clone() {
            app.error(format!("config: {p}"));
        }
        app.rebuild();
        app.refresh_models();
        app.refresh_git();
        app
    }

    /// Asks the extensions for their footer, unless it was asked for less than `min_age` ago.
    pub(super) fn refresh_ext_status(&mut self, min_age: Duration) {
        if self.harness.extensions.is_empty() || self.ext_status_at.is_some_and(|t| t.elapsed() < min_age) {
            return;
        }
        self.ext_status_at = Some(Instant::now());
        let (tx, exts) = (self.tx.clone(), self.harness.extensions.clone());
        tokio::spawn(async move {
            let mut all = Vec::new();
            for e in exts {
                all.extend(e.status().await);
            }
            let _ = tx.send(AppEvent::ExtStatus(all));
        });
    }

    /// The branch and how many files changed, in the background.
    pub(super) fn refresh_git(&self) {
        let (tx, cwd) = (self.tx.clone(), self.harness.cwd.clone());
        tokio::task::spawn_blocking(move || {
            let git = |args: &[&str]| {
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(&cwd)
                    .stderr(std::process::Stdio::null())
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            };
            let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"]).map(|b| b.trim().to_string());
            let mut changes = (0, 0);
            for l in git(&["status", "--porcelain"]).unwrap_or_default().lines() {
                if l.starts_with("??") || l.starts_with('A') {
                    changes.0 += 1;
                } else {
                    changes.1 += 1;
                }
            }
            let _ = tx.send(AppEvent::Side(SideUpdate::Git(branch, changes)));
        });
    }

    pub fn busy(&self) -> bool {
        self.turn.is_some()
    }

    pub(super) fn notice(&mut self, text: impl Into<String>) {
        self.items.push(Item::Notice(text.into()));
        self.scroll.set(0);
    }

    pub(super) fn error(&mut self, text: impl Into<String>) {
        self.items.push(Item::Error(text.into()));
        self.scroll.set(0);
    }

    fn refresh_models(&mut self) {
        for p in self.harness.providers.clone() {
            self.catalogs.insert(p.id(), Catalog::Loading);
            let tx = self.tx.clone();
            tokio::spawn(async move {
                // Providers you aren't logged in to simply have no models.
                let result = match p.status().await {
                    AuthStatus::LoggedOut(_) => Ok(Vec::new()),
                    AuthStatus::LoggedIn(_) => p.models().await.map_err(|e| format!("{e:#}")),
                };
                let _ = tx.send(AppEvent::Models(p.id(), result));
            });
        }
    }

    pub fn handle(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Term(Event::Key(k)) if k.kind != KeyEventKind::Release => self.key(k),
            AppEvent::Term(Event::Paste(text)) => self.paste(text),
            AppEvent::Term(Event::Mouse(m)) => self.wheel(m.kind),
            AppEvent::Term(_) => {}
            AppEvent::Tick => {
                self.tick = self.tick.wrapping_add(1);
                self.panel_tick();
                // Git every 5 s.
                if self.tick.is_multiple_of(50) {
                    self.refresh_git();
                }
                // Extensions' footer shortly after start, then every 5 min.
                if self.tick == 15 || self.tick.is_multiple_of(3000) {
                    self.refresh_ext_status(Duration::ZERO);
                }
            }
            AppEvent::Side(SideUpdate::Git(branch, changes)) => {
                self.side.branch = branch;
                self.side.changes = changes;
            }
            AppEvent::Models(id, result) => self.models_loaded(id, result),
            AppEvent::Harness(turn, ev) => {
                if self.turn.as_ref().is_some_and(|t| t.id == turn) {
                    self.harness_event(ev);
                }
            }
            AppEvent::LoginChoices(list) => {
                if let Some(Picker { kind: PickerKind::Login(slot), .. }) = &mut self.picker {
                    *slot = Some(list);
                }
            }
            AppEvent::LoginStarted(name, url, code) => {
                if code.is_empty() {
                    self.notice(format!("Log in to {name} in your browser (opening it now). If it didn't open: {url}"));
                    codeit_harness::util::open_url(&url);
                } else {
                    self.notice(format!("Log in to {name}: open {url} and enter the code  {code}  (waiting...)"));
                }
            }
            AppEvent::LoginDone(Ok(name)) => {
                self.notice(format!("Logged in to {name}. Loading its models..."));
                self.refresh_models();
            }
            AppEvent::LoginDone(Err(e)) => self.error(format!("Login failed: {e}")),
            AppEvent::McpLogin(Ok(name)) => {
                self.notice(format!("Logged in to {name}; its tools are loaded (/status)."))
            }
            AppEvent::McpLogin(Err(e)) => self.error(format!("MCP login failed: {e}")),
            AppEvent::Status(lines) => self.notice(lines.join("\n")),
            AppEvent::Review(id, ev) => self.review_event(id, ev),
            AppEvent::Panel(seq, reply) => self.panel_reply(seq, reply),
            AppEvent::ExtStatus(s) => self.ext_status = s,
        }
    }

    /// The mouse wheel: scrolls whatever is in front.
    fn wheel(&mut self, kind: ratatui::crossterm::event::MouseEventKind) {
        use ratatui::crossterm::event::MouseEventKind::{ScrollDown, ScrollUp};
        let up = match kind {
            ScrollUp => true,
            ScrollDown => false,
            _ => return,
        };
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let arrow = key(if up { KeyCode::Up } else { KeyCode::Down });
        if self.dialog.is_some() {
            return;
        }
        if self.panel.is_some() {
            return self.panel_key(arrow);
        }
        if self.review.is_some() {
            for _ in 0..3 {
                self.review_key(arrow);
            }
            return;
        }
        if self.picker.is_some() {
            return self.picker_key(arrow);
        }
        self.selected = None;
        let s = self.scroll.get();
        self.scroll.set(if up { s.saturating_add(3) } else { s.saturating_sub(3) });
    }

    fn paste(&mut self, text: String) {
        if self.panel_paste(&text) {
            return;
        }
        if let Some(p) = &mut self.picker {
            p.filter.push_str(text.trim());
            p.selected = 0;
            return;
        }
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        // An empty paste is what some terminals send when the clipboard holds only an image.
        if text.is_empty() && self.dialog.is_none() {
            self.paste_image();
            return;
        }
        // A dropped or pasted image file's path attaches it.
        let trimmed = text.trim().trim_matches(['\'', '"']);
        let text = if self.dialog.is_none()
            && !trimmed.contains('\n')
            && codeit_harness::tools::image_type(std::path::Path::new(trimmed)).is_some()
            && std::path::Path::new(trimmed).is_file()
        {
            format!("@{trimmed} ")
        } else {
            text
        };
        match &mut self.dialog {
            Some(Dialog::Permission { feedback: Some(f), .. }) | Some(Dialog::Question { typing: Some(f), .. }) => {
                f.push_str(&text)
            }
            Some(_) => {}
            None => {
                self.input.insert_str(self.cursor, &text);
                self.cursor += text.len();
            }
        }
    }

    fn models_loaded(&mut self, id: &'static str, result: Result<Vec<ModelInfo>, String>) {
        if let Ok(list) = &result {
            self.harness.remember_models(list);
        }
        let catalog = match result {
            Ok(models) => Catalog::Ready(models),
            Err(e) => Catalog::Failed(e),
        };
        self.catalogs.insert(id, catalog);
        // Restore the session's (or the last used) model as soon as its provider has answered.
        let wanted = self.session.lock().unwrap().model.clone().or(self.saved.model.clone());
        if self.model.is_none()
            && let Some(key) = wanted
            && let Some(m) = self.all_models().into_iter().find(|m| m.key() == key)
        {
            self.effort = self.effort.clone().filter(|e| m.efforts.contains(e));
            self.model = Some(m);
        }
    }

    pub fn all_models(&self) -> Vec<ModelInfo> {
        self.catalogs
            .values()
            .filter_map(|c| if let Catalog::Ready(m) = c { Some(m.clone()) } else { None })
            .flatten()
            .collect()
    }

    /// Rows of the open picker matching its filter: (label, detail, current).
    pub fn picker_rows(&self) -> Vec<Row> {
        let Some(p) = &self.picker else { return Vec::new() };
        let words: Vec<String> = p.filter.to_lowercase().split_whitespace().map(String::from).collect();
        let keep = |hay: &str| words.iter().all(|w| hay.to_lowercase().contains(w.as_str()));
        match &p.kind {
            PickerKind::Roles => ModelRole::ALL
                .iter()
                .map(|r| {
                    let model = match self.role_key(*r) {
                        Some(k) => self.model_name(&k),
                        None => "same as code".into(),
                    };
                    (format!("{:<7}{model}", r.name()), r.purpose().to_string(), false)
                })
                .collect(),
            PickerKind::Model(role) => {
                let current = self.role_key(*role);
                let mut rows: Vec<Row> = Vec::new();
                if *role != ModelRole::Code && p.filter.is_empty() {
                    rows.push(("(same as the code model)".into(), String::new(), current.is_none()));
                }
                rows.extend(
                    self.all_models().into_iter().filter(|m| keep(&format!("{} {} {}", m.provider, m.id, m.name))).map(
                        |m| {
                            let ctx = m.context.map(|c| format!("{}k", c / 1000)).unwrap_or_default();
                            let efforts = if m.efforts.is_empty() {
                                String::new()
                            } else {
                                format!("effort: {}", m.efforts.join("/"))
                            };
                            let marked = current.as_deref() == Some(&m.key());
                            let mut note = format!("{} · {ctx}", m.provider);
                            if !efforts.is_empty() {
                                note.push_str(&format!(" · {efforts}"));
                            }
                            (m.name.clone(), note, marked)
                        },
                    ),
                );
                rows
            }
            PickerKind::Session(list) => {
                let current = self.session.lock().unwrap().id.clone();
                list.iter()
                    .filter(|s| keep(&s.title))
                    .map(|s| (s.title.clone(), ago(s.updated), s.id == current))
                    .collect()
            }
            PickerKind::Login(None) => Vec::new(),
            PickerKind::Login(Some(list)) => list
                .iter()
                .filter(|c| keep(&format!("{} {}", c.id, c.name)))
                .map(|c| (c.name.clone(), c.hint.clone(), c.logged_in))
                .collect(),
            PickerKind::LoginMethod(c) => {
                c.methods.iter().map(|m| (m.label().to_string(), String::new(), false)).collect()
            }
            PickerKind::Key(_) | PickerKind::Enterprise => Vec::new(),
            PickerKind::Effort => self
                .effort_options()
                .into_iter()
                .filter(|(_, label, _)| keep(label))
                .map(|(level, label, detail)| (label, detail, level == self.effort))
                .collect(),
        }
    }

    /// The effort levels of the current model, with "default" first: (level, label, detail).
    fn effort_options(&self) -> Vec<(Option<String>, String, String)> {
        let Some(m) = &self.model else { return Vec::new() };
        let default = match &m.default_effort {
            Some(e) => format!("the model's own ({e})"),
            None => "the model's own".into(),
        };
        let mut out = vec![(None, "default".to_string(), default)];
        out.extend(m.efforts.iter().map(|e| (Some(e.clone()), e.clone(), String::new())));
        out
    }

    /// Opens the effort popup on the current level.
    fn open_effort(&mut self) {
        let Some(m) = &self.model else {
            self.notice("No model yet: pick one with /models.");
            return;
        };
        if m.efforts.is_empty() {
            self.notice(format!("{} has no reasoning effort levels.", m.name));
            return;
        }
        let selected = self.effort_options().iter().position(|o| o.0 == self.effort).unwrap_or(0);
        self.picker = Some(Picker { kind: PickerKind::Effort, filter: String::new(), selected });
    }

    fn select_model(&mut self, m: ModelInfo) {
        if !self.effort.as_ref().is_some_and(|e| m.efforts.contains(e)) {
            self.effort = None;
        }
        self.saved.model = Some(m.key());
        self.saved.effort = self.effort.clone();
        self.saved.save();
        self.model = Some(m);
    }

    /// The model set for a role (`None`: the code model is used).
    pub fn role_key(&self, role: ModelRole) -> Option<String> {
        match role {
            ModelRole::Code => self.model.as_ref().map(|m| m.key()),
            ModelRole::Think => self.saved.think.clone(),
        }
    }

    /// A model's display name, or its key if its provider hasn't answered yet.
    pub fn model_name(&self, key: &str) -> String {
        self.all_models().into_iter().find(|m| m.key() == key).map(|m| m.name).unwrap_or_else(|| key.to_string())
    }

    fn set_role(&mut self, role: ModelRole, key: Option<String>) {
        match role {
            ModelRole::Code => return,
            ModelRole::Think => self.saved.think = key.clone(),
        }
        self.saved.save();
        let what = key.map(|k| self.model_name(&k)).unwrap_or_else(|| "the code model".into());
        self.notice(format!("{} model: {what}", role.name()));
    }

    /// The model a turn of `agent` runs on: the think model for plan, else the code model.
    pub(super) fn model_for(&self, agent: &str) -> Option<ModelInfo> {
        let think = (agent == "plan").then(|| self.saved.think.clone()).flatten();
        think.and_then(|k| self.all_models().into_iter().find(|m| m.key() == k)).or_else(|| self.model.clone())
    }

    fn set_effort(&mut self, effort: Option<String>) {
        self.effort = effort;
        self.saved.effort = self.effort.clone();
        self.saved.save();
    }

    fn cycle_effort(&mut self) {
        let Some(m) = &self.model else { return };
        if m.efforts.is_empty() {
            self.notice(format!("{} has no reasoning effort levels.", m.name));
            return;
        }
        let next = match &self.effort {
            None => Some(m.efforts[0].clone()),
            Some(e) => m.efforts.iter().position(|x| x == e).and_then(|i| m.efforts.get(i + 1)).cloned(),
        };
        self.set_effort(next);
    }

    fn cycle_agent(&mut self) {
        let names: Vec<String> = self.harness.primary_agents().iter().map(|a| a.name.clone()).collect();
        if names.is_empty() {
            return;
        }
        let i = names.iter().position(|n| *n == self.agent).map(|i| (i + 1) % names.len()).unwrap_or(0);
        self.agent = names[i].clone();
    }

    // ── keys ────────────────────────────────────────────────────────────────

    fn key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if self.panel.is_some() && self.dialog.is_none() {
            return self.panel_key(k);
        }
        if self.review.is_some() && self.dialog.is_none() {
            return self.review_key(k);
        }
        if self.dialog.is_some() {
            if ctrl && k.code == KeyCode::Char('c') {
                self.interrupt();
                return;
            }
            return self.dialog_key(k);
        }
        if self.picker.is_some() {
            return self.picker_key(k);
        }
        if (self.selected.is_some() || self.input.is_empty()) && self.nav_key(k) {
            return;
        }
        match k.code {
            KeyCode::Char('c') if ctrl => {
                if self.busy() {
                    self.interrupt();
                } else if !self.input.is_empty() {
                    self.input.clear();
                    self.cursor = 0;
                } else {
                    self.quit = true;
                }
            }
            KeyCode::Char('d') if ctrl && self.input.is_empty() => self.quit = true,
            KeyCode::Char('t') if ctrl => self.cycle_effort(),
            KeyCode::Char('v') if ctrl || k.modifiers.contains(KeyModifiers::ALT) => self.paste_image(),
            KeyCode::Char('o') if ctrl => {
                self.level = (self.level + 1) % 3;
                self.open.clear();
                self.saved.actions = self.level;
                self.saved.save();
            }
            KeyCode::Char('j') if ctrl => self.insert('\n'),
            KeyCode::Char('u') if ctrl => {
                self.input.drain(..self.cursor);
                self.cursor = 0;
            }
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.input.len(),
            KeyCode::Esc => {
                if self.busy() {
                    self.interrupt();
                } else if self.rewind.take().is_some() {
                    self.input.clear();
                    self.cursor = 0;
                } else if self.input.is_empty() {
                    // Esc Esc: edit your last message.
                    if self.last_esc.is_some_and(|t| t.elapsed() < Duration::from_millis(800)) {
                        self.last_esc = None;
                        if let Some(i) = self.items.iter().rposition(|i| matches!(i, Item::User(_))) {
                            self.edit_message(i);
                        }
                    } else {
                        self.last_esc = Some(Instant::now());
                    }
                }
            }
            KeyCode::Enter if k.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) => self.insert('\n'),
            KeyCode::Enter => self.submit(),
            KeyCode::Tab | KeyCode::BackTab => {
                if self.command_matches().is_empty() {
                    self.cycle_agent();
                } else {
                    self.complete_command();
                }
            }
            KeyCode::Backspace => {
                if let Some(c) = self.input[..self.cursor].chars().next_back() {
                    self.cursor -= c.len_utf8();
                    self.input.remove(self.cursor);
                }
            }
            KeyCode::Delete => {
                if self.cursor < self.input.len() {
                    self.input.remove(self.cursor);
                }
            }
            KeyCode::Left => {
                if let Some(c) = self.input[..self.cursor].chars().next_back() {
                    self.cursor -= c.len_utf8();
                }
            }
            KeyCode::Right => {
                if let Some(c) = self.input[self.cursor..].chars().next() {
                    self.cursor += c.len_utf8();
                }
            }
            KeyCode::Up | KeyCode::Down if !self.command_matches().is_empty() => {
                let n = self.command_matches().len();
                let at = self.command_selected();
                let at = if k.code == KeyCode::Up { (at + n - 1) % n } else { (at + 1) % n };
                self.command_pick = Some((self.input.clone(), at));
            }
            KeyCode::Up if self.input.is_empty() && !self.queued.is_empty() => {
                // Take back the last queued message to edit it.
                self.input = self.queued.pop().unwrap_or_default();
                self.cursor = self.input.len();
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.input.len(),
            KeyCode::PageUp => self.scroll.set(self.scroll.get().saturating_add(10)),
            KeyCode::PageDown => self.scroll.set(self.scroll.get().saturating_sub(10)),
            KeyCode::Char(c) if !ctrl => self.insert(c),
            _ => {}
        }
    }

    /// ↑/↓ through the actions while the composer is empty; Enter opens or closes the selected
    /// one. Returns false for keys it leaves to the composer (typing anything leaves the history).
    fn nav_key(&mut self, k: KeyEvent) -> bool {
        let stops = super::chat::selectable(&super::chat::rows(self, self.width.get()));
        let (Some(first), Some(last)) = (stops.first().cloned(), stops.last().cloned()) else { return false };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let at = self.selected.as_ref().and_then(|s| stops.iter().position(|x| x == s));
        match (k.code, at) {
            (KeyCode::Up, None) if self.queued.is_empty() && self.selected.is_none() => self.selected = Some(last),
            (KeyCode::Up, Some(i)) => self.selected = Some(stops[i.saturating_sub(1)].clone()),
            (KeyCode::Down, Some(i)) => self.selected = stops.get(i + 1).cloned(),
            (KeyCode::Home, Some(_)) => self.selected = Some(first),
            (KeyCode::End, Some(_)) => self.selected = Some(last),
            (KeyCode::Enter | KeyCode::Char(' '), Some(i)) => {
                let key = stops[i].clone();
                if !self.open.remove(&key) {
                    self.open.insert(key);
                }
            }
            (KeyCode::Esc, _) if self.selected.is_some() => self.selected = None,
            (_, Some(_)) if !ctrl => {
                self.selected = None;
                return false;
            }
            // The selected action is gone (folded away): start over.
            (KeyCode::Up | KeyCode::Down, None) if self.selected.is_some() => self.selected = Some(last),
            _ => return false,
        }
        true
    }

    /// Saves the clipboard's image and attaches it to the message being written.
    fn paste_image(&mut self) {
        match super::clipboard::save_image() {
            Some(path) => {
                let at = format!("@{} ", path.display());
                self.input.insert_str(self.cursor, &at);
                self.cursor += at.len();
            }
            None => self.notice("No image in the clipboard (copy a screenshot first, then ctrl+v, alt+v or /paste)."),
        }
    }

    /// Copies the text of codeit's last answer.
    fn copy_last(&mut self) {
        let text = {
            let s = self.session.lock().unwrap();
            s.entries
                .iter()
                .rev()
                .filter(|e| e.prompt.is_none() && e.message.role == Role::Assistant)
                .map(|e| {
                    let parts = e.message.parts.iter();
                    parts.filter_map(|p| if let Part::Text { text } = p { Some(text.as_str()) } else { None }).collect()
                })
                .find(|t: &String| !t.trim().is_empty())
        };
        match text {
            Some(t) => {
                super::clipboard::copy_text(t.trim());
                self.notice("Copied the last answer.");
            }
            None => self.notice("Nothing to copy yet."),
        }
    }

    /// Puts one of your messages back in the input; sending it rewinds the conversation (and
    /// the files) to before it, like /undo for each later turn.
    fn edit_message(&mut self, item: usize) {
        let Some(Item::User(text)) = self.items.get(item) else { return };
        self.input = text.clone();
        self.cursor = self.input.len();
        self.selected = None;
        if !self.busy() {
            let turns = self.items[item..].iter().filter(|i| matches!(i, Item::User(_))).count();
            self.rewind = Some(turns);
        }
    }

    fn insert(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Slash commands matching what is typed so far (only while typing the command name).
    pub fn command_matches(&self) -> Vec<&Command> {
        if !self.input.starts_with('/') || self.input.contains(char::is_whitespace) {
            return Vec::new();
        }
        self.commands.iter().filter(|c| c.name.starts_with(self.input.as_str())).collect()
    }

    /// The highlighted slash command: the one picked with ↑/↓, else the exact match, else the first.
    pub fn command_selected(&self) -> usize {
        let matches = self.command_matches();
        match &self.command_pick {
            Some((input, at)) if *input == self.input && *at < matches.len() => *at,
            _ => matches.iter().position(|c| c.name == self.input).unwrap_or(0),
        }
    }

    fn complete_command(&mut self) {
        if let Some(c) = self.command_matches().get(self.command_selected()) {
            self.input = format!("{} ", c.name);
            self.cursor = self.input.len();
        }
    }

    fn picker_key(&mut self, k: KeyEvent) {
        let count = self.picker_rows().len();
        let Some(p) = &mut self.picker else { return };
        // Typing a key or a domain: no list to move through.
        if matches!(p.kind, PickerKind::Key(_) | PickerKind::Enterprise) {
            match k.code {
                KeyCode::Esc => self.picker = None,
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => self.picker = None,
                KeyCode::Backspace => {
                    p.filter.pop();
                }
                KeyCode::Char(c) => p.filter.push(c),
                KeyCode::Enter => {
                    let picker = self.picker.take().expect("picker is open");
                    let text = picker.filter.trim().to_string();
                    match picker.kind {
                        PickerKind::Key(c) => match crate::login::save_key(&c.id, &text) {
                            Ok(()) => {
                                self.notice(format!("Saved the {} key. Its models are in /models.", c.name));
                                self.refresh_models();
                            }
                            Err(e) => self.error(format!("{e:#}")),
                        },
                        _ => self.start_login(
                            crate::login::Method::CopilotEnterprise,
                            "GitHub Copilot".into(),
                            Some(text),
                        ),
                    }
                }
                _ => {}
            }
            return;
        }
        match k.code {
            KeyCode::Esc => self.picker = None,
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => self.picker = None,
            KeyCode::Up => p.selected = p.selected.saturating_sub(1),
            KeyCode::Down => p.selected = (p.selected + 1).min(count.saturating_sub(1)),
            KeyCode::PageUp => p.selected = p.selected.saturating_sub(10),
            KeyCode::PageDown => p.selected = (p.selected + 10).min(count.saturating_sub(1)),
            KeyCode::Backspace => {
                p.filter.pop();
                p.selected = 0;
            }
            KeyCode::Char(c) => {
                p.filter.push(c);
                p.selected = 0;
            }
            KeyCode::Enter => {
                let selected = p.selected;
                let words: Vec<String> = p.filter.to_lowercase().split_whitespace().map(String::from).collect();
                let picker = self.picker.take().expect("picker is open");
                match picker.kind {
                    PickerKind::Roles => {
                        if let Some(role) = ModelRole::ALL.get(selected) {
                            self.picker = Some(picker_with(PickerKind::Model(*role)));
                        }
                    }
                    PickerKind::Model(role) => {
                        let clear_row = role != ModelRole::Code && words.is_empty();
                        if clear_row && selected == 0 {
                            self.set_role(role, None);
                            return;
                        }
                        let selected = selected - clear_row as usize;
                        let models: Vec<ModelInfo> = self
                            .all_models()
                            .into_iter()
                            .filter(|m| {
                                let hay = format!("{} {} {}", m.provider, m.id, m.name).to_lowercase();
                                words.iter().all(|w| hay.contains(w.as_str()))
                            })
                            .collect();
                        match models.into_iter().nth(selected) {
                            Some(m) if role == ModelRole::Code => self.select_model(m),
                            Some(m) => self.set_role(role, Some(m.key())),
                            None => self.picker = Some(picker_with(PickerKind::Model(role))),
                        }
                    }
                    PickerKind::Session(list) => {
                        let found = list
                            .iter()
                            .filter(|s| words.iter().all(|w| s.title.to_lowercase().contains(w.as_str())))
                            .nth(selected)
                            .cloned();
                        if let Some(meta) = found {
                            self.resume(&meta.id);
                        }
                    }
                    PickerKind::Login(Some(list)) => {
                        let found = list
                            .into_iter()
                            .filter(|c| {
                                let hay = format!("{} {}", c.id, c.name).to_lowercase();
                                words.iter().all(|w| hay.contains(w.as_str()))
                            })
                            .nth(selected);
                        if let Some(c) = found {
                            self.login_with(c);
                        }
                    }
                    PickerKind::Login(None) => self.picker = Some(picker),
                    PickerKind::LoginMethod(c) => {
                        if let Some(m) = c.methods.get(selected).copied() {
                            self.login_method(c, m);
                        }
                    }
                    PickerKind::Key(_) | PickerKind::Enterprise => {}
                    PickerKind::Effort => {
                        let found = self
                            .effort_options()
                            .into_iter()
                            .filter(|(_, label, _)| words.iter().all(|w| label.contains(w.as_str())))
                            .nth(selected);
                        if let Some((level, _, _)) = found {
                            self.set_effort(level);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Opens the /login popup; the providers' login state loads in the background.
    fn open_login(&mut self, filter: &str) {
        self.picker = Some(Picker { kind: PickerKind::Login(None), filter: filter.to_string(), selected: 0 });
        let (tx, providers) = (self.tx.clone(), self.harness.providers.clone());
        tokio::spawn(async move {
            let _ = tx.send(AppEvent::LoginChoices(crate::login::choices(&providers).await));
        });
    }

    /// The provider is chosen: ask how, or go straight to its only way.
    fn login_with(&mut self, c: crate::login::Choice) {
        match c.methods.as_slice() {
            [m] => {
                let m = *m;
                self.login_method(c, m)
            }
            _ => self.picker = Some(picker_with(PickerKind::LoginMethod(c))),
        }
    }

    fn login_method(&mut self, c: crate::login::Choice, m: crate::login::Method) {
        use crate::login::Method;
        match m {
            Method::ApiKey => {
                let mut hint = Vec::new();
                if let Some(doc) = &c.doc {
                    hint.push(format!("Keys: {doc}"));
                }
                if !c.env.is_empty() {
                    hint.push(format!("{} works too", c.env.join(" or ")));
                }
                if !hint.is_empty() {
                    self.notice(hint.join(" · "));
                }
                self.picker = Some(picker_with(PickerKind::Key(c)));
            }
            Method::CopilotEnterprise => self.picker = Some(picker_with(PickerKind::Enterprise)),
            Method::Own => {
                self.picker = None;
                self.notice(format!("{}: {}", c.name, c.how));
            }
            _ => self.start_login(m, c.name, None),
        }
    }

    /// Runs a browser or device login in the background.
    fn start_login(&mut self, m: crate::login::Method, name: String, enterprise: Option<String>) {
        let tx = self.tx.clone();
        self.notice(format!("Starting the {name} login..."));
        tokio::spawn(async move {
            let r = async {
                let started = crate::login::start(m, enterprise.as_deref()).await.map_err(|e| format!("{e:#}"))?;
                let _ = tx.send(AppEvent::LoginStarted(name.clone(), started.url.clone(), started.code.clone()));
                started.wait.await.map_err(|e| format!("{e:#}"))?;
                Ok(name)
            }
            .await;
            let _ = tx.send(AppEvent::LoginDone(r));
        });
    }

    fn dialog_key(&mut self, k: KeyEvent) {
        let Some(dialog) = self.dialog.take() else { return };
        match dialog {
            Dialog::Permission { ask, mut selected, feedback } => {
                if let Some(mut text) = feedback {
                    match k.code {
                        KeyCode::Enter => {
                            let msg = (!text.trim().is_empty()).then(|| text.trim().to_string());
                            return self.answer(ask, Reply::Reject(msg));
                        }
                        KeyCode::Esc => return self.answer(ask, Reply::Reject(None)),
                        KeyCode::Backspace => {
                            text.pop();
                        }
                        KeyCode::Char(c) => text.push(c),
                        _ => {}
                    }
                    self.dialog = Some(Dialog::Permission { ask, selected, feedback: Some(text) });
                    return;
                }
                match k.code {
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => selected = (selected + 1).min(2),
                    KeyCode::Char('y') | KeyCode::Char('1') => return self.answer(ask, Reply::Once),
                    KeyCode::Char('a') | KeyCode::Char('2') => return self.answer(ask, Reply::Always),
                    KeyCode::Char('n') | KeyCode::Char('3') => selected = 3,
                    KeyCode::Esc => return self.answer(ask, Reply::Reject(None)),
                    KeyCode::Enter => match selected {
                        0 => return self.answer(ask, Reply::Once),
                        1 => return self.answer(ask, Reply::Always),
                        _ => selected = 3,
                    },
                    _ => {}
                }
                // "No" asks what to do instead (Enter with nothing just says no).
                let feedback = (selected == 3).then(String::new);
                let selected = selected.min(2);
                self.dialog = Some(Dialog::Permission { ask, selected, feedback });
            }
            Dialog::Question { questions, reply, mut index, mut selected, mut chosen, mut answers, typing } => {
                let q = &questions[index];
                let custom = q.options.len();
                if let Some(mut text) = typing {
                    match k.code {
                        KeyCode::Enter => {
                            answers.push(vec![text.trim().to_string()]);
                            index += 1;
                            selected = 0;
                            chosen.clear();
                        }
                        KeyCode::Esc => {}
                        KeyCode::Backspace => {
                            text.pop();
                            self.dialog = Some(Dialog::Question {
                                questions,
                                reply,
                                index,
                                selected,
                                chosen,
                                answers,
                                typing: Some(text),
                            });
                            return;
                        }
                        KeyCode::Char(c) => {
                            text.push(c);
                            self.dialog = Some(Dialog::Question {
                                questions,
                                reply,
                                index,
                                selected,
                                chosen,
                                answers,
                                typing: Some(text),
                            });
                            return;
                        }
                        _ => {
                            self.dialog = Some(Dialog::Question {
                                questions,
                                reply,
                                index,
                                selected,
                                chosen,
                                answers,
                                typing: Some(text),
                            });
                            return;
                        }
                    }
                } else {
                    match k.code {
                        KeyCode::Esc => {
                            let _ = reply.send(None);
                            return self.next_ask();
                        }
                        KeyCode::Up => selected = selected.saturating_sub(1),
                        KeyCode::Down => selected = (selected + 1).min(custom),
                        KeyCode::Char(' ') if q.multiple && selected < custom => {
                            if let Some(p) = chosen.iter().position(|c| *c == selected) {
                                chosen.remove(p);
                            } else {
                                chosen.push(selected);
                            }
                        }
                        KeyCode::Enter if selected == custom => {
                            self.dialog = Some(Dialog::Question {
                                questions,
                                reply,
                                index,
                                selected,
                                chosen,
                                answers,
                                typing: Some(String::new()),
                            });
                            return;
                        }
                        KeyCode::Enter => {
                            let picks = if q.multiple && !chosen.is_empty() { chosen.clone() } else { vec![selected] };
                            answers.push(picks.iter().map(|i| q.options[*i].0.clone()).collect());
                            index += 1;
                            selected = 0;
                            chosen.clear();
                        }
                        KeyCode::Char(c) if c.is_ascii_digit() => {
                            let n = c.to_digit(10).unwrap_or(0) as usize;
                            if n >= 1 && n <= custom {
                                selected = n - 1;
                            }
                        }
                        _ => {}
                    }
                }
                if index >= questions.len() {
                    let _ = reply.send(Some(answers));
                    return self.next_ask();
                }
                self.dialog =
                    Some(Dialog::Question { questions, reply, index, selected, chosen, answers, typing: None });
            }
        }
    }

    fn answer(&mut self, ask: PermissionAsk, reply: Reply) {
        let _ = ask.reply.send(reply);
        self.next_ask();
    }

    fn next_ask(&mut self) {
        self.dialog = None;
        if !self.asks.is_empty() {
            let a = self.asks.remove(0);
            self.show_ask(a);
        }
    }

    pub(super) fn show_ask(&mut self, ask: Ask) {
        if self.dialog.is_some() {
            self.asks.push(ask);
            return;
        }
        self.dialog = Some(match ask {
            Ask::Permission(ask) => Dialog::Permission { ask, selected: 0, feedback: None },
            Ask::Question(QuestionAsk { questions, reply }) => Dialog::Question {
                questions,
                reply,
                index: 0,
                selected: 0,
                chosen: Vec::new(),
                answers: Vec::new(),
                typing: None,
            },
        });
    }

    // ── turns ───────────────────────────────────────────────────────────────

    fn submit(&mut self) {
        // Enter on a partly typed command runs the highlighted one.
        if let Some(c) = self.command_matches().get(self.command_selected()) {
            self.input = c.name.clone();
        }
        self.command_pick = None;
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.input.clear();
        self.cursor = 0;
        if text.starts_with('/') && self.command(&text) {
            return;
        }
        if let Some(turns) = self.rewind.take()
            && !self.busy()
        {
            let mut restored = 0;
            for _ in 0..turns {
                match self.session.lock().unwrap().undo() {
                    Some(u) => restored += u.restored.len(),
                    None => break,
                }
            }
            let _ = self.session.lock().unwrap().save();
            self.rebuild();
            self.notice(format!(
                "Rewound {turns} turn{}{}.",
                if turns == 1 { "" } else { "s" },
                if restored > 0 { format!(" and restored {restored} file changes") } else { String::new() }
            ));
        }
        if self.busy() {
            self.queued.push(text);
            return;
        }
        self.send(text);
    }

    /// Sends `text` now, or queues it behind the running turn.
    pub(super) fn submit_text(&mut self, text: String) {
        if self.busy() {
            self.queued.push(text);
        } else {
            self.send(text);
        }
    }

    /// Sends a prompt or a prompt command.
    fn send(&mut self, text: String) {
        if self.model.is_none() {
            self.input = text;
            self.cursor = self.input.len();
            self.notice("Pick a model first.");
            self.picker = Some(picker_with(PickerKind::Model(ModelRole::Code)));
            return;
        }
        let input = match text.strip_prefix('/').and_then(|t| {
            let (name, args) = t.split_once(char::is_whitespace).unwrap_or((t, ""));
            self.harness.commands().iter().any(|c| c.name == name).then(|| (name.to_string(), args.trim().to_string()))
        }) {
            Some((name, args)) => Input::Command { name, args },
            None => Input::Prompt(text.clone()),
        };
        self.items.push(Item::User(text));
        self.start(input);
    }

    fn start(&mut self, input: Input) {
        let Some(model) = self.model_for(&self.agent) else { return };
        let effort = self.effort.clone().filter(|e| model.efforts.contains(e));
        {
            let mut s = self.session.lock().unwrap();
            s.model = Some(model.key());
            s.effort = effort.clone();
            s.agent = self.agent.clone();
            s.approval = self.approval;
        }
        self.next_turn += 1;
        let id = self.next_turn;
        let cancel = CancellationToken::new();
        let (htx, mut hrx) = tokio::sync::mpsc::unbounded_channel();
        let run = Run {
            harness: self.harness.clone(),
            session: self.session.clone(),
            events: htx,
            cancel: cancel.clone(),
            depth: 0,
        };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let forward = tokio::spawn(async move {
                while let Some(ev) = hrx.recv().await {
                    let _ = tx.send(AppEvent::Harness(id, ev));
                }
            });
            run.start(input).await;
            drop(run);
            let _ = forward.await;
        });
        self.turn = Some(Turn {
            id,
            cancel,
            started: Instant::now(),
            usage: Usage::default(),
            model: model.name.clone(),
            effort,
            agent: self.agent.clone(),
        });
        self.scroll.set(0);
    }

    fn harness_event(&mut self, ev: HEvent) {
        match ev {
            HEvent::Text(t) => match self.items.last_mut() {
                Some(Item::Assistant { text, done: false, .. }) => text.push_str(&t),
                _ => self.items.push(Item::Assistant { text: t, reasoning: String::new(), done: false }),
            },
            HEvent::Reasoning(t) => match self.items.last_mut() {
                Some(Item::Assistant { reasoning, done: false, .. }) => reasoning.push_str(&t),
                _ => self.items.push(Item::Assistant { text: String::new(), reasoning: t, done: false }),
            },
            HEvent::Reset => {
                while matches!(
                    self.items.last(),
                    Some(Item::Assistant { done: false, .. })
                        | Some(Item::Tool(ToolItem { state: ToolState::Pending, .. }))
                ) {
                    self.items.pop();
                }
            }
            HEvent::ToolPending { id, name } => {
                self.close_text();
                self.items.push(Item::Tool(ToolItem::new(id, name, String::new(), ToolState::Pending)));
            }
            HEvent::ToolStart { id, name, title, .. } => {
                self.close_text();
                match self.tool_mut(&id) {
                    Some(t) => {
                        t.title = title;
                        t.state = ToolState::Running;
                        t.started = Some(Instant::now());
                    }
                    None => self.items.push(Item::Tool(ToolItem::new(id, name, title, ToolState::Running))),
                }
            }
            HEvent::ToolProgress { id, text } => {
                if let Some(t) = self.tool_mut(&id) {
                    t.progress.push(text);
                    if t.progress.len() > 3 {
                        t.progress.remove(0);
                    }
                }
            }
            HEvent::ToolDone { id, name, title, output, error, diff, lines, digest } => {
                let state = if error { ToolState::Failed } else { ToolState::Done };
                let t = match self.tool_mut(&id) {
                    Some(t) => t,
                    None => {
                        self.items.push(Item::Tool(ToolItem::new(id.clone(), name, String::new(), state)));
                        self.tool_mut(&id).expect("just added")
                    }
                };
                if !title.is_empty() {
                    t.title = title;
                }
                t.state = state;
                t.output = output;
                t.diff = diff;
                t.lines = lines;
                t.digest = digest;
                t.elapsed = t.started.map(|s| s.elapsed());
            }
            HEvent::Todos(todos) => self.items.push(Item::Todos(todos)),
            HEvent::Ask(a) => self.show_ask(a),
            HEvent::Step { usage, context } => {
                if let Some(t) = &mut self.turn {
                    t.usage.input_tokens = usage.input_tokens;
                    t.usage.cached_tokens = usage.cached_tokens;
                    t.usage.output_tokens += usage.output_tokens;
                    if let Some(c) = usage.credits {
                        t.usage.credits = Some(t.usage.credits.unwrap_or(0.0) + c);
                    }
                }
                if context.is_some() {
                    self.context = context;
                }
            }
            HEvent::Notice(n) => {
                self.close_text();
                self.notice(n);
            }
            HEvent::Error(e) => {
                self.close_text();
                self.error(e);
            }
            HEvent::Title(_) => {}
            HEvent::Done => self.turn_done(),
        }
    }

    fn close_text(&mut self) {
        if let Some(Item::Assistant { done, .. }) = self.items.last_mut() {
            *done = true;
        }
    }

    fn tool_mut(&mut self, id: &str) -> Option<&mut ToolItem> {
        self.items.iter_mut().rev().find_map(|i| match i {
            Item::Tool(t) if t.id == id => Some(t),
            _ => None,
        })
    }

    fn turn_done(&mut self) {
        self.close_text();
        self.refresh_ext_status(Duration::from_secs(60));
        let Some(t) = self.turn.take() else { return };
        // Calls that never got a result (interrupted) stop spinning.
        for i in &mut self.items {
            if let Item::Tool(tool) = i
                && tool.running()
            {
                tool.state = ToolState::Failed;
                tool.output = "Interrupted.".into();
            }
        }
        // Questions from this turn can't be answered anymore.
        self.dialog = None;
        self.asks.clear();
        self.items.push(Item::TurnEnd(TurnInfo {
            agent: t.agent.clone(),
            model: t.model.clone(),
            effort: t.effort.clone(),
            secs: Some(t.started.elapsed().as_secs()),
        }));
        self.refresh_git();
        // Picks up commands and skills the turn wrote.
        self.harness.reload();
        self.commands = command_list(&self.harness);
        if !self.queued.is_empty() {
            let next = self.queued.remove(0);
            self.send(next);
        }
    }

    fn interrupt(&mut self) {
        if let Some(t) = &self.turn {
            t.cancel.cancel();
            self.queued.clear();
        }
        if let Some(Dialog::Permission { ask, .. }) = self.dialog.take() {
            let _ = ask.reply.send(Reply::Reject(None));
        }
    }

    /// Rebuilds the history from the session (after resume or undo).
    fn rebuild(&mut self) {
        let s = self.session.lock().unwrap();
        let mut items = Vec::new();
        if s.summary.is_some() {
            items.push(Item::Notice("(The conversation before this point was summarized.)".into()));
        }
        let tools = self.harness.tools(
            s.model.as_deref().and_then(|m| m.split_once('/')).map(|m| m.1).unwrap_or(""),
            &Default::default(),
            0,
        );
        let results: std::collections::HashMap<&str, (&str, bool)> = s
            .entries
            .iter()
            .flat_map(|e| e.message.parts.iter())
            .filter_map(|p| match p {
                Part::ToolResult { id, content, error } => Some((id.as_str(), (content.as_str(), *error))),
                _ => None,
            })
            .collect();
        // A turn's label: its agent, the model of its last reply, and how long it took.
        let mut turn: Option<(u64, String)> = None;
        let mut last: Option<(u64, Option<String>)> = None;
        let name = |key: &str| {
            let id = key.split_once('/').map(|k| k.1).unwrap_or(key);
            self.all_models().into_iter().find(|m| m.key() == key).map(|m| m.name).unwrap_or_else(|| id.to_string())
        };
        let close =
            |items: &mut Vec<Item>, turn: &mut Option<(u64, String)>, last: &mut Option<(u64, Option<String>)>| {
                if let (Some((start, agent)), Some((end, model))) = (turn.take(), last.take()) {
                    items.push(Item::TurnEnd(TurnInfo {
                        agent,
                        model: model.as_deref().map(name).unwrap_or_default(),
                        effort: None,
                        secs: (end > start).then(|| end - start),
                    }));
                }
            };
        for e in s.entries.iter().skip(s.context_start.min(s.entries.len())) {
            if let Some(p) = &e.prompt {
                close(&mut items, &mut turn, &mut last);
                items.push(Item::User(p.clone()));
                turn = Some((e.time, e.agent.clone().unwrap_or_else(|| s.agent.clone())));
                continue;
            }
            if e.message.role == Role::Assistant {
                last = Some((e.time, e.message.model.clone()));
            }
            if e.message.role != Role::Assistant {
                continue;
            }
            for p in &e.message.parts {
                match p {
                    Part::Text { text } if !text.trim().is_empty() => {
                        items.push(Item::Assistant { text: text.clone(), reasoning: String::new(), done: true })
                    }
                    Part::ToolCall(c) => {
                        let title = tools
                            .iter()
                            .find(|t| t.name() == c.name)
                            .map(|t| t.title(&c.input, &self.harness.cwd))
                            .unwrap_or_else(|| c.input.to_string().chars().take(80).collect());
                        let (output, error) = results.get(c.id.as_str()).copied().unwrap_or(("", true));
                        let state = if error { ToolState::Failed } else { ToolState::Done };
                        let mut t = ToolItem::new(c.id.clone(), c.name.clone(), title, state);
                        t.output = output.lines().take(40).collect::<Vec<_>>().join("\n");
                        items.push(Item::Tool(t));
                    }
                    _ => {}
                }
            }
        }
        close(&mut items, &mut turn, &mut last);
        let todos = s.todos.clone();
        drop(s);
        self.items = items;
        self.selected = None;
        self.open.clear();
        if !todos.is_empty() {
            self.items.push(Item::Todos(todos));
        }
        self.scroll.set(0);
    }

    fn resume(&mut self, id: &str) {
        match Session::load(id) {
            Ok(s) => {
                self.agent = s.agent.clone();
                self.approval = s.approval;
                self.effort = s.effort.clone();
                let key = s.model.clone();
                let title = s.title.clone();
                self.session = Arc::new(Mutex::new(s));
                if let Some(m) = key.and_then(|k| self.all_models().into_iter().find(|m| m.key() == k)) {
                    self.model = Some(m);
                }
                self.context = None;
                self.rebuild();
                self.notice(format!("Resumed: {title}"));
            }
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    /// Switches to the saved session of this folder whose title starts with `prefix`, or starts
    /// one titled `title` (a task's session).
    pub(super) fn open_session_titled(&mut self, prefix: &str, title: &str) {
        let list = Session::list(Some(&self.harness.cwd));
        if let Some(m) = list.iter().find(|m| m.title.starts_with(prefix)) {
            if self.session.lock().unwrap().id != m.id {
                self.resume(&m.id.clone());
            }
            return;
        }
        self.new_session();
        self.session.lock().unwrap().title = title.to_string();
        self.notice(format!("New session: {title}"));
    }

    fn new_session(&mut self) {
        let mut s =
            Session::new(&self.harness.cwd, &self.agent, self.model.as_ref().map(|m| m.key()), self.effort.clone());
        s.approval = self.approval;
        self.session = Arc::new(Mutex::new(s));
        self.items.clear();
        self.selected = None;
        self.open.clear();
        self.context = None;
        self.scroll.set(0);
    }

    /// Handles a built-in command; false if it is a prompt command for the model.
    fn command(&mut self, text: &str) -> bool {
        let mut parts = text.split_whitespace();
        let name = parts.next().unwrap_or_default();
        let args: Vec<&str> = parts.collect();
        let idle = !self.busy();
        match (name, args.as_slice()) {
            ("/models", _) => self.picker = Some(picker_with(PickerKind::Roles)),
            ("/review", rest) => self.open_review(&rest.join(" ")),
            ("/paste", _) => self.paste_image(),
            ("/copy", _) => self.copy_last(),
            ("/rename", rest) if !rest.is_empty() => {
                let title = rest.join(" ");
                let mut s = self.session.lock().unwrap();
                s.title = title.clone();
                let _ = s.save();
                drop(s);
                self.notice(format!("Renamed the session: {title}"));
            }
            ("/rename", _) => self.notice("Usage: /rename <new title>"),
            ("/export", rest) => {
                let s = self.session.lock().unwrap().clone();
                let path = match rest.first() {
                    Some(p) => codeit_harness::util::resolve(&self.harness.cwd, p),
                    None => codeit_providers::data_dir().join("exports").join(format!("{}.md", &s.id[..8])),
                };
                let result = path
                    .parent()
                    .map(std::fs::create_dir_all)
                    .unwrap_or(Ok(()))
                    .and_then(|_| std::fs::write(&path, crate::export::markdown(&s)));
                match result {
                    Ok(()) => self.notice(format!("Exported the session to {}", path.display())),
                    Err(e) => self.error(format!("Couldn't export: {e}")),
                }
            }
            ("/mcp", []) => {
                let lines = self.harness.mcp.describe();
                self.notice(if lines.is_empty() {
                    "No MCP servers configured (the `mcp` config key).".to_string()
                } else {
                    format!("MCP servers:\n{}\n/mcp login|logout|reconnect <name>", lines.join("\n"))
                });
            }
            ("/mcp", ["login", name]) => {
                let (h, tx, name) = (self.harness.clone(), self.tx.clone(), name.to_string());
                self.notice(format!("Starting the login to {name}..."));
                tokio::spawn(async move {
                    let r = async {
                        let login = h.mcp.login(&name).await.map_err(|e| format!("{e:#}"))?;
                        let _ = tx.send(AppEvent::Status(vec![format!(
                            "Log in to {name} in your browser (opening it now). If it didn't open: {}",
                            login.url
                        )]));
                        codeit_harness::util::open_url(&login.url);
                        login.wait().await.map_err(|e| format!("{e:#}"))?;
                        h.mcp.connect(&name).await;
                        Ok(name)
                    }
                    .await;
                    let _ = tx.send(AppEvent::McpLogin(r));
                });
            }
            ("/mcp", ["logout", name]) => match self.harness.mcp.url(name) {
                Some(url) => {
                    let _ = codeit_harness::mcp_oauth::forget(&url);
                    self.notice(format!("Forgot the login to {name}."));
                    let (h, name) = (self.harness.clone(), name.to_string());
                    tokio::spawn(async move { h.mcp.connect(&name).await });
                }
                None => self.notice(format!("{name} isn't a remote MCP server.")),
            },
            ("/mcp", ["reconnect", name]) => {
                let (h, name) = (self.harness.clone(), name.to_string());
                self.notice(format!("Reconnecting {name}..."));
                tokio::spawn(async move { h.mcp.connect(&name).await });
            }
            ("/effort", []) => self.open_effort(),
            ("/effort", [level]) => match &self.model {
                Some(_) if *level == "default" || *level == "none" => self.set_effort(None),
                Some(m) if m.efforts.iter().any(|e| e == level) => self.set_effort(Some(level.to_string())),
                Some(m) if m.efforts.is_empty() => self.notice(format!("{} has no reasoning effort levels.", m.name)),
                Some(m) => self.notice(format!("{} accepts: default, {}", m.name, m.efforts.join(", "))),
                None => self.notice("Pick a model first."),
            },
            ("/agent", []) => self.cycle_agent(),
            ("/agent", [a]) => {
                if self.harness.primary_agents().iter().any(|x| x.name == *a) {
                    self.agent = a.to_string();
                } else {
                    let names: Vec<String> = self.harness.primary_agents().iter().map(|x| x.name.clone()).collect();
                    self.notice(format!("Agents: {}", names.join(", ")));
                }
            }
            ("/new", _) => {
                self.interrupt();
                self.new_session();
            }
            ("/session", _) if idle => {
                let list = Session::list(Some(&self.harness.cwd));
                if list.is_empty() {
                    self.notice("No saved sessions in this folder yet.");
                } else {
                    self.picker = Some(Picker { kind: PickerKind::Session(list), filter: String::new(), selected: 0 });
                }
            }
            ("/undo", _) if idle => {
                let undone = self.session.lock().unwrap().undo();
                match undone {
                    Some(u) => {
                        let _ = self.session.lock().unwrap().save();
                        self.rebuild();
                        self.input = u.prompt;
                        self.cursor = self.input.len();
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
                            msg.push_str(&format!(
                                " Left alone, since they changed after the turn: {}.",
                                skipped.join(", ")
                            ));
                        }
                        self.notice(msg);
                    }
                    None => self.notice("Nothing to undo."),
                }
            }
            ("/compact", _) if idle => {
                if self.model.is_none() {
                    self.notice("Pick a model first.");
                } else {
                    self.start(Input::Compact);
                }
            }
            ("/approvals", []) => self.notice(match self.approval {
                Approval::Auto => {
                    "Approvals: auto (tools run without asking, except outside the project). /approvals ask to change."
                }
                Approval::Ask => "Approvals: ask (edits, commands and fetches ask first). /approvals auto to change.",
            }),
            ("/approvals", [mode]) => {
                self.approval = match *mode {
                    "ask" => Approval::Ask,
                    _ => Approval::Auto,
                };
                self.saved.approval = self.approval;
                self.saved.save();
                self.notice(format!("Approvals: {}", if self.approval == Approval::Ask { "ask" } else { "auto" }));
            }
            ("/login", rest) => self.open_login(&rest.join(" ")),
            ("/logout", [id]) => match crate::login::logout(id) {
                Ok(true) => {
                    self.notice(format!("Removed codeit's login for {id}."));
                    self.refresh_models();
                }
                Ok(false) => self.notice(format!(
                    "codeit had no login saved for {id} (an opencode login or an environment variable, if any, still apply)."
                )),
                Err(e) => self.error(format!("{e:#}")),
            },
            ("/logout", _) => self.notice("Usage: /logout <provider>  (codeit providers lists them)"),
            ("/status", _) => {
                let h = self.harness.clone();
                let tx = self.tx.clone();
                let session = {
                    let s = self.session.lock().unwrap();
                    format!(
                        "Session: {} ({} messages){}",
                        if s.title.is_empty() { "new" } else { &s.title },
                        s.entries.len(),
                        if s.summary.is_some() { ", summarized" } else { "" }
                    )
                };
                tokio::spawn(async move {
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
                    let running = h.lsp.describe().await;
                    let available: Vec<&str> = h
                        .servers
                        .iter()
                        .filter(|s| s.command.first().is_some_and(|c| codeit_harness::lsp::installed(c)))
                        .map(|s| s.id.as_str())
                        .collect();
                    lines.push(format!(
                        "Language servers: {} installed{}",
                        if available.is_empty() { "none".into() } else { available.join(", ") },
                        if running.is_empty() { String::new() } else { format!("; running: {}", running.join(", ")) }
                    ));
                    let skills = h.skills();
                    let skills: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
                    lines
                        .push(format!("Skills: {}", if skills.is_empty() { "none".into() } else { skills.join(", ") }));
                    let files: Vec<String> =
                        h.instruction_files().iter().map(|f| codeit_harness::util::display(&h.cwd, f)).collect();
                    lines.push(format!(
                        "Instructions: {}",
                        if files.is_empty() { "none".into() } else { files.join(", ") }
                    ));
                    lines.push(session);
                    let _ = tx.send(AppEvent::Status(lines));
                });
            }
            ("/help", _) => {
                let mut lines: Vec<String> =
                    self.commands.iter().map(|c| format!("{:<12} {:<26} {}", c.name, c.args, c.help)).collect();
                lines.push(String::new());
                lines.push(
                    "enter send · shift+enter or ctrl+j newline · tab switch agent · ctrl+t effort · esc stops the answer"
                        .into(),
                );
                lines.push("↑↓ (empty input) select an action · enter opens or closes it · esc esc edits your last message".into());
                lines.push(
                    "ctrl+o actions folded/list/open · pgup/pgdn scroll · @path attaches a file · ctrl+c stops or quits · /exit quits".into(),
                );
                self.notice(lines.join("\n"));
            }
            ("/exit", _) => self.quit = true,
            ("/session" | "/undo" | "/compact", _) => {
                self.notice("Wait for the current turn to finish (esc interrupts).")
            }
            _ => {
                if let Some(ext) = self.commands.iter().find(|c| c.name == name).and_then(|c| c.ext.clone()) {
                    self.open_panel(ext, name.trim_start_matches('/'), &args.join(" "));
                    return true;
                }
                let bare = name.trim_start_matches('/');
                if self.harness.commands().iter().any(|c| c.name == bare) {
                    return false;
                }
                self.notice(format!("Unknown command {name}. Type /help."));
            }
        }
        true
    }
}

fn picker_with(kind: PickerKind) -> Picker {
    Picker { kind, filter: String::new(), selected: 0 }
}

pub fn tokens(n: u64) -> String {
    match n {
        0..1000 => n.to_string(),
        1000..10_000 => format!("{:.1}k", n as f64 / 1000.0),
        _ => format!("{}k", n / 1000),
    }
}

pub fn duration(secs: u64) -> String {
    if secs >= 60 { format!("{}m {:02}s", secs / 60, secs % 60) } else { format!("{secs}s") }
}

fn ago(t: u64) -> String {
    let d = codeit_harness::util::now().saturating_sub(t);
    match d {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", d / 60),
        3600..86_400 => format!("{}h ago", d / 3600),
        _ => format!("{}d ago", d / 86_400),
    }
}
