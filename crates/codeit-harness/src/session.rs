//! A conversation and everything the harness tracks about it, saved as JSON under
//! `~/.local/share/codeit/sessions/`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use codeit_providers::{Message, Part, Role, Usage};
use serde::{Deserialize, Serialize};

use crate::permission::Ruleset;
use crate::util;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    #[serde(flatten)]
    pub message: Message,
    /// What the person typed, on user prompts. Other text parts are context the harness added
    /// (attached files, command templates).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default)]
    pub time: u64,
    /// On prompts: the project's snapshot before and after the turn (see `snapshot`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

impl Entry {
    pub fn new(message: Message) -> Self {
        Self { message, prompt: None, usage: None, agent: None, time: util::now(), before: None, after: None }
    }
    /// A user message the person wrote (not one carrying tool results).
    pub fn is_prompt(&self) -> bool {
        self.message.role == Role::User && self.prompt.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Todo {
    pub content: String,
    /// `pending`, `in_progress`, `completed` or `cancelled`.
    pub status: String,
}

/// A file as it was before the harness changed it, so `/undo` can put it back.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileChange {
    /// Index of the entry whose tool call made the change.
    pub entry: usize,
    pub path: PathBuf,
    /// Content before the change; `None` if the file did not exist.
    pub before: Option<String>,
}

/// The last time the model saw a file, for the read-before-edit check and repeated reads.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FileMark {
    /// Modification time (ns) of the version the model knows.
    pub mtime: u64,
    /// The last read: (offset, limit, tool call id), when the model read it rather than wrote it.
    pub read: Option<(usize, usize, String)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Approval {
    /// opencode's behavior: tools run without asking, except outside the project.
    #[default]
    Auto,
    /// Ask before edits, commands (other than read-only ones) and web fetches.
    Ask,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub cwd: PathBuf,
    pub created: u64,
    pub updated: u64,
    /// Set on subagent sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub agent: String,
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub approval: Approval,
    pub entries: Vec<Entry>,
    /// First entry the model still sees; the ones before are covered by `summary`.
    #[serde(default)]
    pub context_start: usize,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub todos: Vec<Todo>,
    #[serde(default)]
    pub changes: Vec<FileChange>,
    #[serde(default)]
    pub files: HashMap<PathBuf, FileMark>,
    /// Nested instruction files already attached to a read.
    #[serde(default)]
    pub instructions: Vec<PathBuf>,
    /// "Always allow" answers given in this session.
    #[serde(default)]
    pub approved: Ruleset,
    /// Usage of the latest request, for the context meter.
    #[serde(default)]
    pub last_usage: Option<Usage>,
}

/// What `/undo` did.
pub struct Undo {
    /// The prompt of the removed turn, to edit and send again.
    pub prompt: String,
    pub restored: Vec<PathBuf>,
    /// Files the turn changed that were changed again since: left as they are.
    pub skipped: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub cwd: PathBuf,
    pub updated: u64,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

pub fn dir() -> PathBuf {
    codeit_providers::data_dir().join("sessions")
}

impl Session {
    pub fn new(cwd: &Path, agent: &str, model: Option<String>, effort: Option<String>) -> Self {
        let now = util::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title: String::new(),
            cwd: cwd.to_path_buf(),
            created: now,
            updated: now,
            parent: None,
            agent: agent.into(),
            model,
            effort,
            approval: Approval::default(),
            entries: Vec::new(),
            context_start: 0,
            summary: None,
            todos: Vec::new(),
            changes: Vec::new(),
            files: HashMap::new(),
            instructions: Vec::new(),
            approved: Ruleset::default(),
            last_usage: None,
        }
    }

    pub fn meta(&self) -> SessionMeta {
        SessionMeta {
            id: self.id.clone(),
            title: self.title.clone(),
            cwd: self.cwd.clone(),
            updated: self.updated,
            parent: self.parent.clone(),
            model: self.model.clone(),
        }
    }

    pub fn save(&mut self) -> Result<()> {
        self.updated = util::now();
        self.write()
    }

    fn write(&self) -> Result<()> {
        let d = dir();
        std::fs::create_dir_all(&d)?;
        write_atomic(&d.join(format!("{}.json", self.id)), &serde_json::to_vec(self)?)?;
        write_atomic(&d.join(format!("{}.meta.json", self.id)), &serde_json::to_vec(&self.meta())?)?;
        Ok(())
    }

    pub fn load(id: &str) -> Result<Session> {
        let path = dir().join(format!("{id}.json"));
        let text = std::fs::read(&path).with_context(|| format!("no session {id}"))?;
        Ok(serde_json::from_slice(&text)?)
    }

    /// Top-level sessions, newest first. With `cwd`, only those started there.
    pub fn list(cwd: Option<&Path>) -> Vec<SessionMeta> {
        let mut out: Vec<SessionMeta> = std::fs::read_dir(dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".meta.json"))
            .filter_map(|e| serde_json::from_slice::<SessionMeta>(&std::fs::read(e.path()).ok()?).ok())
            .filter(|m| m.parent.is_none() && cwd.is_none_or(|c| m.cwd == c))
            .collect();
        out.sort_by_key(|m| std::cmp::Reverse(m.updated));
        out
    }

    /// Renames a saved session, keeping its place in the list.
    pub fn rename(id: &str, title: &str) -> Result<()> {
        let mut s = Self::load(id)?;
        s.title = title.to_string();
        s.write()
    }

    pub fn delete(id: &str) {
        let _ = std::fs::remove_file(dir().join(format!("{id}.json")));
        let _ = std::fs::remove_file(dir().join(format!("{id}.meta.json")));
    }

    pub fn push(&mut self, entry: Entry) -> usize {
        self.entries.push(entry);
        self.entries.len() - 1
    }

    /// Records a file's content before a change, once per file per entry.
    pub fn record_change(&mut self, entry: usize, path: &Path) {
        if self.changes.iter().any(|c| c.entry == entry && c.path == path) {
            return;
        }
        // Huge or binary files are not kept; undo skips them.
        let before = match std::fs::metadata(path) {
            Ok(m) if m.len() > 2_000_000 => return,
            Ok(_) => match std::fs::read_to_string(path) {
                Ok(t) => Some(t),
                Err(_) => return,
            },
            Err(_) => None,
        };
        self.changes.push(FileChange { entry, path: path.to_path_buf(), before });
    }

    /// Marks the version of `path` on disk as known to the model.
    pub fn mark_known(&mut self, path: &Path, read: Option<(usize, usize, String)>) {
        let mtime = mtime(path);
        let mark = self.files.entry(path.to_path_buf()).or_default();
        mark.mtime = mtime;
        mark.read = read;
    }

    /// Removes the last prompt and everything after it, restoring the files changed since.
    /// Returns the prompt text and the paths restored.
    /// Removes the last turn and puts back the files it changed. With snapshots (git projects)
    /// that includes what its shell commands changed, except files changed again since;
    /// otherwise the edits codeit made itself.
    pub fn undo(&mut self) -> Option<Undo> {
        let start = self.entries.iter().rposition(Entry::is_prompt)?;
        if start < self.context_start {
            return None;
        }
        let prompt = self.entries[start].prompt.clone().unwrap_or_default();
        let mut restored = Vec::new();
        let mut skipped = Vec::new();
        let root = util::git_root(&self.cwd).unwrap_or(self.cwd.clone());
        let snap = match (&self.entries[start].before, &self.entries[start].after) {
            (Some(b), Some(a)) => crate::snapshot::restore(&root, b, a).ok(),
            _ => None,
        };
        match snap {
            Some(r) => {
                restored = r.restored;
                skipped = r.skipped;
            }
            None => {
                // Newest first, so the oldest copy of each file wins.
                let changes: Vec<FileChange> = self.changes.iter().filter(|c| c.entry >= start).cloned().collect();
                for c in changes.iter().rev() {
                    let ok = match &c.before {
                        Some(text) => std::fs::write(&c.path, text).is_ok(),
                        None => std::fs::remove_file(&c.path).is_ok() || !c.path.exists(),
                    };
                    if ok && !restored.contains(&c.path) {
                        restored.push(c.path.clone());
                    }
                }
            }
        }
        self.changes.retain(|c| c.entry < start);
        self.entries.truncate(start);
        for p in &restored {
            self.files.remove(p);
        }
        Some(Undo { prompt, restored, skipped })
    }

    /// The messages the model sees: the summary (if any), then the entries after it.
    pub fn context(&self) -> Vec<Message> {
        let mut out = Vec::new();
        if let Some(s) = &self.summary {
            out.push(Message::user(format!(
                "<conversation-summary>\nThe earlier part of this conversation was summarized:\n\n{s}\n</conversation-summary>"
            )));
        }
        out.extend(self.entries[self.context_start.min(self.entries.len())..].iter().map(|e| e.message.clone()));
        answer_all(&mut out);
        out
    }

    /// The tool call with this id, if it is in the entries.
    pub fn find_call(&self, id: &str) -> Option<&codeit_providers::ToolCall> {
        self.entries.iter().rev().flat_map(|e| e.message.tool_calls()).find(|c| c.id == id)
    }

    /// True if the result of tool call `id` is still in the model's context, unpruned.
    pub fn result_visible(&self, id: &str) -> bool {
        self.entries[self.context_start.min(self.entries.len())..].iter().any(|e| {
            e.message
                .parts
                .iter()
                .any(|p| matches!(p, Part::ToolResult { id: rid, content, .. } if rid == id && content != PRUNED))
        })
    }
}

/// Every API rejects a tool call without a result in the next message. Calls left unanswered
/// (a run killed mid-step) get an "interrupted" result, and results whose call is gone (cut
/// off by a summary) become plain text.
fn answer_all(messages: &mut Vec<Message>) {
    let mut i = 0;
    while i < messages.len() {
        let calls: Vec<String> = messages[i].tool_calls().map(|c| c.id.clone()).collect();
        if messages[i].role == Role::User {
            let known: Vec<String> =
                if i > 0 { messages[i - 1].tool_calls().map(|c| c.id.clone()).collect() } else { Vec::new() };
            for p in messages[i].parts.iter_mut() {
                if let Part::ToolResult { id, content, .. } = p
                    && !known.contains(id)
                {
                    *p = Part::Text { text: format!("[result of an earlier tool call]\n{content}") };
                }
            }
        }
        if !calls.is_empty() {
            if messages.get(i + 1).is_none_or(|m| m.role != Role::User) {
                messages.insert(i + 1, Message { role: Role::User, parts: Vec::new(), model: None });
            }
            let next = &mut messages[i + 1];
            let missing: Vec<String> = calls
                .into_iter()
                .filter(|id| !next.parts.iter().any(|p| matches!(p, Part::ToolResult { id: r, .. } if r == id)))
                .collect();
            for (k, id) in missing.into_iter().enumerate() {
                next.parts.insert(
                    k,
                    Part::ToolResult { id, content: "Interrupted: this call never ran.".into(), error: true },
                );
            }
        }
        i += 1;
    }
}

/// What an old tool result is replaced with when the context is pruned.
pub const PRUNED: &str = "[Old tool output removed to save context. Run the tool again if you need it.]";

pub fn mtime(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_keeps_the_place_in_the_list() {
        crate::test_home();
        let mut s = Session::new(Path::new("/rename-test"), "build", None, None);
        s.title = "old".into();
        s.save().unwrap();
        let updated = s.updated;
        Session::rename(&s.id, "new").unwrap();
        let back = Session::load(&s.id).unwrap();
        assert_eq!((back.title.as_str(), back.updated), ("new", updated));
        assert_eq!(Session::list(Some(Path::new("/rename-test")))[0].title, "new");
        Session::delete(&s.id);
        assert!(Session::list(Some(Path::new("/rename-test"))).is_empty());
    }

    #[test]
    fn undo_restores_files_and_entries() {
        let dir = std::env::temp_dir().join(format!("codeit-undo-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        std::fs::write(&a, "old").unwrap();

        let mut s = Session::new(&dir, "build", None, None);
        let mut first = Entry::new(Message::user("one"));
        first.prompt = Some("one".into());
        s.push(first);
        let mut second = Entry::new(Message::user("two"));
        second.prompt = Some("two".into());
        s.push(second);
        let i = s.push(Entry::new(Message::assistant("editing")));
        s.record_change(i, &a);
        std::fs::write(&a, "new").unwrap();
        s.record_change(i, &a);
        std::fs::write(&a, "newer").unwrap();
        s.record_change(i, &b);
        std::fs::write(&b, "created").unwrap();

        let u = s.undo().unwrap();
        assert_eq!(u.prompt, "two");
        assert_eq!(u.restored.len(), 2);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "old");
        assert!(!b.exists());
        assert_eq!(s.entries.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn answers_every_tool_call() {
        use codeit_providers::ToolCall;
        let call =
            |id: &str| Part::ToolCall(ToolCall { id: id.into(), name: "read".into(), input: serde_json::json!({}) });
        let mut s = Session::new(Path::new("/tmp"), "build", None, None);
        s.push(Entry::new(Message::user("go")));
        s.push(Entry::new(Message { role: Role::Assistant, parts: vec![call("a"), call("b")], model: None }));
        s.push(Entry::new(Message {
            role: Role::User,
            parts: vec![Part::ToolResult { id: "b".into(), content: "ok".into(), error: false }],
            model: None,
        }));
        s.push(Entry::new(Message { role: Role::Assistant, parts: vec![call("c")], model: None }));
        let ctx = s.context();
        assert_eq!(ctx.len(), 5);
        let ids = |m: &Message| -> Vec<String> {
            m.parts
                .iter()
                .filter_map(|p| if let Part::ToolResult { id, .. } = p { Some(id.clone()) } else { None })
                .collect()
        };
        assert_eq!(ids(&ctx[2]), ["a", "b"]);
        assert_eq!(ids(&ctx[4]), ["c"]);
        // A result whose call was summarized away becomes text.
        s.context_start = 2;
        s.summary = Some("earlier".into());
        assert!(matches!(&s.context()[1].parts[0], Part::Text { text } if text.contains("earlier tool call")));
    }

    #[test]
    fn round_trips_entries() {
        let mut e = Entry::new(Message::user("hi"));
        e.prompt = Some("hi".into());
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["role"], "user");
        assert_eq!(json["parts"][0]["type"], "text");
        let back: Entry = serde_json::from_value(json).unwrap();
        assert!(back.is_prompt());
    }
}
