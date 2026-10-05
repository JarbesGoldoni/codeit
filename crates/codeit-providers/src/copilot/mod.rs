//! GitHub Copilot provider, ported from opencode's Copilot plugin.
//!
//! Login is GitHub's device flow; the GitHub token goes straight to the Copilot API as a
//! bearer token. Each model is served on one of three APIs (see [`models::Endpoint`]).

pub(crate) mod auth;
pub(crate) mod models;

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue};
use tokio::sync::{Mutex, mpsc::UnboundedSender};

pub use auth::{DeviceFlow, Token};
use models::{CopilotModel, Endpoint};

use crate::paths::debug;
use crate::protocols;
use crate::{AuthStatus, ChatEvent, ChatRequest, ModelInfo, Provider};

const API_VERSION: &str = "2026-06-01";

#[derive(Default)]
pub struct Copilot {
    catalog: Mutex<Vec<CopilotModel>>,
}

impl Copilot {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Starts a device login. Show `flow.login` to the user, then `flow.wait().await` saves the token.
pub async fn login(enterprise: Option<&str>) -> Result<DeviceFlow> {
    auth::start(enterprise).await
}

/// The GitHub token Copilot uses (CODEIT_COPILOT_TOKEN, codeit's login, then opencode's), for
/// plugins that call other GitHub Copilot APIs (quota).
pub fn saved_token() -> Option<Token> {
    auth::load()
}

/// Removes codeit's saved Copilot login. Returns false when there was none.
pub fn logout() -> Result<bool> {
    auth::remove()
}

fn token() -> Result<auth::Token> {
    auth::load()
        .ok_or_else(|| anyhow!("Not logged in to GitHub Copilot. Run `/login copilot` (or `codeit login copilot`)."))
}

fn base_url(t: &auth::Token) -> String {
    match &t.enterprise {
        Some(d) => format!("https://copilot-api.{}", auth::normalize_domain(d)),
        None => std::env::var("CODEIT_COPILOT_API").unwrap_or_else(|_| "https://api.githubcopilot.com".into()),
    }
}

/// `session` is (interaction id, agent initiated) for chat requests.
fn headers(t: &auth::Token, session: Option<(&str, bool)>) -> HeaderMap {
    let mut h = HeaderMap::new();
    let mut put = |k: &'static str, v: &str| {
        if let Ok(v) = HeaderValue::from_str(v) {
            h.insert(k, v);
        }
    };
    put("authorization", &format!("Bearer {}", t.token));
    put("user-agent", &format!("codeit/{}", env!("CARGO_PKG_VERSION")));
    put("x-github-api-version", API_VERSION);
    if let Some((id, agent)) = session {
        put("x-interaction-id", id);
        put("openai-intent", "conversation-edits");
        // Only what a person typed counts as a premium request; tool follow-ups and subagents are "agent".
        put("x-initiator", if agent { "agent" } else { "user" });
    }
    h
}

fn to_info(m: &CopilotModel) -> ModelInfo {
    ModelInfo {
        provider: "copilot",
        id: m.id.clone(),
        name: m.name.clone(),
        context: m.context,
        max_input: m.max_input,
        efforts: m.efforts.clone(),
        default_effort: None,
        vision: m.vision,
    }
}

#[async_trait]
impl Provider for Copilot {
    fn id(&self) -> &'static str {
        "copilot"
    }

    fn name(&self) -> &'static str {
        "GitHub Copilot"
    }

    async fn status(&self) -> AuthStatus {
        match auth::load() {
            Some(t) => AuthStatus::LoggedIn(match &t.enterprise {
                Some(d) => format!("GitHub Enterprise {d} (from {})", t.source),
                None => format!("github.com (from {})", t.source),
            }),
            None => AuthStatus::LoggedOut("not logged in; run `/login copilot`".into()),
        }
    }

    async fn models(&self) -> Result<Vec<ModelInfo>> {
        let t = token()?;
        let list = models::fetch(&base_url(&t), headers(&t, None)).await?;
        let infos = list.iter().filter(|m| m.picker_enabled).map(to_info).collect();
        *self.catalog.lock().await = list;
        Ok(infos)
    }

    async fn chat(&self, req: ChatRequest, events: UnboundedSender<ChatEvent>) -> Result<()> {
        let t = token()?;
        let model = self.catalog.lock().await.iter().find(|m| m.id == req.model).cloned();
        let endpoint = model.as_ref().map(|m| m.endpoint).unwrap_or_else(|| models::guess_endpoint(&req.model));
        let base = base_url(&t);
        let mut h = headers(&t, Some((&req.session_id, req.agent_initiated)));
        // Copilot wants requests that carry images flagged.
        if req.messages.iter().any(|m| m.images().next().is_some()) {
            h.insert("copilot-vision-request", HeaderValue::from_static("true"));
        }
        let (url, body) = match endpoint {
            Endpoint::Chat => (format!("{base}/chat/completions"), protocols::chat_body(&req, "copilot")),
            Endpoint::Responses => {
                (format!("{base}/responses"), protocols::responses_body(&req, "copilot", model.as_ref()))
            }
            Endpoint::Messages => {
                h.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
                h.insert("anthropic-beta", HeaderValue::from_static("interleaved-thinking-2025-05-14"));
                (format!("{base}/v1/messages"), protocols::messages_body(&req, "copilot", model.as_ref()))
            }
        };
        debug(
            "copilot.request",
            format!(
                "model={} endpoint={endpoint:?} tools={} initiator={}",
                req.model,
                req.tools.len(),
                if req.agent_initiated { "agent" } else { "user" }
            ),
        );
        if std::env::var_os("CODEIT_DEBUG_BODY").is_some() {
            debug("copilot.request.body", &body);
        }

        let mut attempt = 0;
        let res = loop {
            attempt += 1;
            let res = crate::http()
                .post(&url)
                .headers(h.clone())
                .header("accept", "text/event-stream")
                .json(&body)
                .send()
                .await?;
            let status = res.status().as_u16();
            if (status == 429 || status >= 500) && attempt < 3 {
                debug("copilot.request.retry", status);
                tokio::time::sleep(Duration::from_millis(1000 << attempt)).await;
                continue;
            }
            break res;
        };
        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            debug("copilot.request.http-error", format!("{status} {}", crate::snippet(&text)));
            let hint = match status.as_u16() {
                401 => " Your GitHub login was rejected; run `/login copilot` again.",
                403 => {
                    " Copilot refused this model or request; check that the model is enabled in your Copilot settings."
                }
                429 => " Copilot is rate limiting you or your premium requests are used up.",
                _ => "",
            };
            bail!("Copilot {status}: {}{hint}", crate::snippet(&text));
        }
        match endpoint {
            Endpoint::Chat => protocols::chat_stream(res, &events).await,
            Endpoint::Responses => protocols::responses_stream(res, &events).await,
            Endpoint::Messages => protocols::messages_stream(res, &events).await,
        }
    }
}
