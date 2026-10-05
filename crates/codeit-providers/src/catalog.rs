//! Every provider in the models.dev catalog, the list opencode logs in to: OpenAI, Anthropic,
//! Google, OpenRouter, xAI, Groq, DeepSeek, Mistral, Moonshot, opencode Zen, local servers and
//! about two hundred more. Each needs an API key (`/login <provider>`, or its environment
//! variable); OpenAI also takes a ChatGPT Plus/Pro login (see [`crate::chatgpt`]).
//!
//! The catalog is cached in `~/.cache/codeit/models.json` and refreshed once a day. Each provider is
//! spoken to in its API's shape: OpenAI's /chat/completions (most of them), OpenAI's /responses
//! (OpenAI itself) or Anthropic's /v1/messages.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::copilot::models::{CopilotModel, Endpoint};
use crate::paths::debug;
use crate::{AuthStatus, ChatEvent, ChatRequest, ModelInfo, Provider, protocols};

const URL: &str = "https://models.dev/api.json";
/// Providers codeit talks to natively (their own login and quirks).
const NATIVE: &[&str] = &["github-copilot", "zai-coding-plan"];

fn cache_file() -> PathBuf {
    crate::cache_dir().join("models.json")
}

fn cached() -> Option<(Value, Duration)> {
    let path = cache_file();
    let age = std::fs::metadata(&path).ok()?.modified().ok()?.elapsed().unwrap_or_default();
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
    v.is_object().then_some((v, age))
}

/// Downloads the catalog into the cache.
pub async fn refresh() -> Result<()> {
    let res = crate::http()
        .get(URL)
        .header("user-agent", format!("codeit/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .send()
        .await?;
    if !res.status().is_success() {
        bail!("models.dev answered {}", res.status());
    }
    let text = res.text().await?;
    let v: Value = serde_json::from_str(&text)?;
    if !v.is_object() {
        bail!("models.dev sent no catalog");
    }
    let path = cache_file();
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, text)?;
    Ok(())
}

/// The catalog: the cached one, downloaded first when there is none (on a thread of its own,
/// so this works inside or outside an async runtime). A stale cache is refreshed for next time.
fn catalog() -> Value {
    if std::env::var_os("CODEIT_NO_CATALOG").is_some() {
        return json!({});
    }
    match cached() {
        Some((v, age)) => {
            if age > Duration::from_secs(24 * 3600) {
                std::thread::spawn(|| block_on(refresh()));
            }
            v
        }
        None => {
            let _ = std::thread::spawn(|| block_on(refresh())).join();
            cached().map(|c| c.0).unwrap_or_else(|| json!({}))
        }
    }
}

fn block_on<F: std::future::Future<Output = Result<()>>>(f: F) {
    if let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build()
        && let Err(e) = rt.block_on(f)
    {
        debug("catalog.refresh", format!("{e:#}"));
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Wire {
    Chat,
    Responses,
    Messages,
}

/// Where a provider's API is when the catalog names only its SDK.
fn default_base(npm: &str) -> Option<(Wire, &'static str)> {
    Some(match npm {
        "@ai-sdk/openai" => (Wire::Responses, "https://api.openai.com/v1"),
        "@ai-sdk/anthropic" => (Wire::Messages, "https://api.anthropic.com/v1"),
        "@ai-sdk/google" => (Wire::Chat, "https://generativelanguage.googleapis.com/v1beta/openai"),
        "@ai-sdk/xai" => (Wire::Chat, "https://api.x.ai/v1"),
        "@ai-sdk/groq" => (Wire::Chat, "https://api.groq.com/openai/v1"),
        "@ai-sdk/mistral" => (Wire::Chat, "https://api.mistral.ai/v1"),
        "@ai-sdk/cerebras" => (Wire::Chat, "https://api.cerebras.ai/v1"),
        "@ai-sdk/togetherai" => (Wire::Chat, "https://api.together.xyz/v1"),
        "@ai-sdk/deepinfra" => (Wire::Chat, "https://api.deepinfra.com/v1/openai"),
        "@ai-sdk/perplexity" => (Wire::Chat, "https://api.perplexity.ai"),
        "@ai-sdk/gateway" => (Wire::Chat, "https://ai-gateway.vercel.sh/v1"),
        _ => return None,
    })
}

#[derive(Clone)]
struct Model {
    info: ModelInfo,
    max_output: Option<u64>,
}

pub struct CatalogProvider {
    id: &'static str,
    name: &'static str,
    wire: Wire,
    base: String,
    env: Vec<String>,
    /// A server on this machine: no key needed.
    local: bool,
    models: Vec<Model>,
    /// Where to get an API key, for the login hint.
    pub doc: Option<String>,
}

fn leak(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

fn is_local(base: &str) -> bool {
    base.contains("://127.0.0.1") || base.contains("://localhost") || base.contains("://0.0.0.0")
}

fn efforts(wire: Wire, reasoning: bool) -> Vec<String> {
    if !reasoning {
        return Vec::new();
    }
    match wire {
        Wire::Messages => vec!["high".into(), "max".into()],
        _ => vec!["low".into(), "medium".into(), "high".into()],
    }
}

fn parse(id: &str, p: &Value) -> Option<CatalogProvider> {
    let npm = p["npm"].as_str().unwrap_or("");
    let api = p["api"].as_str().map(|a| a.trim_end_matches('/').to_string());
    let (wire, base) = match (default_base(npm), api) {
        // OpenAI-shaped SDKs pointed at another host speak /chat/completions.
        (Some((Wire::Responses, _)), Some(api)) if id != "openai" => (Wire::Chat, api),
        (Some((w, _)), Some(api)) => (w, api),
        (Some((w, base)), None) => (w, base.to_string()),
        (None, Some(api)) => (Wire::Chat, api),
        (None, None) => return None,
    };
    let pid = leak(id);
    let mut models: Vec<Model> = p["models"]
        .as_object()?
        .values()
        .filter(|m| m["tool_call"].as_bool() != Some(false) && m["status"] != "deprecated")
        .map(|m| {
            let mid = m["id"].as_str().unwrap_or_default();
            let reasoning = m["reasoning"].as_bool().unwrap_or(false);
            let vision = m["modalities"]["input"].as_array().is_some_and(|a| a.iter().any(|x| x == "image"));
            Model {
                info: ModelInfo {
                    provider: pid,
                    id: mid.to_string(),
                    name: m["name"].as_str().unwrap_or(mid).to_string(),
                    context: m["limit"]["context"].as_u64().filter(|n| *n > 0),
                    max_input: m["limit"]["input"].as_u64().filter(|n| *n > 0),
                    efforts: efforts(wire, reasoning),
                    default_effort: None,
                    vision,
                },
                max_output: m["limit"]["output"].as_u64().filter(|n| *n > 0),
            }
        })
        .collect();
    // Newest first.
    models.sort_by(|a, b| b.info.id.cmp(&a.info.id));
    Some(CatalogProvider {
        id: pid,
        name: leak(p["name"].as_str().unwrap_or(id)),
        wire,
        local: is_local(&base),
        base,
        env: p["env"].as_array().into_iter().flatten().filter_map(|e| e.as_str().map(String::from)).collect(),
        models,
        doc: p["doc"].as_str().map(String::from),
    })
}

/// The catalog's providers codeit can talk to, plus Ollama (not in the catalog).
pub fn providers() -> Vec<Arc<CatalogProvider>> {
    let c = catalog();
    let mut out: Vec<Arc<CatalogProvider>> = c
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(id, _)| !NATIVE.contains(&id.as_str()))
        .filter_map(|(id, p)| parse(id, p).map(Arc::new))
        .collect();
    if !out.iter().any(|p| p.id == "ollama") {
        out.push(Arc::new(CatalogProvider {
            id: "ollama",
            name: "Ollama (local)",
            wire: Wire::Chat,
            base: std::env::var("OLLAMA_HOST")
                .map(|h| format!("{}/v1", h.trim_end_matches('/')))
                .unwrap_or_else(|_| "http://127.0.0.1:11434/v1".into()),
            env: Vec::new(),
            local: true,
            models: Vec::new(),
            doc: Some("https://ollama.com".into()),
        }));
    }
    out.sort_by_key(|p| p.id);
    out
}

impl CatalogProvider {
    /// The key and where it came from: codeit's or opencode's login, then the environment.
    fn key(&self) -> Option<(String, String)> {
        if let Some((k, from)) = crate::auth::api_key(self.id) {
            return Some((k, from.to_string()));
        }
        self.env
            .iter()
            .filter(|e| e.ends_with("_KEY") || e.ends_with("_TOKEN"))
            .find_map(|e| std::env::var(e).ok().filter(|v| !v.trim().is_empty()).map(|v| (v, e.clone())))
    }

    /// The environment variables that hold this provider's key.
    pub fn env_keys(&self) -> Vec<String> {
        self.env.iter().filter(|e| e.ends_with("_KEY") || e.ends_with("_TOKEN")).cloned().collect()
    }

    pub fn is_local(&self) -> bool {
        self.local
    }

    fn model(&self, id: &str) -> Option<&Model> {
        self.models.iter().find(|m| m.info.id == id)
    }

    /// The model's limits, in the shape the request builders take.
    fn limits(&self, id: &str) -> CopilotModel {
        let m = self.model(id);
        let max_output = m.and_then(|m| m.max_output).map(|o| o.min(64_000));
        CopilotModel {
            id: id.to_string(),
            name: id.to_string(),
            endpoint: Endpoint::Chat,
            context: m.and_then(|m| m.info.context),
            max_input: m.and_then(|m| m.info.max_input),
            max_output,
            efforts: m.map(|m| m.info.efforts.clone()).unwrap_or_default(),
            adaptive_thinking: false,
            max_thinking_budget: max_output.map(|o| o.min(32_000)),
            picker_enabled: true,
            vision: m.is_some_and(|m| m.info.vision),
        }
    }

    /// Whether the local server answers (a quick connect, so pickers stay fast).
    async fn running(&self) -> bool {
        let host = self.base.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or("");
        let addr = if host.contains(':') { host.to_string() } else { format!("{host}:80") };
        let addr = addr.replace("localhost", "127.0.0.1");
        matches!(
            tokio::time::timeout(Duration::from_millis(300), tokio::net::TcpStream::connect(addr)).await,
            Ok(Ok(_))
        )
    }

    async fn live_models(&self) -> Result<Vec<ModelInfo>> {
        let res = crate::http().get(format!("{}/models", self.base)).timeout(Duration::from_secs(5)).send().await?;
        let v: Value = res.json().await?;
        Ok(v["data"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| m["id"].as_str())
            .map(|id| ModelInfo {
                provider: self.id,
                id: id.to_string(),
                name: id.to_string(),
                context: None,
                max_input: None,
                efforts: Vec::new(),
                default_effort: None,
                vision: false,
            })
            .collect())
    }
}

#[async_trait]
impl Provider for CatalogProvider {
    fn id(&self) -> &'static str {
        self.id
    }

    fn name(&self) -> &'static str {
        self.name
    }

    async fn status(&self) -> AuthStatus {
        if self.id == "openai"
            && let Some((v, from)) = crate::auth::get("openai")
            && v["type"] == "oauth"
        {
            return AuthStatus::LoggedIn(format!("ChatGPT Plus/Pro (from {from})"));
        }
        match self.key() {
            Some((_, from)) => AuthStatus::LoggedIn(format!("API key (from {from})")),
            None if self.local && self.running().await => {
                AuthStatus::LoggedIn(format!("local server at {}", self.base))
            }
            None if self.local => AuthStatus::LoggedOut(format!("not running at {}", self.base)),
            None => AuthStatus::LoggedOut(format!("no key; run `/login {}`", self.id)),
        }
    }

    async fn models(&self) -> Result<Vec<ModelInfo>> {
        if self.id == "openai" && crate::chatgpt::logged_in() {
            return Ok(self.models.iter().map(|m| m.info.clone()).filter(|m| crate::chatgpt::allowed(&m.id)).collect());
        }
        if self.key().is_none() && !self.local {
            return Ok(Vec::new());
        }
        if self.local {
            // What the server has loaded, when it is running.
            if !self.running().await {
                return Ok(Vec::new());
            }
            let live = self.live_models().await.unwrap_or_default();
            if !live.is_empty() || self.models.is_empty() {
                return Ok(live);
            }
        }
        Ok(self.models.iter().map(|m| m.info.clone()).collect())
    }

    async fn chat(&self, req: ChatRequest, events: UnboundedSender<ChatEvent>) -> Result<()> {
        if self.id == "openai" && crate::chatgpt::logged_in() {
            return crate::chatgpt::chat(req, events).await;
        }
        let key = self.key().map(|k| k.0);
        if key.is_none() && !self.local {
            bail!("No {} key. Run `/login {}`.", self.name, self.id);
        }
        let limits = self.limits(&req.model);
        let (url, body) = match self.wire {
            Wire::Chat => {
                let mut body = protocols::chat_body(&req, self.id);
                if let Some(o) = limits.max_output {
                    body["max_tokens"] = json!(o);
                }
                (format!("{}/chat/completions", self.base), body)
            }
            Wire::Responses => {
                (format!("{}/responses", self.base), protocols::responses_body(&req, self.id, Some(&limits)))
            }
            Wire::Messages => {
                (format!("{}/messages", self.base), protocols::messages_body(&req, self.id, Some(&limits)))
            }
        };
        debug("catalog.request", format!("provider={} model={} wire={:?}", self.id, req.model, self.wire));
        if std::env::var_os("CODEIT_DEBUG_BODY").is_some() {
            debug("catalog.request.body", &body);
        }
        let mut attempt = 0;
        let res = loop {
            attempt += 1;
            let mut r = crate::http()
                .post(&url)
                .header("user-agent", format!("codeit/{}", env!("CARGO_PKG_VERSION")))
                .header("accept", "text/event-stream")
                .json(&body);
            r = match (self.wire, &key) {
                (Wire::Messages, Some(k)) => r.header("x-api-key", k).header("anthropic-version", "2023-06-01"),
                (Wire::Messages, None) => r.header("anthropic-version", "2023-06-01"),
                (_, Some(k)) => r.bearer_auth(k),
                (_, None) => r,
            };
            if self.id == "openrouter" {
                r = r.header("X-Title", "codeit");
            }
            let res = r.send().await.map_err(|e| anyhow!("{}: {e}", self.name))?;
            let status = res.status().as_u16();
            if (status == 429 || status >= 500) && attempt < 3 {
                debug("catalog.request.retry", status);
                tokio::time::sleep(Duration::from_millis(1000 << attempt)).await;
                continue;
            }
            break res;
        };
        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            debug("catalog.request.http-error", format!("{status} {}", crate::snippet(&text)));
            let hint = match status.as_u16() {
                401 | 403 => format!(" The key was rejected; run `/login {}` with a current one.", self.id),
                429 => " You are being rate limited, or your credit ran out.".into(),
                _ => String::new(),
            };
            bail!("{} {status}: {}{hint}", self.name, crate::snippet(&text));
        }
        match self.wire {
            Wire::Chat => protocols::chat_stream(res, &events).await,
            Wire::Responses => protocols::responses_stream(res, &events).await,
            Wire::Messages => protocols::messages_stream(res, &events).await,
        }
    }
}

/// When the catalog was last downloaded, for `codeit status`.
pub fn age() -> Option<SystemTime> {
    std::fs::metadata(cache_file()).ok()?.modified().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_api_shape() {
        let openai = json!({"npm": "@ai-sdk/openai", "name": "OpenAI", "env": ["OPENAI_API_KEY"], "models": {
            "gpt-x": {"id": "gpt-x", "name": "GPT X", "reasoning": true, "tool_call": true, "modalities": {"input": ["text", "image"]}, "limit": {"context": 400000, "output": 128000}},
            "emb": {"id": "emb", "name": "Embedding", "tool_call": false}
        }});
        let p = parse("openai", &openai).unwrap();
        assert_eq!((p.wire, p.base.as_str()), (Wire::Responses, "https://api.openai.com/v1"));
        assert_eq!(p.models.len(), 1);
        assert!(p.models[0].info.vision);
        assert_eq!(p.env_keys(), vec!["OPENAI_API_KEY".to_string()]);

        let compat = json!({"npm": "@ai-sdk/openai-compatible", "api": "https://api.deepseek.com/", "models": {}});
        let p = parse("deepseek", &compat).unwrap();
        assert_eq!((p.wire, p.base.as_str()), (Wire::Chat, "https://api.deepseek.com"));

        let claude = json!({"npm": "@ai-sdk/anthropic", "models": {"c": {"id": "c", "reasoning": true, "tool_call": true, "limit": {"output": 64000}}}});
        let p = parse("anthropic", &claude).unwrap();
        assert_eq!(p.wire, Wire::Messages);
        assert_eq!(p.models[0].info.efforts, vec!["high", "max"]);

        assert!(parse("azure", &json!({"npm": "@ai-sdk/azure", "models": {}})).is_none());
        let local = parse(
            "lmstudio",
            &json!({"npm": "@ai-sdk/openai-compatible", "api": "http://127.0.0.1:1234/v1", "models": {}}),
        )
        .unwrap();
        assert!(local.is_local());
    }
}
