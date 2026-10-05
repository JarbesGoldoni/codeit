//! Model providers for codeit.
//!
//! Every provider implements [`Provider`]: it reports whether you are logged in, lists the
//! models your account can use, and streams a reply (text, reasoning, tool calls) as
//! [`ChatEvent`]s. The harness only talks to this trait, so providers stay independent of each
//! other. Messages are provider-neutral [`Part`]s; each provider converts them to its API.
//!
//! - [`copilot`]: GitHub Copilot, through a GitHub device login (port of opencode's plugin).
//! - [`zai`]: Z.ai's GLM Coding Plan, with an API key.
//! - [`catalog`]: every other provider opencode lists (models.dev), with an API key; OpenAI
//!   also with a ChatGPT Plus/Pro login ([`chatgpt`]).

pub mod auth;
pub mod catalog;
pub mod chatgpt;
pub mod copilot;
pub mod demo;
mod paths;
pub(crate) mod protocols;
mod sse;
pub mod zai;

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

pub use paths::{cache_dir, data_dir, migrate_from_done};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// One piece of a message. Tool results travel in user messages, as in Anthropic's API.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text {
        text: String,
    },
    /// The model's reasoning. `meta` is the provider's opaque data for replaying it (an Anthropic
    /// signature, OpenAI encrypted reasoning, Copilot's reasoning_opaque); only the same model gets it back.
    Reasoning {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        meta: Option<Value>,
    },
    ToolCall(ToolCall),
    ToolResult {
        id: String,
        content: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        error: bool,
    },
    /// An image the user attached or a tool read, base64-encoded. In a message carrying tool
    /// results it goes after them (APIs without images in tool results get it as user content).
    Image {
        /// `image/png`, `image/jpeg`, `image/gif` or `image/webp`.
        media_type: String,
        data: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Parsed arguments. Arguments that were not valid JSON arrive as a string.
    pub input: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub parts: Vec<Part>,
    /// `provider/model` that wrote an assistant message; reasoning `meta` is replayed only to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self { role: Role::User, parts: vec![Part::Text { text: text.into() }], model: None }
    }
    pub fn assistant(text: impl Into<String>) -> Self {
        Self { role: Role::Assistant, parts: vec![Part::Text { text: text.into() }], model: None }
    }
    /// The text parts joined.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for p in &self.parts {
            if let Part::Text { text } = p {
                if !out.is_empty() && !text.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
        out
    }
    /// The images: (media type, base64 data).
    pub fn images(&self) -> impl Iterator<Item = (&str, &str)> {
        self.parts.iter().filter_map(|p| match p {
            Part::Image { media_type, data } => Some((media_type.as_str(), data.as_str())),
            _ => None,
        })
    }
    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.parts.iter().filter_map(|p| if let Part::ToolCall(c) = p { Some(c) } else { None })
    }
}

/// A tool the model may call. `parameters` is a JSON Schema object.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Clone, Debug, Default)]
pub struct ChatRequest {
    /// Model id as the provider knows it (without the `provider/` prefix).
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    /// Reasoning effort, one of the model's [`ModelInfo::efforts`].
    pub effort: Option<String>,
    /// Stable for the whole conversation. Copilot sends it as X-Interaction-Id.
    pub session_id: String,
    /// No person typed the latest message: tool follow-ups, subagents, compaction.
    /// Copilot doesn't count these as premium requests (`x-initiator: agent`).
    pub agent_initiated: bool,
}

impl ChatRequest {
    /// Reasoning `meta` may be replayed to this model only if it wrote the message.
    pub(crate) fn same_model(&self, provider: &str, m: &Message) -> bool {
        m.model.as_deref().and_then(split_key) == Some((provider, self.model.as_str()))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ChatEvent {
    Text(String),
    Reasoning(String),
    /// Opaque data that completes the reasoning streamed so far (see [`Part::Reasoning`]).
    ReasoningMeta(Value),
    /// A tool call has begun streaming; its arguments follow in [`ChatEvent::ToolCall`].
    ToolStart {
        id: String,
        name: String,
    },
    ToolCall(ToolCall),
    Usage(Usage),
    /// The reply stopped early (output limit, content filter).
    Incomplete(String),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    /// The credits a reply cost, for providers that bill in credits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct ModelInfo {
    pub provider: &'static str,
    pub id: String,
    pub name: String,
    pub context: Option<u64>,
    /// Most input tokens a request may carry, when the provider says (else use `context`).
    pub max_input: Option<u64>,
    /// Reasoning effort levels the model accepts, lowest first. Empty: no effort control.
    pub efforts: Vec<String>,
    pub default_effort: Option<String>,
    /// Accepts images.
    pub vision: bool,
}

impl ModelInfo {
    /// `provider/model`, the form used on the command line and in saved state.
    pub fn key(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }
}

#[derive(Clone, Debug)]
pub enum AuthStatus {
    LoggedIn(String),
    LoggedOut(String),
}

/// What the user must do to finish a device login.
#[derive(Clone, Debug)]
pub struct DeviceLogin {
    pub url: String,
    pub code: String,
}

#[async_trait]
pub trait Provider: Send + Sync {
    /// Short id, used as the `provider/` prefix of model keys.
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    async fn status(&self) -> AuthStatus;
    async fn models(&self) -> Result<Vec<ModelInfo>>;
    /// Streams the reply into `events`. Returns when the reply is complete.
    async fn chat(&self, request: ChatRequest, events: UnboundedSender<ChatEvent>) -> Result<()>;
}

/// The providers codeit ships with.
pub fn all() -> Vec<Arc<dyn Provider>> {
    let mut v: Vec<Arc<dyn Provider>> = vec![Arc::new(copilot::Copilot::new()), Arc::new(zai::Zai)];
    v.extend(catalog::providers().into_iter().map(|p| p as Arc<dyn Provider>));
    if demo::enabled() {
        v.push(Arc::new(demo::Demo));
    }
    v
}

/// Splits `provider/model` (the model id itself may contain `/`).
pub fn split_key(key: &str) -> Option<(&str, &str)> {
    key.split_once('/')
}

pub(crate) fn http() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder().connect_timeout(std::time::Duration::from_secs(15)).build().expect("http client")
        })
        .clone()
}

/// Parses streamed tool arguments; empty means no arguments, invalid JSON is kept as a string.
pub(crate) fn parse_args(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return Value::Object(Default::default());
    }
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.into()))
}

/// First 500 characters of an error body, for messages.
pub(crate) fn snippet(text: &str) -> String {
    let t = text.trim();
    if t.is_empty() { "no details".into() } else { t.chars().take(500).collect() }
}
