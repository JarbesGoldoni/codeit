//! What the harness tells the interface while it works, and the questions it asks.

use codeit_providers::Usage;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::session::Todo;

pub enum Event {
    /// Streamed reply text.
    Text(String),
    /// Streamed reasoning.
    Reasoning(String),
    /// The reply so far is being discarded (the step is retried).
    Reset,
    /// The model began a tool call; its arguments are still streaming.
    ToolPending {
        id: String,
        name: String,
    },
    /// A tool call starts running. `title` is a short description, like `src/main.rs` for read.
    ToolStart {
        id: String,
        name: String,
        title: String,
        input: Value,
    },
    /// Progress of a running tool (a subagent's current step).
    ToolProgress {
        id: String,
        text: String,
    },
    /// A tool call finished. `output` is what the user sees (shortened), `diff` a unified diff
    /// for file changes, `lines` the length of the full output, and `digest` what the model
    /// got instead of it when it was condensed.
    ToolDone {
        id: String,
        name: String,
        title: String,
        output: String,
        error: bool,
        diff: Option<String>,
        lines: Option<usize>,
        digest: Option<String>,
    },
    Todos(Vec<Todo>),
    /// The model needs an answer before it can go on.
    Ask(Ask),
    /// A model request finished. `context` is (tokens used, usable limit) when the limit is known.
    Step {
        usage: Usage,
        context: Option<(u64, u64)>,
    },
    /// Something worth a line in the history (retries, compaction, limits).
    Notice(String),
    Error(String),
    /// The session's title was set.
    Title(String),
    /// The turn is over; the session is saved.
    Done,
}

pub enum Ask {
    Permission(PermissionAsk),
    Question(QuestionAsk),
}

pub struct PermissionAsk {
    /// Permission name: a tool, `edit`, `external_directory`, `doom_loop`.
    pub permission: String,
    /// What would be allowed: paths, commands, URLs.
    pub patterns: Vec<String>,
    /// What an "always" answer allows for the rest of the session (`*` is everything).
    pub always: Vec<String>,
    /// Short description, e.g. `bash: cargo test`.
    pub title: String,
    /// Longer detail to show: a diff, a full command.
    pub detail: Option<String>,
    pub reply: oneshot::Sender<Reply>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Reply {
    Once,
    /// Allow this and similar calls for the rest of the session.
    Always,
    /// Refuse; the optional text is passed to the model.
    Reject(Option<String>),
}

pub struct QuestionAsk {
    pub questions: Vec<Question>,
    /// One list of chosen labels (or typed answers) per question; `None` if dismissed.
    pub reply: oneshot::Sender<Option<Vec<Vec<String>>>>,
}

#[derive(Clone, Debug)]
pub struct Question {
    pub question: String,
    pub header: String,
    pub options: Vec<(String, String)>,
    pub multiple: bool,
}
