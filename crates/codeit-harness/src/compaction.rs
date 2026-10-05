//! Keeping the context small: pruning old tool output, and summarizing the conversation when
//! it nears the model's limit.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, bail};
use codeit_providers::{ChatEvent, ChatRequest, Message, Part, Provider, Role};

use crate::event::Event;
use crate::session::{PRUNED, Session};
use crate::{Run, prompt, util};

/// Tool output in the newest turns that is never pruned (tokens).
const PROTECT: u64 = 40_000;
/// Prune only when at least this much would go, so prompt caches aren't broken for little.
const PRUNE_MIN: u64 = 20_000;
/// Each tool result in the text given to the summarizer is cut to this many characters.
const SUMMARY_TOOL_CHARS: usize = 2_000;

/// Tokens the next request will carry: the last reported usage plus what was added since.
pub fn estimate(s: &Session) -> u64 {
    let start = s.context_start.min(s.entries.len());
    let last_reply = s.entries.iter().rposition(|e| e.message.role == Role::Assistant && e.usage.is_some());
    match (&s.last_usage, last_reply) {
        (Some(u), Some(i)) if i >= start && u.input_tokens > 0 => {
            let added: u64 = s.entries[i + 1..].iter().map(|e| message_tokens(&e.message)).sum();
            u.input_tokens + u.output_tokens + added
        }
        _ => {
            s.summary.as_deref().map(util::tokens).unwrap_or(0)
                + s.entries[start..].iter().map(|e| message_tokens(&e.message)).sum::<u64>()
        }
    }
}

fn message_tokens(m: &Message) -> u64 {
    m.parts
        .iter()
        .map(|p| match p {
            Part::Text { text } | Part::Reasoning { text, .. } => util::tokens(text),
            Part::ToolCall(c) => util::tokens(&c.input.to_string()) + 10,
            Part::ToolResult { content, .. } => util::tokens(content) + 10,
            // Providers bill an image at roughly this many tokens.
            Part::Image { .. } => 1_500,
        })
        .sum()
}

/// Replaces old tool output with a short note, keeping the last two turns and the newest
/// output intact. Skill instructions are never pruned.
pub fn prune(s: &mut Session) {
    let start = s.context_start.min(s.entries.len());
    let names: HashMap<String, String> =
        s.entries.iter().flat_map(|e| e.message.tool_calls()).map(|c| (c.id.clone(), c.name.clone())).collect();
    let mut prompts = 0;
    let mut protected = 0;
    let mut targets: Vec<(usize, usize)> = Vec::new();
    let mut total = 0;
    for i in (start..s.entries.len()).rev() {
        if s.entries[i].is_prompt() {
            prompts += 1;
        }
        if prompts < 2 {
            continue;
        }
        for (j, p) in s.entries[i].message.parts.iter().enumerate().rev() {
            let Part::ToolResult { id, content, .. } = p else { continue };
            if content == PRUNED || names.get(id).is_some_and(|n| n == "skill") {
                continue;
            }
            let t = util::tokens(content);
            if protected < PROTECT {
                protected += t;
                continue;
            }
            total += t;
            targets.push((i, j));
        }
    }
    // Images older than the last two turns go: each costs as much as a page of text.
    let mut prompts = 0;
    for i in (start..s.entries.len()).rev() {
        if s.entries[i].is_prompt() {
            prompts += 1;
        }
        if prompts >= 2 {
            for p in &mut s.entries[i].message.parts {
                if matches!(p, Part::Image { .. }) {
                    total += 1_500;
                    *p = Part::Text { text: "[image removed to save context]".into() };
                }
            }
        }
    }
    if total < PRUNE_MIN {
        return;
    }
    for (i, j) in targets {
        if let Part::ToolResult { content, .. } = &mut s.entries[i].message.parts[j] {
            *content = PRUNED.to_string();
        }
    }
    // Repeated-read pointers may now point at pruned output.
    for m in s.files.values_mut() {
        m.read = None;
    }
}

/// The conversation as plain text for the summarizer.
fn transcript(messages: &[Message]) -> String {
    let mut out = String::new();
    for m in messages {
        for p in &m.parts {
            let line = match (m.role, p) {
                (Role::User, Part::Text { text }) => format!("[User]: {text}"),
                (Role::Assistant, Part::Text { text }) if !text.trim().is_empty() => format!("[Assistant]: {text}"),
                (_, Part::ToolCall(c)) => {
                    let args: String = c.input.to_string().chars().take(500).collect();
                    format!("[Tool call]: {}({args})", c.name)
                }
                (_, Part::ToolResult { content, error, .. }) => {
                    let body: String = content.chars().take(SUMMARY_TOOL_CHARS).collect();
                    let more = if content.chars().count() > SUMMARY_TOOL_CHARS { "\n[truncated]" } else { "" };
                    format!("[Tool {}]: {body}{more}", if *error { "error" } else { "result" })
                }
                _ => continue,
            };
            out.push_str(&line);
            out.push_str("\n\n");
        }
    }
    out
}

/// Summarizes the older part of the conversation, keeping the most recent turns as they are.
pub async fn compact(run: &Run, provider: &Arc<dyn Provider>, model_id: &str, key: &str, auto: bool) -> Result<()> {
    let usable = match run.harness.model_info(key).await {
        Some(i) => run.harness.usable_context(&i),
        None => None,
    };
    let keep_budget = usable.map(|u| (u / 4).clamp(2_000, 15_000)).unwrap_or(8_000);
    let (messages, prior, keep_from, session_id) = {
        let s = run.session.lock().unwrap();
        let start = s.context_start.min(s.entries.len());
        // Keep the newest turns that fit the budget, starting at a prompt.
        let mut keep_from = s.entries.len();
        let mut size = 0;
        for i in (start..s.entries.len()).rev() {
            size += message_tokens(&s.entries[i].message);
            if size > keep_budget {
                break;
            }
            if s.entries[i].is_prompt() {
                keep_from = i;
            }
        }
        // Everything fits in the budget yet the context is full: summarize it all.
        if keep_from == start {
            keep_from = s.entries.len();
        }
        let messages: Vec<Message> = s.entries[start..keep_from].iter().map(|e| e.message.clone()).collect();
        (messages, s.summary.clone(), keep_from, s.id.clone())
    };
    if messages.is_empty() {
        if !auto {
            run.events.send(Event::Notice("Nothing to compact yet.".into())).ok();
        }
        return Ok(());
    }
    run.events
        .send(Event::Notice(if auto {
            "Context is nearly full; summarizing the conversation...".into()
        } else {
            "Summarizing the conversation...".into()
        }))
        .ok();

    let mut request = String::new();
    if let Some(p) = &prior {
        request.push_str(&format!(
            "<prior-summary>\n{p}\n</prior-summary>\n\nThe prior summary covers everything before the conversation below. Write one new summary covering both: carry forward what still matters from the prior summary, and where they conflict, the conversation wins.\n\n"
        ));
    }
    request.push_str(&format!(
        "<conversation>\n{}</conversation>\n\n{}",
        transcript(&messages),
        prompt::COMPACTION_REQUEST
    ));
    // The small model writes the summary when its context can hold the transcript.
    let (provider, model_id) = match run.harness.helper(key, util::tokens(&request) + 4_000).await {
        Some(h) => (h.provider, h.model),
        None => (provider.clone(), model_id.to_string()),
    };
    let req = ChatRequest {
        model: model_id,
        system: Some(prompt::COMPACTION.to_string()),
        messages: vec![Message::user(request)],
        tools: Vec::new(),
        effort: None,
        session_id,
        agent_initiated: true,
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let p = provider.clone();
    let task = tokio::spawn(async move { p.chat(req, tx).await });
    let mut summary = String::new();
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Some(ChatEvent::Text(t)) => summary.push_str(&t),
                Some(_) => {}
                None => break,
            },
            _ = run.cancel.cancelled() => {
                task.abort();
                bail!("Compaction interrupted.");
            }
        }
    }
    task.await??;
    let summary = summary.trim().to_string();
    if summary.is_empty() {
        bail!("The model returned an empty summary; the conversation was not compacted.");
    }
    let mut s = run.session.lock().unwrap();
    let count = keep_from - s.context_start.min(keep_from);
    s.summary = Some(summary);
    s.context_start = keep_from;
    s.last_usage = None;
    for m in s.files.values_mut() {
        m.read = None;
    }
    s.instructions.clear();
    let _ = s.save();
    drop(s);
    run.events.send(Event::Notice(format!("Summarized {count} earlier messages to free up context."))).ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Entry;
    use codeit_providers::ToolCall;
    use serde_json::json;

    fn tool_turn(s: &mut Session, prompt: &str, output_chars: usize) {
        let mut e = Entry::new(Message::user(prompt));
        e.prompt = Some(prompt.into());
        s.push(e);
        let id = uuid::Uuid::new_v4().to_string();
        let call = ToolCall { id: id.clone(), name: "bash".into(), input: json!({"command": "x"}) };
        s.push(Entry::new(Message { role: Role::Assistant, parts: vec![Part::ToolCall(call)], model: None }));
        let result = Part::ToolResult { id, content: "o".repeat(output_chars), error: false };
        s.push(Entry::new(Message { role: Role::User, parts: vec![result], model: None }));
    }

    #[test]
    fn prunes_old_output_but_not_recent_turns() {
        let mut s = Session::new(std::path::Path::new("/tmp"), "build", None, None);
        for i in 0..6 {
            tool_turn(&mut s, &format!("p{i}"), 80_000); // 20k tokens each
        }
        prune(&mut s);
        let pruned: Vec<bool> = s
            .entries
            .iter()
            .filter_map(|e| match e.message.parts.first() {
                Some(Part::ToolResult { content, .. }) => Some(content == PRUNED),
                _ => None,
            })
            .collect();
        // Last two turns protected, then 40k tokens (two more outputs) kept.
        assert_eq!(pruned, [true, true, false, false, false, false]);
    }

    #[test]
    fn small_sessions_are_left_alone() {
        let mut s = Session::new(std::path::Path::new("/tmp"), "build", None, None);
        for i in 0..4 {
            tool_turn(&mut s, &format!("p{i}"), 4_000);
        }
        prune(&mut s);
        assert!(s.entries.iter().all(|e| {
            !e.message.parts.iter().any(|p| matches!(p, Part::ToolResult { content, .. } if content == PRUNED))
        }));
        assert!(estimate(&s) > 4_000);
    }
}
