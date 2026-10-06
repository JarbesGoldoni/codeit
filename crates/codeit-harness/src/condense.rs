//! Condensing long tool output before it enters the context.
//!
//! A long command log, web page or MCP result is mostly noise: progress lines, banners,
//! things that went fine. Its useful part is a few lines. When an output is long, a helper
//! model (the small model, else the session's) writes what ran, how it ended and the key
//! results; codeit itself copies every error-looking line verbatim next to it, so nothing the
//! agent needs to act on depends on the helper's wording. The full output stays on disk and
//! the model gets its path to grep. Reads and searches are never condensed: the agent asked
//! for that exact text.
//!
//! The helper request is agent-initiated (not a Copilot premium request). If it fails or is
//! slow, the output goes to the model as before (start and end kept).

use std::sync::Arc;
use std::time::Duration;

use codeit_providers::{ChatEvent, ChatRequest, Message, Provider};
use regex::Regex;

use crate::{Harness, prompt, util};

/// Tools whose exact output the agent needs, never condensed.
const EXACT: &[&str] = &["read", "edit", "write", "apply_patch", "grep", "glob", "todo", "task", "skill", "question"];
/// Outputs at least this long (tokens) are condensed, unless the config says otherwise.
const DEFAULT_MIN_TOKENS: u64 = 2_000;
/// Error lines copied verbatim.
const MAX_ERROR_LINES: usize = 20;
const TIMEOUT: Duration = Duration::from_secs(45);

/// Whether this tool's output may be condensed at this size.
pub fn wanted(h: &Harness, tool: &str, content: &str) -> bool {
    if h.config.tool_output.condense == Some(false) || EXACT.contains(&tool) {
        return false;
    }
    util::tokens(content) >= h.config.tool_output.condense_min_tokens.unwrap_or(DEFAULT_MIN_TOKENS)
}

/// Lines that look like errors, failures or their locations, in order, without repeats.
pub fn error_lines(text: &str) -> Vec<String> {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(error|errors|failed|failure|fatal|panic(ked)?|exception|traceback|assert(ion)?|denied|not found|undefined|segfault)\b|^\s*E\s{2,}|^\s+at\s+\S+:\d+|^\s*--> \S+:\d+",
        )
        .expect("valid regex")
    });
    let mut out: Vec<String> = Vec::new();
    for l in text.lines() {
        let t = l.trim_end();
        if t.is_empty() || !re.is_match(t) {
            continue;
        }
        let t = util::cut_line(t, 300);
        if !out.contains(&t) {
            out.push(t);
        }
        if out.len() == MAX_ERROR_LINES {
            break;
        }
    }
    out
}

/// Lines in a tool's full output (an output cut to its start and end says how long it was).
pub fn full_lines(content: &str) -> usize {
    const NOTE: &str = "(Output was long: the full ";
    content
        .rfind(NOTE)
        .and_then(|i| content[i + NOTE.len()..].split_whitespace().next()?.parse().ok())
        .unwrap_or_else(|| content.lines().count())
}

/// The full output's file, from the note `Ctx::limit` adds when it cut an output.
fn saved_path(content: &str) -> Option<String> {
    let start = content.rfind(" lines are in ")? + " lines are in ".len();
    let rest = &content[start..];
    let end = rest.find(". Use grep")?;
    Some(rest[..end].to_string())
}

pub struct Helper {
    pub key: String,
    pub provider: Arc<dyn Provider>,
    pub model: String,
}

/// Sends one request without tools to the helper model and returns its text; `None` when it
/// fails, answers nothing or takes too long.
pub async fn ask(
    helper: &Helper,
    session_id: &str,
    system: &str,
    request: String,
    cancel: &tokio_util::sync::CancellationToken,
) -> Option<String> {
    let req = ChatRequest {
        model: helper.model.clone(),
        system: Some(system.to_string()),
        messages: vec![Message::user(request)],
        tools: Vec::new(),
        effort: None,
        session_id: session_id.to_string(),
        agent_initiated: true,
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let p = helper.provider.clone();
    let task_handle = tokio::spawn(async move { p.chat(req, tx).await });
    let mut text = String::new();
    let collect = async {
        while let Some(ev) = rx.recv().await {
            if let ChatEvent::Text(t) = ev {
                text.push_str(&t);
            }
        }
    };
    tokio::select! {
        _ = collect => {}
        _ = tokio::time::sleep(TIMEOUT) => { task_handle.abort(); return None; }
        _ = cancel.cancelled() => { task_handle.abort(); return None; }
    }
    if !matches!(task_handle.await, Ok(Ok(()))) {
        return None;
    }
    let text = text.trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

/// Asks the helper model for a digest of `content`; `None` when it fails or takes too long.
/// Returns (what the model gets, the digest alone for the interface).
#[allow(clippy::too_many_arguments)]
pub async fn condense(
    helper: &Helper,
    session_id: &str,
    task: &str,
    tool: &str,
    title: &str,
    content: &str,
    cancel: &tokio_util::sync::CancellationToken,
) -> Option<(String, String)> {
    let request = format!(
        "<tool-output tool=\"{tool}\" call=\"{}\">\n{content}\n</tool-output>\n\nThe agent's current task: {}",
        title.replace('"', "'"),
        task.chars().take(600).collect::<String>()
    );
    let digest = ask(helper, session_id, prompt::CONDENSE, request, cancel).await?;

    let lines = full_lines(content);
    let path = saved_path(content).or_else(|| crate::tools::save_output(content).map(|p| p.display().to_string()));
    let mut out = format!("[{tool}: {lines} lines of output, condensed by {}]\n{digest}", helper.key);
    let errors = error_lines(content);
    if !errors.is_empty() {
        out.push_str("\n\nError lines, verbatim:\n");
        out.push_str(&errors.join("\n"));
    }
    if let Some(p) = path {
        out.push_str(&format!("\n\nFull output: {p} (grep it or read a range for anything not above; don't rerun the command just to see it)."));
    }
    Some((out, digest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_error_lines_verbatim_once() {
        let out = "compiling a\ncompiling b\nerror[E0425]: cannot find value `x`\n  --> src/main.rs:3:5\nerror[E0425]: cannot find value `x`\ntest add ... FAILED\nok\n";
        assert_eq!(
            error_lines(out),
            ["error[E0425]: cannot find value `x`", "  --> src/main.rs:3:5", "test add ... FAILED"]
        );
    }

    #[test]
    fn finds_the_saved_output() {
        let c = "a\n\n(Output was long: the full 900 lines are in /tmp/x/1-ab.txt. Use grep or read with offset on it instead of rerunning.)";
        assert_eq!(saved_path(c).as_deref(), Some("/tmp/x/1-ab.txt"));
        assert_eq!(full_lines(c), 900);
        assert_eq!(full_lines("a\nb"), 2);
    }
}
