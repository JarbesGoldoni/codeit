//! todo: the model's task list, shown to the user as it changes.

use std::path::Path;

use anyhow::{Result, bail};
use async_trait::async_trait;
use codeit_providers::ToolSpec;
use serde_json::{Value, json};

use super::{Ctx, Output, Tool, spec};
use crate::Harness;
use crate::event::Event;
use crate::session::Todo as Item;

pub struct Todo;

const STATES: [&str; 4] = ["pending", "in_progress", "completed", "cancelled"];

#[async_trait]
impl Tool for Todo {
    fn name(&self) -> &str {
        "todo"
    }

    fn spec(&self, _: &Harness) -> ToolSpec {
        spec(
            "todo",
            "Write the task list for the current work; the user sees it. Use it for work with 3 or more steps or when \
the user gives several tasks. Send the whole list each time. Keep exactly one item in_progress while working, and mark \
an item completed right after finishing it (only when it is really done, verification included). Skip it for simple \
or purely conversational requests.",
            json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": { "type": "string", "description": "Specific, actionable step" },
                                "status": { "type": "string", "enum": STATES }
                            },
                            "required": ["content", "status"]
                        }
                    }
                },
                "required": ["todos"]
            }),
        )
    }

    fn title(&self, input: &Value, _: &Path) -> String {
        let n = input["todos"].as_array().map(Vec::len).unwrap_or(0);
        format!("{n} items")
    }

    async fn run(&self, input: Value, ctx: &Ctx) -> Result<Output> {
        let Some(list) = input["todos"].as_array() else { bail!("todos must be an array") };
        let mut todos = Vec::new();
        for t in list {
            let content = t["content"].as_str().unwrap_or_default().trim().to_string();
            let status = t["status"].as_str().unwrap_or("pending").to_string();
            if !STATES.contains(&status.as_str()) {
                bail!("unknown status `{status}` (use one of {})", STATES.join(", "));
            }
            if !content.is_empty() {
                todos.push(Item { content, status });
            }
        }
        let count = |s: &str| todos.iter().filter(|t| t.status == s).count();
        let summary = format!(
            "Todo list updated: {} pending, {} in progress, {} completed{}.",
            count("pending"),
            count("in_progress"),
            count("completed"),
            match count("cancelled") {
                0 => String::new(),
                n => format!(", {n} cancelled"),
            }
        );
        ctx.session.lock().unwrap().todos = todos.clone();
        let _ = ctx.events.send(Event::Todos(todos));
        Ok(Output::text(summary))
    }
}
