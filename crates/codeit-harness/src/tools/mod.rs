//! Tools the model can call, and what each call gets to work with.

mod bash;
mod edit;
mod fetch;
mod files;
pub(crate) mod lsp;
mod patch;
mod question;
mod search;
mod skill;
mod task;
mod todo;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use codeit_providers::ToolSpec;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::Harness;
use crate::agent::Agent;
use crate::event::{Ask, Event, PermissionAsk, Reply};
use crate::permission::{Action, Ruleset};
use crate::session::Session;
use crate::util;

pub use edit::replace;
pub use patch::{apply_patch, parse_patch};

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn spec(&self, harness: &Harness) -> ToolSpec;
    /// The permission that turns this tool off for an agent (default: its name).
    fn permission(&self) -> &str {
        self.name()
    }
    /// Read-only calls may run in parallel with each other.
    fn read_only(&self) -> bool {
        false
    }
    /// Short description of a call for the interface, like `src/main.rs` for read.
    fn title(&self, input: &Value, cwd: &Path) -> String;
    async fn run(&self, input: Value, ctx: &Ctx) -> Result<Output>;
}

/// What a tool returns: `content` goes to the model; `display` (if set) is shown to the user
/// instead, `diff` is a unified diff of file changes, and `images` (media type, base64) go to
/// the model with the result.
#[derive(Debug, Default)]
pub struct Output {
    pub content: String,
    pub display: Option<String>,
    pub diff: Option<String>,
    pub images: Vec<(String, String)>,
    /// Files written, checked by the language servers afterwards.
    pub touched: Vec<PathBuf>,
}

impl Output {
    pub fn text(content: impl Into<String>) -> Self {
        Self { content: content.into(), ..Default::default() }
    }
}

/// A permission check refused the call.
#[derive(Debug)]
pub struct Refused {
    /// The user said no (the turn stops), rather than a configured rule.
    pub by_user: bool,
    pub message: String,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refused {}

/// Everything a tool call can use.
pub struct Ctx {
    pub harness: Arc<Harness>,
    pub session: Arc<Mutex<Session>>,
    pub agent: Agent,
    /// The agent's effective rules (defaults, config, agent, approval preset).
    pub rules: Ruleset,
    pub call_id: String,
    /// Index of the assistant entry that made the call.
    pub entry: usize,
    pub events: UnboundedSender<Event>,
    pub cancel: CancellationToken,
    /// Subagent nesting depth (0 for the session the user talks to).
    pub depth: usize,
}

impl Ctx {
    pub fn cwd(&self) -> &Path {
        &self.harness.cwd
    }

    pub fn resolve(&self, path: &str) -> PathBuf {
        util::resolve(self.cwd(), path)
    }

    pub fn display(&self, path: &Path) -> String {
        util::display(self.cwd(), path)
    }

    /// Checks `permission` for each pattern; asks the user for those set to ask.
    /// `always` are the patterns an "always allow" answer adds for the rest of the session.
    pub async fn ask(
        &self,
        permission: &str,
        patterns: &[String],
        always: &[String],
        title: String,
        detail: Option<String>,
    ) -> Result<()> {
        let mut pending = Vec::new();
        for p in patterns {
            match self.rules.evaluate(permission, p) {
                Action::Allow => {}
                Action::Deny => {
                    return Err(Refused {
                        by_user: false,
                        message: format!("The configuration denies {permission} for `{p}`. Don't retry it; find another way or ask the user."),
                    }
                    .into());
                }
                Action::Ask => {
                    let approved = self.session.lock().unwrap().approved.evaluate(permission, p) == Action::Allow;
                    if !approved {
                        pending.push(p.clone());
                    }
                }
            }
        }
        if pending.is_empty() {
            return Ok(());
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        let always_list = if always.is_empty() { pending.clone() } else { always.to_vec() };
        let ask = PermissionAsk {
            permission: permission.into(),
            patterns: pending,
            always: always_list,
            title,
            detail,
            reply: tx,
        };
        if self.events.send(Event::Ask(Ask::Permission(ask))).is_err() {
            return Err(refused_by_user(None).into());
        }
        let reply = tokio::select! {
            r = rx => r.unwrap_or(Reply::Reject(None)),
            _ = self.cancel.cancelled() => Reply::Reject(None),
        };
        match reply {
            Reply::Once => Ok(()),
            Reply::Always => {
                let mut s = self.session.lock().unwrap();
                let list = if always.is_empty() { patterns } else { always };
                for p in list {
                    s.approved.push(permission, p, Action::Allow);
                }
                Ok(())
            }
            Reply::Reject(msg) => Err(refused_by_user(msg).into()),
        }
    }

    /// Checks access to a path: `external_directory` when it is outside the project, then
    /// `permission` (read or edit) on its project-relative path.
    pub async fn check_path(&self, path: &Path, permission: &str, detail: Option<String>) -> Result<()> {
        let h = &self.harness;
        if !path.starts_with(&h.root) && !path.starts_with(&h.cwd) {
            let dir = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
            let d = dir.to_string_lossy().into_owned();
            self.ask(
                "external_directory",
                &[path.to_string_lossy().into_owned()],
                &[format!("{d}/*")],
                format!("access outside the project: {}", path.display()),
                None,
            )
            .await?;
        }
        let rel = util::display(&h.root, path);
        let verb = if permission == "edit" { "edit" } else { permission };
        self.ask(
            permission,
            std::slice::from_ref(&rel),
            &["*".into()],
            format!("{verb} {}", self.display(path)),
            detail,
        )
        .await
    }

    /// Saves output too long for the context to a file and returns the kept part plus a note
    /// saying where the rest is.
    pub fn limit(&self, text: &str) -> String {
        let (max_lines, max_bytes) = self.harness.output_limits();
        let (kept, cut) = util::head_tail(text, max_lines, max_bytes);
        if !cut {
            return kept;
        }
        match save_output(text) {
            Some(path) => format!(
                "{kept}\n\n(Output was long: the full {} lines are in {}. Use grep or read with offset on it instead of rerunning.)",
                text.lines().count(),
                path.display()
            ),
            None => kept,
        }
    }
}

pub fn refused_by_user(msg: Option<String>) -> Refused {
    let message = match msg.filter(|m| !m.trim().is_empty()) {
        Some(m) => format!("The user rejected this call and said: {m}"),
        None => "The user rejected this call. Stop and wait for their instructions.".into(),
    };
    Refused { by_user: true, message }
}

/// Where long outputs go: `~/.cache/codeit/tool-output/`, kept for a week.
pub fn output_dir() -> PathBuf {
    codeit_providers::cache_dir().join("tool-output")
}

pub(crate) fn save_output(text: &str) -> Option<PathBuf> {
    let dir = output_dir();
    std::fs::create_dir_all(&dir).ok()?;
    // Drop files older than a week.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let week = std::time::Duration::from_secs(7 * 24 * 3600);
        for e in entries.flatten() {
            let old =
                e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|a| a > week);
            if old {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let path = dir.join(format!("{}-{}.txt", util::now(), &uuid::Uuid::new_v4().to_string()[..8]));
    std::fs::write(&path, text).ok()?;
    Some(path)
}

/// Required string argument.
pub fn arg<'a>(input: &'a Value, key: &str) -> Result<&'a str> {
    input[key].as_str().ok_or_else(|| anyhow::anyhow!("missing required argument `{key}` (a string)"))
}

pub fn opt_u64(input: &Value, key: &str) -> Option<u64> {
    input[key].as_u64().or_else(|| input[key].as_str().and_then(|s| s.parse().ok()))
}

pub fn spec(name: &str, description: &str, parameters: Value) -> ToolSpec {
    ToolSpec { name: name.into(), description: description.trim().into(), parameters }
}

/// Whether a model edits with apply_patch (GPT-5 class) rather than edit/write, as opencode does.
pub fn uses_patch(model_id: &str) -> bool {
    model_id.contains("gpt-") && !model_id.contains("gpt-4") && !model_id.contains("oss")
}

/// The built-in tools, in the order they are offered.
pub fn builtin(patch: bool) -> Vec<Arc<dyn Tool>> {
    let mut v: Vec<Arc<dyn Tool>> = vec![Arc::new(bash::Bash), Arc::new(files::Read)];
    if patch {
        v.push(Arc::new(patch::ApplyPatch));
    } else {
        v.push(Arc::new(edit::Edit));
        v.push(Arc::new(files::Write));
    }
    v.extend([
        Arc::new(search::Grep) as Arc<dyn Tool>,
        Arc::new(search::Glob),
        Arc::new(todo::Todo),
        Arc::new(task::Task),
        Arc::new(skill::SkillTool),
        Arc::new(fetch::WebFetch),
        Arc::new(fetch::WebSearch),
        Arc::new(lsp::LspTool),
        Arc::new(question::QuestionTool),
    ]);
    v
}

/// Reads a file for the model (shared by the read tool and `@file` attachments).
pub use files::read_for_model;

/// Images larger than this aren't sent (providers reject them, and they cost a lot).
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;

/// The media type of an image file, by its extension.
pub fn image_type(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// An image file as (media type, base64), for the model.
pub fn read_image(path: &Path) -> Result<(String, String)> {
    let kind =
        image_type(path).ok_or_else(|| anyhow::anyhow!("{} is not a PNG, JPEG, GIF or WebP image", path.display()))?;
    let bytes = std::fs::read(path)?;
    if bytes.len() > MAX_IMAGE_BYTES {
        anyhow::bail!("{} is {} MB; images over 5 MB can't be sent.", path.display(), bytes.len() / 1_000_000);
    }
    Ok((kind.to_string(), util::base64(&bytes)))
}
