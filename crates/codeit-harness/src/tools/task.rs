//! task: hands work to a subagent with its own context, and returns its final answer.
//! The subagent's session is kept, so the same task_id can continue it later.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use codeit_providers::{Part, Role, ToolSpec};
use serde_json::{Value, json};

use super::{Ctx, Output, Tool, arg, spec};
use crate::event::Event;
use crate::session::Session;
use crate::{Harness, Input, Run};

pub struct Task;

#[async_trait]
impl Tool for Task {
    fn name(&self) -> &str {
        "task"
    }

    fn spec(&self, h: &Harness) -> ToolSpec {
        let agents: Vec<String> =
            h.agents.iter().filter(|a| a.subagent()).map(|a| format!("- {}: {}", a.name, a.description)).collect();
        spec(
            "task",
            &format!(
                "Start a subagent on a task. It works with its own context and returns only its final message, which \
the user doesn't see (summarize it for them). Use it for broad searches and research that would fill your context, \
and for independent pieces of work, launched in parallel with several calls in one response. Don't use it to read a \
known file or search for a specific symbol. The subagent knows nothing of this conversation: give it a complete brief \
and say exactly what to report back, and whether it should change code or only research. Pass task_id from an earlier \
result to continue that subagent.\n\nSubagents:\n{}",
                agents.join("\n")
            ),
            json!({
                "type": "object",
                "properties": {
                    "description": { "type": "string", "description": "Short (3-5 word) description" },
                    "prompt": { "type": "string", "description": "The full task for the subagent" },
                    "subagent_type": { "type": "string", "description": "Which subagent to use" },
                    "task_id": { "type": "string", "description": "Continue an earlier subagent session" }
                },
                "required": ["description", "prompt", "subagent_type"]
            }),
        )
    }

    fn read_only(&self) -> bool {
        // Subagents launched together run in parallel.
        true
    }

    fn title(&self, input: &Value, _: &Path) -> String {
        format!(
            "{} ({})",
            input["description"].as_str().unwrap_or_default(),
            input["subagent_type"].as_str().unwrap_or("general")
        )
    }

    async fn run(&self, input: Value, ctx: &Ctx) -> Result<Output> {
        let prompt = arg(&input, "prompt")?.to_string();
        let kind = input["subagent_type"].as_str().unwrap_or("general");
        let description = input["description"].as_str().unwrap_or(kind).to_string();
        let agent = ctx
            .harness
            .agent(kind)
            .filter(|a| a.subagent())
            .ok_or_else(|| {
                let names: Vec<&str> =
                    ctx.harness.agents.iter().filter(|a| a.subagent()).map(|a| a.name.as_str()).collect();
                anyhow!("No subagent `{kind}`. Available: {}", names.join(", "))
            })?
            .clone();
        if ctx.depth > 0 {
            bail!("Subagents can't start other subagents.");
        }
        ctx.ask(
            "task",
            &[kind.to_string()],
            &[kind.to_string()],
            format!("start {kind} subagent: {description}"),
            None,
        )
        .await?;

        let (parent_id, model, effort, approval) = {
            let s = ctx.session.lock().unwrap();
            (s.id.clone(), s.model.clone(), s.effort.clone(), s.approval)
        };
        let resumed = input["task_id"]
            .as_str()
            .and_then(|id| Session::load(id).ok())
            .filter(|s| s.parent.as_deref() == Some(parent_id.as_str()));
        let child = match resumed {
            Some(s) => s,
            None => {
                let mut s = Session::new(ctx.cwd(), &agent.name, agent.model.clone().or(model), effort);
                s.parent = Some(parent_id);
                s.title = description.clone();
                s.approval = approval;
                s
            }
        };
        let task_id = child.id.clone();
        let child = Arc::new(Mutex::new(child));

        // Forward the subagent's questions to the user and its steps as progress.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let parent_events = ctx.events.clone();
        let call_id = ctx.call_id.clone();
        let forward = tokio::spawn(async move {
            let mut errors = Vec::new();
            while let Some(ev) = rx.recv().await {
                match ev {
                    Event::Ask(a) => {
                        let _ = parent_events.send(Event::Ask(a));
                    }
                    Event::ToolStart { name, title, .. } => {
                        let _ = parent_events
                            .send(Event::ToolProgress { id: call_id.clone(), text: format!("{name} {title}") });
                    }
                    Event::Error(e) => errors.push(e),
                    _ => {}
                }
            }
            errors
        });
        let run = Run {
            harness: ctx.harness.clone(),
            session: child.clone(),
            events: tx,
            cancel: ctx.cancel.child_token(),
            depth: ctx.depth + 1,
        };
        run.start(Input::Prompt(prompt)).await;
        drop(run);
        let errors = forward.await.unwrap_or_default();

        let answer = {
            let s = child.lock().unwrap();
            s.entries
                .iter()
                .rev()
                .find(|e| {
                    e.message.role == Role::Assistant
                        && e.message.parts.iter().any(|p| matches!(p, Part::Text { text } if !text.trim().is_empty()))
                })
                .map(|e| e.message.text())
                .unwrap_or_default()
        };
        if answer.trim().is_empty() {
            let why = if errors.is_empty() { "it returned no answer".to_string() } else { errors.join("; ") };
            bail!("The subagent stopped: {why}. task_id: {task_id}");
        }
        let mut content = format!(
            "task_id: {task_id} (pass it to continue this subagent)\n\n<task_result>\n{}\n</task_result>",
            answer.trim()
        );
        if !errors.is_empty() {
            content.push_str(&format!("\n(The subagent hit errors: {})", errors.join("; ")));
        }
        Ok(Output { content, display: Some(answer.trim().to_string()), diff: None, ..Default::default() })
    }
}
