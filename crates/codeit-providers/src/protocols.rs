//! Request bodies and stream readers for the three API shapes codeit speaks: OpenAI's
//! /chat/completions and /responses, and Anthropic's /v1/messages. Copilot serves models on all
//! three; the catalog providers use whichever their API is.
//!
//! Each converts codeit's messages (text, reasoning, tool calls, tool results) into its API's
//! shape, and turns the streamed reply back into [`ChatEvent`]s.

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::copilot::models::CopilotModel;
use crate::{ChatEvent, ChatRequest, Message, Part, Role, ToolCall, Usage, parse_args, sse};

fn system(req: &ChatRequest) -> Option<&str> {
    req.system.as_deref().filter(|s| !s.trim().is_empty())
}

fn error_message(v: &Value) -> Option<String> {
    let e = &v["error"];
    e["message"].as_str().map(String::from).or_else(|| e.as_str().map(String::from))
}

fn args_string(input: &Value) -> String {
    match input {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Reasoning meta of this message, if it may be replayed to the requested model.
fn meta<'a>(req: &ChatRequest, provider: &str, m: &'a Message, key: &str) -> Option<&'a Value> {
    if !req.same_model(provider, m) {
        return None;
    }
    m.parts.iter().find_map(|p| match p {
        Part::Reasoning { meta: Some(meta), .. } if meta.get(key).is_some() => Some(meta),
        _ => None,
    })
}

/// Streams tool calls whose arguments arrive in pieces, keyed by the API's index.
#[derive(Default)]
struct PendingTools(Vec<(String, String, String)>);

impl PendingTools {
    fn start(&mut self, events: &UnboundedSender<ChatEvent>, id: &str, name: &str) -> usize {
        let _ = events.send(ChatEvent::ToolStart { id: id.into(), name: name.into() });
        self.0.push((id.into(), name.into(), String::new()));
        self.0.len() - 1
    }
    fn flush(&mut self, events: &UnboundedSender<ChatEvent>) {
        for (id, name, args) in self.0.drain(..) {
            let _ = events.send(ChatEvent::ToolCall(ToolCall { id, name, input: parse_args(&args) }));
        }
    }
}

// ── OpenAI /chat/completions ─────────────────────────────────────────────────

/// `provider` is the id the reasoning was recorded under (it is replayed only to the same model).
pub fn chat_body(req: &ChatRequest, provider: &str) -> Value {
    let mut messages = Vec::new();
    if let Some(s) = system(req) {
        messages.push(json!({ "role": "system", "content": s }));
    }
    for m in &req.messages {
        match m.role {
            Role::User => {
                // Tool results are separate `tool` messages, before the user's own text.
                for p in &m.parts {
                    if let Part::ToolResult { id, content, .. } = p {
                        messages.push(json!({ "role": "tool", "tool_call_id": id, "content": content }));
                    }
                }
                let text = m.text();
                let images: Vec<Value> = m
                    .images()
                    .map(
                        |(t, d)| json!({ "type": "image_url", "image_url": { "url": format!("data:{t};base64,{d}") } }),
                    )
                    .collect();
                if !images.is_empty() {
                    let mut content =
                        vec![json!({ "type": "text", "text": if text.is_empty() { "(image)" } else { &text } })];
                    content.extend(images);
                    messages.push(json!({ "role": "user", "content": content }));
                } else if !text.is_empty() {
                    messages.push(json!({ "role": "user", "content": text }));
                }
            }
            Role::Assistant => {
                let text = m.text();
                let calls: Vec<Value> = m
                    .tool_calls()
                    .map(|c| {
                        json!({ "id": c.id, "type": "function",
                                "function": { "name": c.name, "arguments": args_string(&c.input) } })
                    })
                    .collect();
                let mut msg =
                    json!({ "role": "assistant", "content": if text.is_empty() { Value::Null } else { json!(text) } });
                if !calls.is_empty() {
                    msg["tool_calls"] = Value::Array(calls);
                }
                if let Some(meta) = meta(req, provider, m, "reasoning_opaque") {
                    msg["reasoning_opaque"] = meta["reasoning_opaque"].clone();
                    let reasoning: String = m
                        .parts
                        .iter()
                        .filter_map(|p| if let Part::Reasoning { text, .. } = p { Some(text.as_str()) } else { None })
                        .collect();
                    msg["reasoning_text"] = json!(reasoning);
                }
                messages.push(msg);
            }
        }
    }
    // Like the Copilot CLI and opencode, no max_tokens: Copilot applies the model's own limit.
    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| json!({ "type": "function", "function": { "name": t.name, "description": t.description, "parameters": t.parameters } }))
            .collect();
    }
    if let Some(e) = &req.effort {
        body["reasoning_effort"] = json!(e);
    }
    body
}

pub async fn chat_stream(res: reqwest::Response, events: &UnboundedSender<ChatEvent>) -> Result<()> {
    let mut tools = PendingTools::default();
    // The API's tool index -> position in `tools`.
    let mut index: Vec<(u64, usize)> = Vec::new();
    let mut opaque: Option<String> = None;
    sse::for_each_json(res, |_, v| {
        if let Some(msg) = error_message(&v) {
            return Err(anyhow!("{msg}"));
        }
        let choice = &v["choices"][0];
        let delta = &choice["delta"];
        // Copilot names it reasoning_text; other OpenAI-compatible servers use reasoning_content.
        if let Some(t) =
            delta["reasoning_text"].as_str().or(delta["reasoning_content"].as_str()).filter(|t| !t.is_empty())
        {
            let _ = events.send(ChatEvent::Reasoning(t.into()));
        }
        if let Some(o) = delta["reasoning_opaque"].as_str().filter(|o| !o.is_empty()) {
            opaque = Some(o.into());
        }
        if let Some(t) = delta["content"].as_str().filter(|t| !t.is_empty()) {
            let _ = events.send(ChatEvent::Text(t.into()));
        }
        for tc in delta["tool_calls"].as_array().into_iter().flatten() {
            let i = tc["index"].as_u64().unwrap_or(0);
            let pos = match index.iter().find(|(k, _)| *k == i) {
                Some((_, p)) => *p,
                None => {
                    let id = tc["id"].as_str().map(String::from).unwrap_or_else(|| format!("call_{i}"));
                    let p = tools.start(events, &id, tc["function"]["name"].as_str().unwrap_or_default());
                    index.push((i, p));
                    p
                }
            };
            if let Some(a) = tc["function"]["arguments"].as_str() {
                tools.0[pos].2.push_str(a);
            }
        }
        if choice["finish_reason"] == "length" {
            let _ = events.send(ChatEvent::Incomplete("output token limit".into()));
        } else if choice["finish_reason"] == "content_filter" {
            let _ = events.send(ChatEvent::Incomplete("content filter".into()));
        }
        let u = &v["usage"];
        if u.is_object() {
            let _ = events.send(ChatEvent::Usage(Usage {
                input_tokens: u["prompt_tokens"].as_u64().unwrap_or(0),
                output_tokens: u["completion_tokens"].as_u64().unwrap_or(0),
                cached_tokens: u["prompt_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0),
                credits: None,
            }));
        }
        Ok(true)
    })
    .await?;
    if let Some(o) = opaque {
        let _ = events.send(ChatEvent::ReasoningMeta(json!({ "reasoning_opaque": o })));
    }
    tools.flush(events);
    Ok(())
}

// ── OpenAI /responses ────────────────────────────────────────────────────────

pub fn responses_body(req: &ChatRequest, provider: &str, model: Option<&CopilotModel>) -> Value {
    let mut input: Vec<Value> = Vec::new();
    for m in &req.messages {
        match m.role {
            Role::User => {
                for p in &m.parts {
                    if let Part::ToolResult { id, content, .. } = p {
                        input.push(json!({ "type": "function_call_output", "call_id": id, "output": content }));
                    }
                }
                let text = m.text();
                let images: Vec<Value> = m
                    .images()
                    .map(|(t, d)| json!({ "type": "input_image", "image_url": format!("data:{t};base64,{d}") }))
                    .collect();
                if !images.is_empty() {
                    let mut content =
                        vec![json!({ "type": "input_text", "text": if text.is_empty() { "(image)" } else { &text } })];
                    content.extend(images);
                    input.push(json!({ "role": "user", "content": content }));
                } else if !text.is_empty() {
                    input.push(json!({ "role": "user", "content": text }));
                }
            }
            Role::Assistant => {
                let replay = req.same_model(provider, m);
                for p in &m.parts {
                    match p {
                        Part::Text { text } if !text.is_empty() => {
                            input.push(json!({ "role": "assistant", "content": text }))
                        }
                        // With store:false, reasoning comes back only with its encrypted state.
                        Part::Reasoning { text, meta: Some(meta) } if replay && meta["encrypted_content"].is_string() => {
                            let summary =
                                if text.is_empty() { json!([]) } else { json!([{ "type": "summary_text", "text": text }]) };
                            input.push(json!({ "type": "reasoning", "summary": summary,
                                               "encrypted_content": meta["encrypted_content"] }));
                        }
                        Part::ToolCall(c) => input.push(json!({
                            "type": "function_call", "call_id": c.id, "name": c.name, "arguments": args_string(&c.input),
                        })),
                        _ => {}
                    }
                }
            }
        }
    }
    let mut body = json!({ "model": req.model, "input": input, "stream": true, "store": false });
    if let Some(s) = system(req) {
        body["instructions"] = json!(s);
    }
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| json!({ "type": "function", "name": t.name, "description": t.description, "parameters": t.parameters, "strict": false }))
            .collect();
    }
    let reasons = req.effort.is_some() || model.is_some_and(|m| !m.efforts.is_empty());
    if reasons {
        let mut reasoning = json!({ "summary": "auto" });
        if let Some(e) = &req.effort {
            reasoning["effort"] = json!(e);
        }
        body["reasoning"] = reasoning;
        body["include"] = json!(["reasoning.encrypted_content"]);
    }
    body
}

pub async fn responses_stream(res: reqwest::Response, events: &UnboundedSender<ChatEvent>) -> Result<()> {
    sse::for_each_json(res, |event, v| {
        let kind = v["type"].as_str().or(event).unwrap_or("");
        match kind {
            "response.output_text.delta" => {
                if let Some(t) = v["delta"].as_str() {
                    let _ = events.send(ChatEvent::Text(t.into()));
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(t) = v["delta"].as_str() {
                    let _ = events.send(ChatEvent::Reasoning(t.into()));
                }
            }
            "response.reasoning_summary_part.done" => {
                let _ = events.send(ChatEvent::Reasoning("\n\n".into()));
            }
            "response.output_item.added" if v["item"]["type"] == "function_call" => {
                let item = &v["item"];
                let _ = events.send(ChatEvent::ToolStart {
                    id: item["call_id"].as_str().unwrap_or_default().into(),
                    name: item["name"].as_str().unwrap_or_default().into(),
                });
            }
            "response.output_item.done" => {
                let item = &v["item"];
                match item["type"].as_str() {
                    Some("function_call") => {
                        let _ = events.send(ChatEvent::ToolCall(ToolCall {
                            id: item["call_id"].as_str().unwrap_or_default().into(),
                            name: item["name"].as_str().unwrap_or_default().into(),
                            input: parse_args(item["arguments"].as_str().unwrap_or_default()),
                        }));
                    }
                    Some("reasoning") if item["encrypted_content"].is_string() => {
                        let _ = events
                            .send(ChatEvent::ReasoningMeta(json!({ "encrypted_content": item["encrypted_content"] })));
                    }
                    _ => {}
                }
            }
            "response.completed" | "response.incomplete" => {
                let u = &v["response"]["usage"];
                let _ = events.send(ChatEvent::Usage(Usage {
                    input_tokens: u["input_tokens"].as_u64().unwrap_or(0),
                    output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
                    cached_tokens: u["input_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0),
                    credits: None,
                }));
                if kind == "response.incomplete" {
                    let reason = v["response"]["incomplete_details"]["reason"].as_str().unwrap_or("unknown");
                    let _ = events.send(ChatEvent::Incomplete(reason.into()));
                }
                return Ok(false);
            }
            "response.failed" => {
                let msg = v["response"]["error"]["message"].as_str().unwrap_or("response failed");
                return Err(anyhow!("Copilot: {msg}"));
            }
            "error" => {
                let msg =
                    v["message"].as_str().map(String::from).or_else(|| error_message(&v)).unwrap_or(v.to_string());
                return Err(anyhow!("Copilot: {msg}"));
            }
            _ => {}
        }
        Ok(true)
    })
    .await
}

// ── Anthropic /v1/messages ───────────────────────────────────────────────────

pub fn messages_body(req: &ChatRequest, provider: &str, model: Option<&CopilotModel>) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    for m in &req.messages {
        let replay = req.same_model(provider, m);
        let mut blocks: Vec<Value> = Vec::new();
        // Tool results must come first in a user message.
        for p in &m.parts {
            if let Part::ToolResult { id, content, error } = p {
                let mut b = json!({ "type": "tool_result", "tool_use_id": id, "content": content });
                if *error {
                    b["is_error"] = json!(true);
                }
                blocks.push(b);
            }
        }
        for p in &m.parts {
            match p {
                Part::Text { text } if !text.is_empty() => blocks.push(json!({ "type": "text", "text": text })),
                Part::Image { media_type, data } => blocks.push(json!({
                    "type": "image", "source": { "type": "base64", "media_type": media_type, "data": data },
                })),
                // Thinking goes back only to the model that signed it.
                Part::Reasoning { text, meta: Some(meta) } if replay => {
                    if let Some(sig) = meta["signature"].as_str() {
                        blocks.push(json!({ "type": "thinking", "thinking": text, "signature": sig }));
                    } else if let Some(data) = meta["redacted"].as_str() {
                        blocks.push(json!({ "type": "redacted_thinking", "data": data }));
                    }
                }
                Part::ToolCall(c) => blocks.push(json!({
                    "type": "tool_use", "id": c.id, "name": c.name,
                    "input": if c.input.is_object() { c.input.clone() } else { json!({ "raw": c.input }) },
                })),
                _ => {}
            }
        }
        if blocks.is_empty() {
            blocks.push(
                json!({ "type": "text", "text": if m.role == Role::User { "Continue." } else { "(no response)" } }),
            );
        }
        let role = if m.role == Role::User { "user" } else { "assistant" };
        // Anthropic rejects two messages in a row with the same role: merge them.
        match messages.last_mut() {
            Some(last) if last["role"] == role => {
                let content = last["content"].as_array_mut().expect("content is an array");
                // Tool results lead the merged message too.
                let (results, rest): (Vec<Value>, Vec<Value>) =
                    blocks.into_iter().partition(|b| b["type"] == "tool_result");
                let at = content.iter().take_while(|b| b["type"] == "tool_result").count();
                content.splice(at..at, results);
                content.extend(rest);
            }
            _ => messages.push(json!({ "role": role, "content": blocks })),
        }
    }
    // Prompt caching: the system prompt and the last two messages, as opencode does.
    let n = messages.len();
    for m in messages.iter_mut().skip(n.saturating_sub(2)) {
        if let Some(last) = m["content"].as_array_mut().and_then(|c| c.last_mut()) {
            last["cache_control"] = json!({ "type": "ephemeral" });
        }
    }

    let mut max_tokens = model.and_then(|m| m.max_output).unwrap_or(32_000);
    let mut body = json!({ "model": req.model, "messages": messages, "stream": true });
    if let Some(s) = system(req) {
        body["system"] = json!([{ "type": "text", "text": s, "cache_control": { "type": "ephemeral" } }]);
    }
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| json!({ "name": t.name, "description": t.description, "input_schema": t.parameters }))
            .collect();
    }
    if let (Some(effort), Some(m)) = (&req.effort, model) {
        if m.adaptive_thinking {
            body["thinking"] = json!({ "type": "adaptive" });
            body["output_config"] = json!({ "effort": effort });
        } else if let Some(max) = m.max_thinking_budget {
            // Same budgets opencode uses: max = the whole budget, high = half of it.
            let budget = if effort == "max" { max.saturating_sub(1) } else { max / 2 };
            let budget = budget.min(max_tokens.saturating_sub(1024)).max(1024);
            max_tokens = max_tokens.max(budget + 1024);
            body["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
        }
    }
    body["max_tokens"] = json!(max_tokens);
    body
}

pub async fn messages_stream(res: reqwest::Response, events: &UnboundedSender<ChatEvent>) -> Result<()> {
    let mut usage = Usage::default();
    let mut tools = PendingTools::default();
    // Content block index -> position in `tools`.
    let mut index: Vec<(u64, usize)> = Vec::new();
    sse::for_each_json(res, |event, v| {
        match v["type"].as_str().or(event).unwrap_or("") {
            "message_start" => {
                let u = &v["message"]["usage"];
                usage.cached_tokens = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
                usage.input_tokens = u["input_tokens"].as_u64().unwrap_or(0)
                    + usage.cached_tokens
                    + u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
            }
            "content_block_start" => {
                let b = &v["content_block"];
                match b["type"].as_str() {
                    Some("tool_use") => {
                        let p = tools.start(
                            events,
                            b["id"].as_str().unwrap_or_default(),
                            b["name"].as_str().unwrap_or_default(),
                        );
                        index.push((v["index"].as_u64().unwrap_or(0), p));
                    }
                    Some("redacted_thinking") => {
                        let _ = events.send(ChatEvent::ReasoningMeta(json!({ "redacted": b["data"] })));
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let d = &v["delta"];
                match d["type"].as_str() {
                    Some("text_delta") => {
                        if let Some(t) = d["text"].as_str() {
                            let _ = events.send(ChatEvent::Text(t.into()));
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(t) = d["thinking"].as_str() {
                            let _ = events.send(ChatEvent::Reasoning(t.into()));
                        }
                    }
                    Some("signature_delta") => {
                        if let Some(s) = d["signature"].as_str() {
                            let _ = events.send(ChatEvent::ReasoningMeta(json!({ "signature": s })));
                        }
                    }
                    Some("input_json_delta") => {
                        let i = v["index"].as_u64().unwrap_or(0);
                        if let (Some((_, p)), Some(j)) =
                            (index.iter().find(|(k, _)| *k == i), d["partial_json"].as_str())
                        {
                            tools.0[*p].2.push_str(j);
                        }
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(o) = v["usage"]["output_tokens"].as_u64() {
                    usage.output_tokens = o;
                }
                match v["delta"]["stop_reason"].as_str() {
                    Some("max_tokens") => {
                        let _ = events.send(ChatEvent::Incomplete("output token limit".into()));
                    }
                    Some("refusal") => {
                        let _ = events.send(ChatEvent::Incomplete("the model refused".into()));
                    }
                    _ => {}
                }
            }
            "message_stop" => return Ok(false),
            "error" => return Err(anyhow!("Copilot: {}", error_message(&v).unwrap_or(v.to_string()))),
            _ => {}
        }
        Ok(true)
    })
    .await?;
    tools.flush(events);
    let _ = events.send(ChatEvent::Usage(usage));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolSpec;
    use crate::copilot::models::Endpoint;

    fn req(effort: Option<&str>) -> ChatRequest {
        ChatRequest {
            model: "m".into(),
            system: Some("sys".into()),
            messages: vec![Message::user("hi"), Message::assistant("yo"), Message::user("again")],
            effort: effort.map(String::from),
            session_id: "s".into(),
            ..Default::default()
        }
    }

    /// user → assistant (reasoning with meta, text, tool call) → tool result.
    fn tool_req() -> ChatRequest {
        let mut r = req(None);
        r.messages = vec![
            Message::user("list"),
            Message {
                role: Role::Assistant,
                parts: vec![
                    Part::Reasoning {
                        text: "think".into(),
                        meta: Some(json!({"signature": "sig", "encrypted_content": "enc", "reasoning_opaque": "op"})),
                    },
                    Part::Text { text: "Looking.".into() },
                    Part::ToolCall(ToolCall { id: "c1".into(), name: "glob".into(), input: json!({"pattern": "*"}) }),
                ],
                model: Some("copilot/m".into()),
            },
            Message {
                role: Role::User,
                parts: vec![Part::ToolResult { id: "c1".into(), content: "a.rs".into(), error: false }],
                model: None,
            },
        ];
        r.tools =
            vec![ToolSpec { name: "glob".into(), description: "find".into(), parameters: json!({"type": "object"}) }];
        r
    }

    #[test]
    fn chat_body_has_system_first() {
        let b = chat_body(&req(Some("high")), "copilot");
        assert_eq!(b["messages"][0], json!({"role": "system", "content": "sys"}));
        assert_eq!(b["messages"][3]["content"], "again");
        assert_eq!(b["reasoning_effort"], "high");
    }

    #[test]
    fn chat_body_with_tools() {
        let b = chat_body(&tool_req(), "copilot");
        let a = &b["messages"][2];
        assert_eq!(a["tool_calls"][0]["function"], json!({"name": "glob", "arguments": "{\"pattern\":\"*\"}"}));
        assert_eq!(a["reasoning_opaque"], "op");
        assert_eq!(b["messages"][3], json!({"role": "tool", "tool_call_id": "c1", "content": "a.rs"}));
        assert_eq!(b["tools"][0]["function"]["name"], "glob");
        // Another model's reasoning is not replayed.
        let mut other = tool_req();
        other.model = "other".into();
        assert!(chat_body(&other, "copilot")["messages"][2].get("reasoning_opaque").is_none());
    }

    #[test]
    fn responses_body_uses_instructions() {
        let b = responses_body(&req(None), "copilot", None);
        assert_eq!(b["instructions"], "sys");
        assert_eq!(b["input"][1], json!({"role": "assistant", "content": "yo"}));
        assert!(b.get("reasoning").is_none());
    }

    #[test]
    fn responses_body_with_tools() {
        let mut r = tool_req();
        r.effort = Some("high".into());
        let b = responses_body(&r, "copilot", None);
        let input = b["input"].as_array().unwrap();
        assert_eq!(input[1]["type"], "reasoning");
        assert_eq!(input[1]["encrypted_content"], "enc");
        assert_eq!(input[3]["type"], "function_call");
        assert_eq!(input[4], json!({"type": "function_call_output", "call_id": "c1", "output": "a.rs"}));
        assert_eq!(b["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(b["tools"][0]["name"], "glob");
    }

    #[test]
    fn messages_body_budgets_thinking() {
        let m = CopilotModel {
            id: "claude".into(),
            name: "Claude".into(),
            endpoint: Endpoint::Messages,
            context: None,
            max_input: None,
            max_output: Some(64_000),
            efforts: vec!["high".into(), "max".into()],
            adaptive_thinking: false,
            max_thinking_budget: Some(32_000),
            picker_enabled: true,
            vision: false,
        };
        let b = messages_body(&req(Some("high")), "copilot", Some(&m));
        assert_eq!(b["thinking"], json!({"type": "enabled", "budget_tokens": 16_000}));
        assert_eq!(b["max_tokens"], 64_000);
        assert_eq!(b["system"][0]["text"], "sys");
    }

    #[test]
    fn messages_body_with_tools() {
        let b = messages_body(&tool_req(), "copilot", None);
        let a = &b["messages"][1]["content"];
        assert_eq!(a[0], json!({"type": "thinking", "thinking": "think", "signature": "sig"}));
        assert_eq!(a[2]["type"], "tool_use");
        let u = &b["messages"][2]["content"][0];
        assert_eq!(u["type"], "tool_result");
        assert_eq!(u["cache_control"]["type"], "ephemeral");
        assert_eq!(b["tools"][0]["input_schema"], json!({"type": "object"}));
    }

    #[test]
    fn sends_images_in_each_api_format() {
        let msg = Message {
            role: Role::User,
            parts: vec![
                Part::ToolResult { id: "c1".into(), content: "Image a.png attached.".into(), error: false },
                Part::Text { text: "what is this?".into() },
                Part::Image { media_type: "image/png".into(), data: "QUJD".into() },
            ],
            model: None,
        };
        let r = ChatRequest { model: "m".into(), messages: vec![msg], session_id: "s".into(), ..Default::default() };
        let chat = chat_body(&r, "copilot");
        assert_eq!(chat["messages"][0]["role"], "tool");
        assert_eq!(
            chat["messages"][1]["content"][1],
            json!({"type": "image_url", "image_url": {"url": "data:image/png;base64,QUJD"}})
        );
        let resp = responses_body(&r, "copilot", None);
        assert_eq!(
            resp["input"][1]["content"][1],
            json!({"type": "input_image", "image_url": "data:image/png;base64,QUJD"})
        );
        let msgs = messages_body(&r, "copilot", None);
        let blocks = &msgs["messages"][0]["content"];
        assert_eq!(blocks[0]["type"], "tool_result", "tool results stay first");
        assert_eq!(blocks[2]["source"], json!({"type": "base64", "media_type": "image/png", "data": "QUJD"}));
    }
}
