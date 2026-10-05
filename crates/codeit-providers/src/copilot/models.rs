//! Copilot's model list (`GET /models`), filtered the way opencode does.

use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::Value;

/// Which API a model is served on. Copilot reports this per model in `supported_endpoints`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endpoint {
    /// OpenAI `/chat/completions`.
    Chat,
    /// OpenAI `/responses` (GPT-5 class).
    Responses,
    /// Anthropic `/v1/messages` (Claude).
    Messages,
}

#[derive(Clone, Debug)]
pub struct CopilotModel {
    pub id: String,
    pub name: String,
    pub endpoint: Endpoint,
    pub context: Option<u64>,
    pub max_input: Option<u64>,
    pub max_output: Option<u64>,
    pub efforts: Vec<String>,
    pub adaptive_thinking: bool,
    pub max_thinking_budget: Option<u64>,
    pub picker_enabled: bool,
    pub vision: bool,
}

/// Same rule as opencode when Copilot doesn't say: GPT-5+ (not mini) on Responses, else Chat.
pub fn guess_endpoint(id: &str) -> Endpoint {
    if id.starts_with("claude-") {
        return Endpoint::Messages;
    }
    let major = id
        .strip_prefix("gpt-")
        .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|n| n.parse::<u32>().ok());
    match major {
        Some(n) if n >= 5 && !id.starts_with("gpt-5-mini") => Endpoint::Responses,
        _ => Endpoint::Chat,
    }
}

pub fn parse(item: &Value) -> Option<CopilotModel> {
    let id = item["id"].as_str()?;
    let caps = &item["capabilities"];
    let limits = &caps["limits"];
    let supports = &caps["supports"];
    // Not usable: disabled by policy, or missing what a chat needs.
    if item["policy"]["state"] == "disabled"
        || limits["max_output_tokens"].as_u64().is_none()
        || limits["max_prompt_tokens"].as_u64().is_none()
        || supports["tool_calls"].as_bool().is_none()
    {
        return None;
    }
    let eps: Vec<&str> =
        item["supported_endpoints"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let endpoint = if eps.contains(&"/v1/messages") {
        Endpoint::Messages
    } else if eps.contains(&"/responses") {
        Endpoint::Responses
    } else if eps.contains(&"/chat/completions") {
        Endpoint::Chat
    } else {
        guess_endpoint(id)
    };
    let adaptive_thinking = supports["adaptive_thinking"].as_bool().unwrap_or(false);
    let max_thinking_budget = supports["max_thinking_budget"].as_u64();
    let mut efforts: Vec<String> = supports["reasoning_effort"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    // On /v1/messages without adaptive thinking, effort means a thinking budget: offer high and max.
    if endpoint == Endpoint::Messages && !(adaptive_thinking && !efforts.is_empty()) {
        efforts = if max_thinking_budget.is_some() { vec!["high".into(), "max".into()] } else { Vec::new() };
    }
    Some(CopilotModel {
        id: id.into(),
        name: item["name"].as_str().unwrap_or(id).into(),
        endpoint,
        context: limits["max_context_window_tokens"].as_u64().or(limits["max_prompt_tokens"].as_u64()),
        max_input: limits["max_prompt_tokens"].as_u64(),
        max_output: limits["max_output_tokens"].as_u64(),
        efforts,
        adaptive_thinking,
        max_thinking_budget,
        picker_enabled: item["model_picker_enabled"].as_bool().unwrap_or(false),
        vision: supports["vision"].as_bool().unwrap_or(false)
            || limits["vision"]["supported_media_types"]
                .as_array()
                .is_some_and(|a| a.iter().any(|t| t.as_str().is_some_and(|t| t.starts_with("image/")))),
    })
}

pub async fn fetch(base: &str, headers: reqwest::header::HeaderMap) -> Result<Vec<CopilotModel>> {
    let res =
        crate::http().get(format!("{base}/models")).headers(headers).timeout(Duration::from_secs(15)).send().await?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if status.as_u16() == 401 {
        bail!(
            "Copilot rejected the GitHub token (401). Log in again with `/login copilot` (or `codeit login copilot`)."
        );
    }
    if !status.is_success() {
        bail!("Copilot /models {status}: {}", crate::snippet(&text));
    }
    let body: Value = serde_json::from_str(&text)?;
    Ok(body["data"].as_array().into_iter().flatten().filter_map(parse).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(id: &str, eps: &[&str], supports: Value) -> Value {
        json!({
            "id": id, "name": id.to_uppercase(), "model_picker_enabled": true, "version": id,
            "supported_endpoints": eps,
            "capabilities": { "family": id, "limits": { "max_output_tokens": 64000, "max_prompt_tokens": 128000 }, "supports": supports },
        })
    }

    #[test]
    fn picks_endpoint_and_efforts() {
        let claude = parse(&item(
            "claude-x",
            &["/v1/messages", "/chat/completions"],
            json!({"tool_calls": true, "max_thinking_budget": 32000}),
        ))
        .unwrap();
        assert_eq!(claude.endpoint, Endpoint::Messages);
        assert_eq!(claude.efforts, ["high", "max"]);
        let gpt =
            parse(&item("gpt-5.4", &["/responses"], json!({"tool_calls": true, "reasoning_effort": ["low", "high"]})))
                .unwrap();
        assert_eq!(gpt.endpoint, Endpoint::Responses);
        assert_eq!(gpt.efforts, ["low", "high"]);
        assert!(parse(&item("embed", &[], json!({}))).is_none(), "no tool_calls: not a chat model");
    }

    #[test]
    fn guesses_endpoint() {
        assert_eq!(guess_endpoint("gpt-5.4"), Endpoint::Responses);
        assert_eq!(guess_endpoint("gpt-5-mini"), Endpoint::Chat);
        assert_eq!(guess_endpoint("gpt-4.1"), Endpoint::Chat);
    }
}
