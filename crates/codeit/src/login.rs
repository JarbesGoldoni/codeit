//! Logging in to providers, shared by `codeit login` and the TUI's `/login`: which providers there
//! are (opencode's order), how each one logs in, and running the login.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Result, bail};
use codeit_providers::{AuthStatus, Provider};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Method {
    /// GitHub's device login, on github.com.
    Copilot,
    /// GitHub's device login on a GitHub Enterprise domain.
    CopilotEnterprise,
    ChatGptBrowser,
    ChatGptDevice,
    ApiKey,
    /// A plugin provider that logs in with its own tool: show what its status says to do.
    Own,
}

impl Method {
    pub fn label(self) -> &'static str {
        match self {
            Method::Copilot => "GitHub login (github.com)",
            Method::CopilotEnterprise => "GitHub Enterprise login",
            Method::ChatGptBrowser => "ChatGPT Plus/Pro (browser)",
            Method::ChatGptDevice => "ChatGPT Plus/Pro (code, no browser here)",
            Method::ApiKey => "API key",
            Method::Own => "its own login",
        }
    }
}

/// A provider as the login list shows it.
#[derive(Clone, Debug)]
pub struct Choice {
    pub id: String,
    pub name: String,
    pub hint: String,
    pub logged_in: bool,
    pub methods: Vec<Method>,
    /// Where to get a key, and the variables that can hold one.
    pub doc: Option<String>,
    pub env: Vec<String>,
    /// What the provider says to do to log in (its status when logged out).
    pub how: String,
}

/// opencode's order, then codeit's own, then by name.
fn rank(id: &str) -> usize {
    ["opencode", "openai", "copilot", "google", "anthropic", "openrouter", "vercel", "zai-coding-plan"]
        .iter()
        .position(|p| *p == id)
        .unwrap_or(99)
}

/// `plugin`: a provider added by a plugin binary, not codeit's own nor the catalog's.
fn methods(id: &str, plugin: bool) -> Vec<Method> {
    match id {
        "copilot" => vec![Method::Copilot, Method::CopilotEnterprise],
        "openai" => vec![Method::ChatGptBrowser, Method::ChatGptDevice, Method::ApiKey],
        _ if plugin => vec![Method::Own],
        _ => vec![Method::ApiKey],
    }
}

fn hint(id: &str) -> &'static str {
    match id {
        "openai" => "ChatGPT Plus/Pro or API key",
        "copilot" => "GitHub login",
        "zai-coding-plan" => "coding plan key",
        "opencode" => "recommended by opencode",
        _ => "",
    }
}

/// Every provider that can be logged in to (the demo one aside), in the order to show them.
pub async fn choices(providers: &[Arc<dyn Provider>]) -> Vec<Choice> {
    let catalog: Vec<_> = codeit_providers::catalog::providers();
    let mut out = Vec::new();
    for p in providers.iter().filter(|p| p.id() != "demo") {
        let local = catalog.iter().find(|c| c.id() == p.id());
        if local.is_some_and(|c| c.is_local()) {
            continue;
        }
        let status = p.status().await;
        let plugin = local.is_none() && !["copilot", "zai-coding-plan", "openai"].contains(&p.id());
        out.push(Choice {
            id: p.id().to_string(),
            name: p.name().to_string(),
            hint: hint(p.id()).to_string(),
            logged_in: matches!(status, AuthStatus::LoggedIn(_)),
            methods: methods(p.id(), plugin),
            how: match status {
                AuthStatus::LoggedIn(s) => format!("Logged in: {s}"),
                AuthStatus::LoggedOut(s) => s,
            },
            doc: local.and_then(|c| c.doc.clone()),
            env: local.map(|c| c.env_keys()).unwrap_or_else(|| match p.id() {
                "zai-coding-plan" => vec!["ZAI_API_KEY".into()],
                _ => Vec::new(),
            }),
        });
    }
    out.sort_by(|a, b| rank(&a.id).cmp(&rank(&b.id)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    out
}

pub fn save_key(id: &str, key: &str) -> Result<()> {
    let key = key.trim();
    if key.is_empty() {
        bail!("empty key");
    }
    match id {
        "zai-coding-plan" => codeit_providers::zai::login(key),
        _ => codeit_providers::auth::set(id, serde_json::json!({ "type": "api", "key": key })),
    }
}

/// Removes codeit's login for a provider. Returns false when it had none.
pub fn logout(id: &str) -> Result<bool> {
    match id {
        "copilot" => codeit_providers::copilot::logout(),
        "zai-coding-plan" => codeit_providers::zai::logout(),
        _ => codeit_providers::auth::remove(id),
    }
}

/// A login in progress: open `url` (and enter `code` there, when there is one), then await `wait`.
pub struct Started {
    pub url: String,
    pub code: String,
    pub wait: Pin<Box<dyn Future<Output = Result<()>> + Send>>,
}

/// Starts a login that happens in the browser. `enterprise` is the GitHub Enterprise domain.
pub async fn start(method: Method, enterprise: Option<&str>) -> Result<Started> {
    Ok(match method {
        Method::Copilot | Method::CopilotEnterprise => {
            let flow = codeit_providers::copilot::login(enterprise).await?;
            let (url, code) = (flow.login.url.clone(), flow.login.code.clone());
            Started { url, code, wait: Box::pin(flow.wait()) }
        }
        Method::ChatGptBrowser | Method::ChatGptDevice => {
            let flow = if method == Method::ChatGptBrowser {
                codeit_providers::chatgpt::browser().await?
            } else {
                codeit_providers::chatgpt::device().await?
            };
            let (url, code) = (flow.login.url.clone(), flow.login.code.clone());
            Started { url, code, wait: Box::pin(flow.wait()) }
        }
        Method::ApiKey | Method::Own => bail!("{} isn't a browser login", method.label()),
    })
}
