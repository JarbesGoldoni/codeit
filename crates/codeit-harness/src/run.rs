//! One turn: the model replies, calls tools, gets their results, and goes on until it answers.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use codeit_providers::{ChatEvent, ChatRequest, Message, Part, Provider, Role, ToolCall, Usage};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::event::Event;
use crate::permission::Ruleset;
use crate::session::{Entry, Session};
use crate::tools::{Ctx, Output, Refused, Tool};
use crate::{Harness, compaction, prompt, util};

pub enum Input {
    /// What the user typed. `@path` mentions attach those files.
    Prompt(String),
    /// A prompt command: `/name args`.
    Command { name: String, args: String },
    /// Summarize the conversation now.
    Compact,
}

pub struct Run {
    pub harness: Arc<Harness>,
    pub session: Arc<Mutex<Session>>,
    pub events: UnboundedSender<Event>,
    pub cancel: CancellationToken,
    /// 0 for the user's session, 1 inside a subagent.
    pub depth: usize,
}

/// What one model request produced.
#[derive(Default)]
struct Step {
    parts: Vec<Part>,
    usage: Option<Usage>,
    incomplete: Option<String>,
    error: Option<anyhow::Error>,
    cancelled: bool,
}

const MAX_RETRIES: u32 = 3;

impl Run {
    fn emit(&self, e: Event) {
        let _ = self.events.send(e);
    }

    /// Runs the turn to the end. Errors are reported as events; the session is saved.
    pub async fn start(&self, input: Input) {
        // Snapshots around the turns you type, so /undo can revert what commands changed.
        let snapshots = self.depth == 0 && !matches!(input, Input::Compact);
        let before = if snapshots { self.snapshot().await } else { None };
        let first_new = self.session.lock().unwrap().entries.len();
        if let Err(e) = self.turn(input).await {
            self.emit(Event::Error(format!("{e:#}")));
        }
        if let Some(before) = before {
            let after = self.snapshot().await;
            let mut s = self.session.lock().unwrap();
            let n = s.entries.len();
            if let Some(e) = s.entries[first_new.min(n)..].iter_mut().find(|e| e.is_prompt()) {
                e.before = Some(before);
                e.after = after;
            }
        }
        {
            let mut s = self.session.lock().unwrap();
            if self.harness.config.compaction.prune != Some(false) {
                compaction::prune(&mut s);
            }
            if let Err(e) = s.save() {
                self.emit(Event::Error(format!("couldn't save the session: {e:#}")));
            }
        }
        self.emit(Event::Done);
    }

    async fn snapshot(&self) -> Option<String> {
        let root = self.harness.root.clone();
        tokio::task::spawn_blocking(move || crate::snapshot::track(&root)).await.ok().flatten()
    }

    fn model(&self) -> Result<(String, Arc<dyn Provider>, String)> {
        let key = self
            .session
            .lock()
            .unwrap()
            .model
            .clone()
            .ok_or_else(|| anyhow!("No model selected. Pick one with /models."))?;
        let (pid, mid) = codeit_providers::split_key(&key).ok_or_else(|| anyhow!("bad model key `{key}`"))?;
        Ok((key.clone(), self.harness.provider(pid)?, mid.to_string()))
    }

    fn agent(&self, name: &str) -> Agent {
        self.harness.agent(name).or_else(|| self.harness.agent("build")).cloned().expect("the build agent exists")
    }

    async fn turn(&self, input: Input) -> Result<()> {
        let (key, provider, model_id) = self.model()?;
        let agent_name = self.session.lock().unwrap().agent.clone();
        let mut agent = self.agent(&agent_name);

        match input {
            Input::Compact => {
                compaction::compact(self, &provider, &model_id, &key, false).await?;
                return Ok(());
            }
            Input::Prompt(text) => self.add_prompt(&text, &text).await,
            Input::Command { name, args } => {
                let cmd = self
                    .harness
                    .commands
                    .iter()
                    .find(|c| c.name == name)
                    .cloned()
                    .ok_or_else(|| anyhow!("unknown command /{name}"))?;
                let text =
                    crate::command::run_shell(&crate::command::render(&cmd.template, &args), &self.harness.cwd).await;
                let typed = format!("/{name} {args}").trim_end().to_string();
                if let Some(a) = cmd.agent.as_deref().and_then(|a| self.harness.agent(a))
                    && a.primary()
                    && !cmd.subtask
                {
                    agent = a.clone();
                }
                if cmd.subtask || cmd.agent.as_deref().and_then(|a| self.harness.agent(a)).is_some_and(|a| !a.primary())
                {
                    // Run it in a subagent, as a task call the model then reports on.
                    let mut entry = Entry::new(Message::user(typed.clone()));
                    entry.prompt = Some(typed);
                    self.session.lock().unwrap().push(entry);
                    let call = ToolCall {
                        id: format!("call_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]),
                        name: "task".into(),
                        input: json!({
                            "description": name,
                            "prompt": text,
                            "subagent_type": cmd.agent.clone().unwrap_or_else(|| "general".into()),
                        }),
                    };
                    let msg =
                        Message { role: Role::Assistant, parts: vec![Part::ToolCall(call)], model: Some(key.clone()) };
                    let mut e = Entry::new(msg);
                    e.agent = Some(agent.name.clone());
                    self.session.lock().unwrap().push(e);
                } else {
                    self.add_prompt(&typed, &text).await;
                }
            }
        }
        self.run_loop(&agent, &key, &provider, &model_id).await
    }

    /// Adds the user's message, with the files it mentions attached.
    async fn add_prompt(&self, typed: &str, text: &str) {
        let mut parts = vec![Part::Text { text: text.to_string() }];
        for path in mentions(text, &self.harness.cwd) {
            let shown = util::display(&self.harness.cwd, &path);
            if path.is_file() && crate::tools::image_type(&path).is_some() {
                match crate::tools::read_image(&path) {
                    Ok((media_type, data)) => {
                        parts.push(Part::Text { text: format!("<image path=\"{shown}\"> attached below.") });
                        parts.push(Part::Image { media_type, data });
                    }
                    Err(e) => parts.push(Part::Text { text: format!("<image path=\"{shown}\"> not attached: {e}") }),
                }
                continue;
            }
            let body = if path.is_dir() {
                std::fs::read_dir(&path)
                    .map(|r| {
                        let mut v: Vec<String> =
                            r.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
                        v.sort();
                        v.join("\n")
                    })
                    .unwrap_or_default()
            } else {
                match crate::tools::read_for_model(&path, 1, 2000) {
                    Ok(t) => t,
                    Err(e) => format!("({e})"),
                }
            };
            parts.push(Part::Text { text: format!("<file path=\"{shown}\">\n{body}\n</file>") });
            if path.is_file() {
                self.session.lock().unwrap().mark_known(&path, None);
            }
        }
        let mut entry = Entry::new(Message { role: Role::User, parts, model: None });
        entry.prompt = Some(typed.to_string());
        let title = {
            let mut s = self.session.lock().unwrap();
            s.push(entry);
            if s.title.is_empty() {
                s.title = util::title(typed);
                Some(s.title.clone())
            } else {
                None
            }
        };
        if let Some(t) = title {
            self.emit(Event::Title(t));
        }
    }

    async fn run_loop(&self, agent: &Agent, key: &str, provider: &Arc<dyn Provider>, model_id: &str) -> Result<()> {
        let h = &self.harness;
        let approval = self.session.lock().unwrap().approval;
        let rules = h.rules(agent, approval);
        let mut step = 0;
        let mut retries = 0;
        let mut compacted = false;
        loop {
            if self.cancel.is_cancelled() {
                break;
            }
            // Tool calls without results (a subtask command) run before the next request.
            if let Some((index, calls)) = self.unanswered() {
                let stop = self.run_tools(calls, index, agent, &rules, model_id).await;
                if stop || self.cancel.is_cancelled() {
                    break;
                }
                continue;
            }
            step += 1;
            let info = h.model_info(key).await;
            let usable = info.as_ref().and_then(|i| h.usable_context(i));
            let used = compaction::estimate(&self.session.lock().unwrap());
            if let Some(limit) = usable
                && h.config.compaction.auto != Some(false)
                && !compacted
                && used >= limit
            {
                compaction::compact(self, provider, model_id, key, true).await?;
                compacted = true;
                continue;
            }
            compacted = false;

            // On the last allowed step the tools stay defined (APIs reject tool calls in the
            // history otherwise), but a reminder says not to use them and calls are refused.
            let last_step = agent.steps.is_some_and(|m| step >= m);
            let tools: Vec<Arc<dyn Tool>> = h.tools(model_id, &rules, self.depth);
            let req = {
                let s = self.session.lock().unwrap();
                let mut messages = s.context();
                self.remind(&s, agent, &mut messages, last_step);
                if info.as_ref().is_some_and(|i| !i.vision) {
                    without_images(&mut messages, key);
                }
                ChatRequest {
                    model: model_id.to_string(),
                    system: Some(prompt::system(h, agent, key, &rules)),
                    messages,
                    tools: tools.iter().map(|t| t.spec(h)).collect(),
                    effort: s.effort.clone(),
                    session_id: s.id.clone(),
                    agent_initiated: step > 1 || self.depth > 0,
                }
            };
            let out = self.stream(provider, req).await;

            if let Some(err) = out.error {
                let has_calls = out.parts.iter().any(|p| matches!(p, Part::ToolCall(_)));
                let text = format!("{err:#}");
                if overflowed(&text) && !compacted {
                    self.emit(Event::Reset);
                    compaction::compact(self, provider, model_id, key, true).await?;
                    compacted = true;
                    step -= 1;
                    continue;
                }
                if !has_calls && retries < MAX_RETRIES && transient(&text) {
                    retries += 1;
                    let wait = 1u64 << retries;
                    self.emit(Event::Reset);
                    self.emit(Event::Notice(format!("{text}\nRetrying in {wait}s ({retries}/{MAX_RETRIES})...")));
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(wait)) => {}
                        _ = self.cancel.cancelled() => break,
                    }
                    step -= 1;
                    continue;
                }
                // Keep the text that arrived; calls that will never run are dropped.
                let mut parts = out.parts;
                parts.retain(|p| !matches!(p, Part::ToolCall(_)));
                if !parts.is_empty() {
                    self.push_assistant(parts, key, agent, out.usage);
                }
                bail!(text);
            }
            retries = 0;

            if out.parts.is_empty() {
                if !out.cancelled {
                    self.emit(Event::Notice("The model returned an empty reply.".into()));
                }
                break;
            }
            let calls: Vec<ToolCall> = out
                .parts
                .iter()
                .filter_map(|p| if let Part::ToolCall(c) = p { Some(c.clone()) } else { None })
                .collect();
            let index = self.push_assistant(out.parts, key, agent, out.usage.clone());
            if let Some(u) = out.usage {
                let context = usable.map(|l| (u.input_tokens + u.output_tokens, l));
                self.emit(Event::Step { usage: u, context });
            }
            if let Some(reason) = out.incomplete {
                self.emit(Event::Notice(format!("The reply stopped early ({reason}).")));
            }
            if calls.is_empty() || out.cancelled {
                if out.cancelled && !calls.is_empty() {
                    self.answer_interrupted(&calls);
                }
                break;
            }
            if last_step {
                let parts = calls
                    .iter()
                    .map(|c| Part::ToolResult {
                        id: c.id.clone(),
                        content: "Step limit reached: tools are disabled.".into(),
                        error: true,
                    })
                    .collect();
                self.session.lock().unwrap().push(Entry::new(Message { role: Role::User, parts, model: None }));
                self.emit(Event::Notice("The agent reached its step limit.".into()));
                break;
            }
            let stop = self.run_tools(calls, index, agent, &rules, model_id).await;
            let _ = self.session.lock().unwrap().save();
            if stop || self.cancel.is_cancelled() {
                break;
            }
        }
        Ok(())
    }

    /// The plan-mode reminder, the switch back to build, and the step-limit notice.
    fn remind(&self, s: &Session, agent: &Agent, messages: &mut [Message], last_step: bool) {
        let mut add = |text: &str, last: bool| {
            let target = if last {
                messages.last_mut()
            } else {
                messages
                    .iter_mut()
                    .rev()
                    .find(|m| m.role == Role::User && !m.parts.iter().any(|p| matches!(p, Part::ToolResult { .. })))
            };
            if let Some(m) = target {
                m.parts.push(Part::Text { text: text.to_string() });
            }
        };
        if agent.name == "plan" {
            add(prompt::PLAN, false);
        } else if agent.name == "build" {
            let prev =
                s.entries.iter().rev().find(|e| e.message.role == Role::Assistant).and_then(|e| e.agent.as_deref());
            if prev == Some("plan") {
                add(prompt::BUILD_SWITCH, false);
            }
        }
        if last_step {
            add(prompt::MAX_STEPS, true);
        }
    }

    fn push_assistant(&self, parts: Vec<Part>, key: &str, agent: &Agent, usage: Option<Usage>) -> usize {
        let mut e = Entry::new(Message { role: Role::Assistant, parts, model: Some(key.to_string()) });
        e.agent = Some(agent.name.clone());
        e.usage = usage.clone();
        let mut s = self.session.lock().unwrap();
        if usage.is_some() {
            s.last_usage = usage;
        }
        s.push(e)
    }

    /// The last assistant entry's tool calls, if they have no results yet.
    fn unanswered(&self) -> Option<(usize, Vec<ToolCall>)> {
        let s = self.session.lock().unwrap();
        let last = s.entries.last()?;
        if last.message.role != Role::Assistant {
            return None;
        }
        let calls: Vec<ToolCall> = last.message.tool_calls().cloned().collect();
        (!calls.is_empty()).then(|| (s.entries.len() - 1, calls))
    }

    fn answer_interrupted(&self, calls: &[ToolCall]) {
        let parts = calls
            .iter()
            .map(|c| Part::ToolResult { id: c.id.clone(), content: "Interrupted by the user.".into(), error: true })
            .collect();
        self.session.lock().unwrap().push(Entry::new(Message { role: Role::User, parts, model: None }));
    }

    /// Sends one request and collects the reply, forwarding text as it streams.
    async fn stream(&self, provider: &Arc<dyn Provider>, req: ChatRequest) -> Step {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let p = provider.clone();
        let task = tokio::spawn(async move { p.chat(req, tx).await });
        let mut step = Step::default();
        loop {
            tokio::select! {
                ev = rx.recv() => match ev {
                    Some(ev) => self.collect(&mut step, ev),
                    None => break,
                },
                _ = self.cancel.cancelled() => {
                    task.abort();
                    step.cancelled = true;
                    // Unfinished tool calls can't be answered; drop them.
                    step.parts.retain(|p| !matches!(p, Part::ToolCall(_)));
                    return step;
                }
            }
        }
        match task.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => step.error = Some(e),
            Err(e) => step.error = Some(anyhow!("the request task failed: {e}")),
        }
        step
    }

    fn collect(&self, step: &mut Step, ev: ChatEvent) {
        let parts = &mut step.parts;
        match ev {
            ChatEvent::Text(t) => {
                if let Some(Part::Text { text }) = parts.last_mut() {
                    text.push_str(&t);
                } else {
                    parts.push(Part::Text { text: t.clone() });
                }
                self.emit(Event::Text(t));
            }
            ChatEvent::Reasoning(t) => {
                if let Some(Part::Reasoning { text, meta: None }) = parts.last_mut() {
                    text.push_str(&t);
                } else {
                    parts.push(Part::Reasoning { text: t.clone(), meta: None });
                }
                self.emit(Event::Reasoning(t));
            }
            ChatEvent::ReasoningMeta(m) => {
                let open = parts.iter_mut().rev().find_map(|p| match p {
                    Part::Reasoning { meta, .. } if meta.is_none() => Some(meta),
                    _ => None,
                });
                match open {
                    Some(meta) => *meta = Some(m),
                    None => parts.push(Part::Reasoning { text: String::new(), meta: Some(m) }),
                }
            }
            ChatEvent::ToolStart { id, name } => self.emit(Event::ToolPending { id, name }),
            ChatEvent::ToolCall(c) => parts.push(Part::ToolCall(c)),
            ChatEvent::Usage(u) => step.usage = Some(u),
            ChatEvent::Incomplete(r) => step.incomplete = Some(r),
        }
    }

    /// Runs the calls (read-only ones side by side) and records their results.
    /// Returns true when the user rejected a call, which ends the turn.
    async fn run_tools(
        &self,
        calls: Vec<ToolCall>,
        entry: usize,
        agent: &Agent,
        rules: &Ruleset,
        model_id: &str,
    ) -> bool {
        let tools = self.harness.tools(model_id, rules, self.depth);
        let mut results: Vec<Part> = Vec::new();
        let mut images: Vec<Part> = Vec::new();
        let mut stop = false;
        let mut i = 0;
        while i < calls.len() {
            if stop || self.cancel.is_cancelled() {
                let why = if stop { "Skipped: the user rejected an earlier call." } else { "Interrupted by the user." };
                self.emit(Event::ToolDone {
                    id: calls[i].id.clone(),
                    name: calls[i].name.clone(),
                    title: String::new(),
                    output: why.into(),
                    error: true,
                    diff: None,
                    lines: None,
                    digest: None,
                });
                results.push(Part::ToolResult { id: calls[i].id.clone(), content: why.into(), error: true });
                i += 1;
                continue;
            }
            let read_only = |c: &ToolCall| tools.iter().any(|t| t.name() == c.name && t.read_only());
            let mut j = i + 1;
            if read_only(&calls[i]) {
                while j < calls.len() && read_only(&calls[j]) {
                    j += 1;
                }
            }
            let batch = &calls[i..j];
            let outcomes =
                futures_util::future::join_all(batch.iter().map(|c| self.run_tool(c, entry, agent, rules, &tools)))
                    .await;
            for (part, more, rejected) in outcomes {
                stop |= rejected;
                results.push(part);
                images.extend(more);
            }
            i = j;
        }
        // Images tools returned follow the results.
        results.extend(images);
        self.session.lock().unwrap().push(Entry::new(Message { role: Role::User, parts: results, model: None }));
        stop
    }

    async fn run_tool(
        &self,
        call: &ToolCall,
        entry: usize,
        agent: &Agent,
        rules: &Ruleset,
        tools: &[Arc<dyn Tool>],
    ) -> (Part, Vec<Part>, bool) {
        let fail =
            |content: String| (Part::ToolResult { id: call.id.clone(), content, error: true }, Vec::new(), false);
        let Some(tool) = tools.iter().find(|t| t.name() == call.name) else {
            let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
            let msg = format!("Unknown tool `{}`. Available tools: {}.", call.name, names.join(", "));
            self.emit(Event::ToolDone {
                id: call.id.clone(),
                name: call.name.clone(),
                title: String::new(),
                output: msg.clone(),
                error: true,
                diff: None,
                lines: None,
                digest: None,
            });
            return fail(msg);
        };
        let input = match &call.input {
            Value::Object(_) => call.input.clone(),
            Value::String(raw) => match serde_json::from_str::<Value>(raw) {
                Ok(v @ Value::Object(_)) => v,
                _ => {
                    let raw: String = raw.chars().take(200).collect();
                    return fail(format!(
                        "The arguments were not a valid JSON object ({raw}). Call {} again with arguments matching its schema.",
                        call.name
                    ));
                }
            },
            _ => json!({}),
        };
        let title = tool.title(&input, &self.harness.cwd);
        self.emit(Event::ToolStart {
            id: call.id.clone(),
            name: call.name.clone(),
            title: title.clone(),
            input: input.clone(),
        });

        let ctx = Ctx {
            harness: self.harness.clone(),
            session: self.session.clone(),
            agent: agent.clone(),
            rules: rules.clone(),
            call_id: call.id.clone(),
            entry,
            events: self.events.clone(),
            cancel: self.cancel.clone(),
            depth: self.depth,
        };
        let result = async {
            if self.repeated(call) {
                ctx.ask(
                    "doom_loop",
                    std::slice::from_ref(&call.name),
                    std::slice::from_ref(&call.name),
                    format!("{} was called 3 times in a row with the same arguments", call.name),
                    Some(title.clone()),
                )
                .await?;
            }
            tokio::select! {
                r = tool.run(input, &ctx) => r,
                _ = self.cancel.cancelled() => Err(anyhow!("Interrupted by the user.")),
            }
        }
        .await;

        let mut images = Vec::new();
        let (content, error, rejected) = match result {
            Ok(Output { mut content, display, diff, images: imgs, touched }) => {
                images = imgs.into_iter().map(|(media_type, data)| Part::Image { media_type, data }).collect();
                // What the language servers think of the files just written.
                if !touched.is_empty()
                    && let Some(note) = crate::tools::lsp::errors_after_edit(&self.harness, &touched).await
                {
                    content.push_str("\n\n");
                    content.push_str(&note);
                }
                let shown = display.unwrap_or_else(|| content.clone());
                let lines = crate::condense::full_lines(&content);
                let (content, digest) = self.condense(call, &title, content).await;
                self.emit(Event::ToolDone {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    title: title.clone(),
                    output: cap(&shown),
                    error: false,
                    diff,
                    lines: Some(lines),
                    digest,
                });
                (content, false, false)
            }
            Err(e) => {
                let rejected = e.downcast_ref::<Refused>().is_some_and(|r| r.by_user);
                let msg = format!("{e:#}");
                self.emit(Event::ToolDone {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    title: title.clone(),
                    output: cap(&msg),
                    error: true,
                    diff: None,
                    lines: None,
                    digest: None,
                });
                (msg, true, rejected)
            }
        };
        (Part::ToolResult { id: call.id.clone(), content, error }, images, rejected)
    }

    /// Long command, fetch and MCP output goes to the model condensed by the helper model
    /// (see [`crate::condense`]). Returns what the model gets, and the digest when condensed.
    async fn condense(&self, call: &ToolCall, title: &str, content: String) -> (String, Option<String>) {
        if !crate::condense::wanted(&self.harness, &call.name, &content) {
            return (content, None);
        }
        let Ok((key, _, _)) = self.model() else { return (content, None) };
        let Some(helper) = self.harness.helper(&key, util::tokens(&content) + 2_000).await else {
            return (content, None);
        };
        let (session_id, task) = {
            let s = self.session.lock().unwrap();
            (s.id.clone(), s.entries.iter().rev().find_map(|e| e.prompt.clone()).unwrap_or_default())
        };
        match crate::condense::condense(&helper, &session_id, &task, &call.name, title, &content, &self.cancel).await {
            Some((for_model, digest)) => (for_model, Some(digest)),
            None => (content, None),
        }
    }

    /// True when this call repeats the two before it exactly (a loop the model may be stuck in).
    fn repeated(&self, call: &ToolCall) -> bool {
        let s = self.session.lock().unwrap();
        let all: Vec<&ToolCall> = s.entries.iter().flat_map(|e| e.message.tool_calls()).collect();
        let Some(pos) = all.iter().position(|c| c.id == call.id) else { return false };
        pos >= 2 && all[pos - 2..pos].iter().all(|c| c.name == call.name && c.input == call.input)
    }
}

/// Output sent to the interface: enough to show, not the whole thing.
fn cap(text: &str) -> String {
    util::head_tail(text, 200, 16_000).0
}

/// Replaces images with a note, for models that can't see them.
fn without_images(messages: &mut [Message], model: &str) {
    for m in messages {
        for p in &mut m.parts {
            if matches!(p, Part::Image { .. }) {
                *p = Part::Text { text: format!("[An image was attached here, but {model} can't see images.]") };
            }
        }
    }
}

/// `@path` mentions of existing files or folders.
fn mentions(text: &str, cwd: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for word in text.split_whitespace() {
        let Some(p) = word.strip_prefix('@') else { continue };
        let p = p.trim_end_matches([',', '.', ';', ':', ')', '?', '!', '"', '\'']);
        if p.is_empty() {
            continue;
        }
        let path = util::resolve(cwd, p);
        if path.exists() && !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

/// Errors worth retrying: network trouble, rate limits, overloaded servers.
fn transient(text: &str) -> bool {
    let t = text.to_lowercase();
    [
        "timed out",
        "timeout",
        "connection",
        "stream failed",
        "error decoding",
        "429",
        "500",
        "502",
        "503",
        "504",
        "529",
        "overloaded",
        "rate limit",
        "throttl",
        "temporarily",
    ]
    .iter()
    .any(|k| t.contains(k))
}

/// The provider said the conversation is too long for the model.
fn overflowed(text: &str) -> bool {
    let t = text.to_lowercase();
    [
        "context length",
        "context window",
        "too many tokens",
        "prompt is too long",
        "input is too long",
        "maximum context",
        "exceeds the context",
        "context_length_exceeded",
        "input too long",
    ]
    .iter()
    .any(|k| t.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_errors_and_mentions() {
        assert!(transient("Copilot 503 Service Unavailable"));
        assert!(!transient("Copilot 400: bad request"));
        assert!(overflowed("prompt is too long: 210000 tokens > 200000 maximum"));
        let d = std::env::temp_dir().join(format!("codeit-mention-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("a.rs"), "x").unwrap();
        assert_eq!(mentions("look at @a.rs, and @missing.rs", &d), vec![d.join("a.rs")]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
