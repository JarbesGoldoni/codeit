//! Z.ai's GLM Coding Plan (`zai-coding-plan`, as in opencode): an OpenAI-compatible
//! /chat/completions API with the API key of a coding plan subscription. (Z.ai's pay-as-you-go
//! API is the catalog's `zai`.)
//!
//! The key comes from `ZAI_API_KEY`, codeit's login (`/login zai-coding-plan`), or opencode's.
//! Effort is GLM's thinking switch: `on` (default) or `off`.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::paths::debug;
use crate::protocols;
use crate::{AuthStatus, ChatEvent, ChatRequest, ModelInfo, Part, Provider, Role};

const BASE: &str = "https://api.z.ai/api/coding/paas/v4";
pub const ID: &str = "zai-coding-plan";

pub struct Zai;

/// Where codeit kept the key before it moved to auth.json.
fn legacy_file() -> PathBuf {
    crate::data_dir().join("zai.json")
}

/// The key and where it came from.
fn key() -> Option<(String, &'static str)> {
    if let Ok(k) = std::env::var("ZAI_API_KEY")
        && !k.trim().is_empty()
    {
        return Some((k.trim().to_string(), "ZAI_API_KEY"));
    }
    if let Some(k) = crate::auth::api_key(ID) {
        return Some(k);
    }
    let v: Value = serde_json::from_str(&std::fs::read_to_string(legacy_file()).ok()?).ok()?;
    v["key"].as_str().filter(|k| !k.is_empty()).map(|k| (k.to_string(), "codeit login"))
}

/// Saves the key (readable only by you).
pub fn login(key: &str) -> Result<()> {
    let key = key.trim();
    if key.is_empty() {
        bail!("empty key");
    }
    crate::auth::set(ID, json!({ "type": "api", "key": key }))?;
    let _ = std::fs::remove_file(legacy_file());
    Ok(())
}

/// Removes codeit's saved key. Returns false when there was none.
pub fn logout() -> Result<bool> {
    let legacy = std::fs::remove_file(legacy_file()).is_ok();
    Ok(crate::auth::remove(ID)? || legacy)
}

fn token() -> Result<String> {
    key().map(|k| k.0).ok_or_else(|| anyhow!("No Z.ai Coding Plan key. Run `/login zai-coding-plan`."))
}

/// What the API's model list doesn't say: names and context sizes.
fn info(id: &str) -> ModelInfo {
    let name = id
        .split('-')
        .map(|p| if p == "glm" { "GLM".to_string() } else { p[..1].to_uppercase() + &p[1..] })
        .collect::<Vec<_>>()
        .join("-");
    let context = if id.starts_with("glm-4.5") { 128_000 } else { 200_000 };
    ModelInfo {
        provider: ID,
        id: id.to_string(),
        name,
        context: Some(context),
        max_input: None,
        efforts: vec!["off".into(), "on".into()],
        default_effort: Some("on".into()),
        // The coding plan serves the text models only.
        vision: false,
    }
}

/// The OpenAI body, with GLM's thinking switch and its reasoning replayed (preserved thinking).
pub(crate) fn body(req: &ChatRequest) -> Value {
    let mut body = protocols::chat_body(req, ID);
    if let Some(o) = body.as_object_mut() {
        o.remove("reasoning_effort");
    }
    let on = req.effort.as_deref() != Some("off");
    body["thinking"] = json!({ "type": if on { "enabled" } else { "disabled" } });
    // Each assistant message is one body message, in order.
    let mut ours = req.messages.iter().filter(|m| m.role == Role::Assistant);
    if let Some(list) = body["messages"].as_array_mut() {
        for msg in list.iter_mut().filter(|m| m["role"] == "assistant") {
            let Some(m) = ours.next() else { break };
            if !req.same_model(ID, m) {
                continue;
            }
            let reasoning: String = m
                .parts
                .iter()
                .filter_map(|p| if let Part::Reasoning { text, .. } = p { Some(text.as_str()) } else { None })
                .collect();
            if !reasoning.is_empty() {
                msg["reasoning_content"] = json!(reasoning);
            }
        }
    }
    body
}

#[async_trait]
impl Provider for Zai {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "Z.ai Coding Plan"
    }

    async fn status(&self) -> AuthStatus {
        match key() {
            Some((_, from)) => AuthStatus::LoggedIn(format!("API key (from {from})")),
            None => AuthStatus::LoggedOut(format!("no key; run `/login {ID}`")),
        }
    }

    async fn models(&self) -> Result<Vec<ModelInfo>> {
        let key = token()?;
        let res = crate::http()
            .get(format!("{BASE}/models"))
            .bearer_auth(&key)
            .header("user-agent", format!("codeit/{}", env!("CARGO_PKG_VERSION")))
            .send()
            .await?;
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("Z.ai {status}: {}", crate::snippet(&text));
        }
        let v: Value = serde_json::from_str(&text)?;
        let mut ids: Vec<&str> = v["data"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str()).collect();
        // Newest first.
        ids.reverse();
        Ok(ids.into_iter().map(info).collect())
    }

    async fn chat(&self, req: ChatRequest, events: UnboundedSender<ChatEvent>) -> Result<()> {
        let key = token()?;
        let body = body(&req);
        debug("zai.request", format!("model={} tools={}", req.model, req.tools.len()));
        if std::env::var_os("CODEIT_DEBUG_BODY").is_some() {
            debug("zai.request.body", &body);
        }
        let mut attempt = 0;
        let res = loop {
            attempt += 1;
            let res = crate::http()
                .post(format!("{BASE}/chat/completions"))
                .bearer_auth(&key)
                .header("user-agent", format!("codeit/{}", env!("CARGO_PKG_VERSION")))
                .header("accept", "text/event-stream")
                .json(&body)
                .send()
                .await?;
            let status = res.status().as_u16();
            if (status == 429 || status >= 500) && attempt < 3 {
                debug("zai.request.retry", status);
                tokio::time::sleep(Duration::from_millis(1000 << attempt)).await;
                continue;
            }
            break res;
        };
        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            debug("zai.request.http-error", format!("{status} {}", crate::snippet(&text)));
            let hint = match status.as_u16() {
                401 => " The key was rejected; run `/login zai-coding-plan` with a current one.",
                429 => " You've hit the coding plan's limit for now (it resets every few hours).",
                _ => "",
            };
            bail!("Z.ai {status}: {}{hint}", crate::snippet(&text));
        }
        protocols::chat_stream(res, &events).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Message;

    #[test]
    fn switches_thinking_and_replays_reasoning() {
        let mut a = Message::assistant("hi");
        a.parts.insert(0, Part::Reasoning { text: "hmm".into(), meta: None });
        a.model = Some("zai-coding-plan/glm-5.1".into());
        let mut req = ChatRequest {
            model: "glm-5.1".into(),
            messages: vec![Message::user("a"), a, Message::user("b")],
            effort: Some("off".into()),
            ..Default::default()
        };
        let b = body(&req);
        assert_eq!(b["thinking"]["type"], "disabled");
        assert!(b.get("reasoning_effort").is_none());
        assert_eq!(b["messages"][1]["reasoning_content"], "hmm");
        req.model = "glm-4.7".into();
        req.effort = None;
        let b = body(&req);
        assert_eq!(b["thinking"]["type"], "enabled");
        assert!(b["messages"][1].get("reasoning_content").is_none());
        assert_eq!(info("glm-5.3-flash").name, "GLM-5.3-Flash");
    }
}
