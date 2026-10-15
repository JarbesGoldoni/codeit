//! A session's conversation as the web app shows it: items built from the saved session, then
//! changed by the harness's events. Each change goes out as an [`Op`], so every open tab (and one
//! that reconnects) shows the same thing.

use std::collections::HashMap;
use std::time::Instant;

use codeit_harness::Harness;
use codeit_harness::event::Event;
use codeit_harness::session::Session;
use codeit_providers::{Part, Role};
use serde::Serialize;
use serde_json::Value;

/// Live output of a running command kept for the screen.
const LIVE_MAX: usize = 16_000;
const LIVE_KEEP: usize = 8_000;

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Item {
    User {
        text: String,
    },
    Assistant {
        text: String,
        reasoning: String,
        done: bool,
    },
    Tool(Box<Tool>),
    Notice {
        text: String,
        error: bool,
    },
    /// The end of a turn: who answered, and how long it took.
    TurnEnd {
        agent: String,
        model: String,
        effort: Option<String>,
        secs: Option<u64>,
    },
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Tool {
    pub id: String,
    pub name: String,
    pub title: String,
    /// pending, running, done or failed.
    pub state: &'static str,
    pub output: String,
    pub diff: Option<String>,
    pub lines: Option<usize>,
    pub digest: Option<String>,
    /// What a running command printed so far.
    pub live: String,
    /// A running subagent's latest steps.
    pub progress: Vec<String>,
    /// The call's arguments, when short.
    pub input: Option<Value>,
    pub secs: Option<f64>,
    #[serde(skip)]
    started: Option<Instant>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Push {
        item: Item,
    },
    /// Adds text to a field of item `index`: `text` or `reasoning` (replies), `live` (tools).
    Append {
        index: usize,
        field: &'static str,
        text: String,
    },
    Set {
        index: usize,
        item: Item,
    },
    Truncate {
        len: usize,
    },
}

#[derive(Default)]
pub struct Timeline {
    pub items: Vec<Item>,
}

/// Arguments worth sending along (a full command, a pattern), not a file's whole content.
fn short_input(input: &Value) -> Option<Value> {
    (input.to_string().len() <= 2_000).then(|| input.clone())
}

impl Timeline {
    pub fn push(&mut self, item: Item) -> Op {
        self.items.push(item.clone());
        Op::Push { item }
    }

    fn set(&self, index: usize) -> Op {
        Op::Set { index, item: self.items[index].clone() }
    }

    fn tool(&self, id: &str) -> Option<usize> {
        self.items.iter().rposition(|i| matches!(i, Item::Tool(t) if t.id == id))
    }

    /// Marks the reply being streamed as finished.
    fn close_text(&mut self, ops: &mut Vec<Op>) {
        let last = self.items.len().saturating_sub(1);
        if let Some(Item::Assistant { done, .. }) = self.items.last_mut()
            && !*done
        {
            *done = true;
            ops.push(self.set(last));
        }
    }

    /// Applies one of the harness's events (asks, todos, steps and titles are handled elsewhere).
    pub fn apply(&mut self, ev: &Event) -> Vec<Op> {
        let mut ops = Vec::new();
        match ev {
            Event::Text(t) | Event::Reasoning(t) => {
                let field = if matches!(ev, Event::Text(_)) { "text" } else { "reasoning" };
                let last = self.items.len().saturating_sub(1);
                match self.items.last_mut() {
                    Some(Item::Assistant { text, reasoning, done: false }) => {
                        if field == "text" { text } else { reasoning }.push_str(t);
                        ops.push(Op::Append { index: last, field, text: t.clone() });
                    }
                    _ => {
                        let (text, reasoning) =
                            if field == "text" { (t.clone(), String::new()) } else { (String::new(), t.clone()) };
                        ops.push(self.push(Item::Assistant { text, reasoning, done: false }));
                    }
                }
            }
            Event::Reset => {
                let before = self.items.len();
                while self.items.last().is_some_and(|i| match i {
                    Item::Assistant { done, .. } => !done,
                    Item::Tool(t) => t.state == "pending",
                    _ => false,
                }) {
                    self.items.pop();
                }
                if self.items.len() != before {
                    ops.push(Op::Truncate { len: self.items.len() });
                }
            }
            Event::ToolPending { id, name } => {
                self.close_text(&mut ops);
                ops.push(self.push(Item::Tool(Box::new(Tool {
                    id: id.clone(),
                    name: name.clone(),
                    state: "pending",
                    ..Default::default()
                }))));
            }
            Event::ToolStart { id, name, title, input } => {
                self.close_text(&mut ops);
                match self.tool(id) {
                    Some(i) => {
                        if let Item::Tool(t) = &mut self.items[i] {
                            t.title = title.clone();
                            t.state = "running";
                            t.input = short_input(input);
                            t.started = Some(Instant::now());
                        }
                        ops.push(self.set(i));
                    }
                    None => ops.push(self.push(Item::Tool(Box::new(Tool {
                        id: id.clone(),
                        name: name.clone(),
                        title: title.clone(),
                        state: "running",
                        input: short_input(input),
                        started: Some(Instant::now()),
                        ..Default::default()
                    })))),
                }
            }
            Event::ToolProgress { id, text } => {
                if let Some(i) = self.tool(id) {
                    if let Item::Tool(t) = &mut self.items[i] {
                        t.progress.push(text.clone());
                        if t.progress.len() > 3 {
                            t.progress.remove(0);
                        }
                    }
                    ops.push(self.set(i));
                }
            }
            Event::ToolOutput { id, text } => {
                if let Some(i) = self.tool(id)
                    && let Item::Tool(t) = &mut self.items[i]
                {
                    t.live.push_str(text);
                    if t.live.len() > LIVE_MAX {
                        let mut cut = t.live.len() - LIVE_KEEP;
                        while !t.live.is_char_boundary(cut) {
                            cut += 1;
                        }
                        t.live.drain(..cut);
                        ops.push(self.set(i));
                    } else {
                        ops.push(Op::Append { index: i, field: "live", text: text.clone() });
                    }
                }
            }
            Event::ToolDone { id, name, title, output, error, diff, lines, digest } => {
                let (i, new) = match self.tool(id) {
                    Some(i) => (i, false),
                    None => {
                        self.items.push(Item::Tool(Box::new(Tool {
                            id: id.clone(),
                            name: name.clone(),
                            ..Default::default()
                        })));
                        (self.items.len() - 1, true)
                    }
                };
                if let Item::Tool(t) = &mut self.items[i] {
                    if !title.is_empty() {
                        t.title = title.clone();
                    }
                    t.state = if *error { "failed" } else { "done" };
                    t.live.clear();
                    t.output = output.clone();
                    t.diff = diff.clone();
                    t.lines = *lines;
                    t.digest = digest.clone();
                    t.secs = t.started.map(|s| s.elapsed().as_secs_f64());
                }
                ops.push(if new { Op::Push { item: self.items[i].clone() } } else { self.set(i) });
            }
            Event::Notice(n) => {
                self.close_text(&mut ops);
                ops.push(self.push(Item::Notice { text: n.clone(), error: false }));
            }
            Event::Error(e) => {
                self.close_text(&mut ops);
                ops.push(self.push(Item::Notice { text: e.clone(), error: true }));
            }
            _ => {}
        }
        ops
    }

    /// The turn ended: the reply is finished, calls that never got a result stop, and the turn's
    /// label goes under it.
    pub fn end_turn(&mut self, end: Item) -> Vec<Op> {
        let mut ops = Vec::new();
        self.close_text(&mut ops);
        for i in 0..self.items.len() {
            if let Item::Tool(t) = &mut self.items[i]
                && matches!(t.state, "pending" | "running")
            {
                t.state = "failed";
                t.output = "Interrupted.".into();
                t.live.clear();
                ops.push(self.set(i));
            }
        }
        ops.push(self.push(end));
        ops
    }

    /// The conversation of a saved session, as far as the model still sees it.
    pub fn from_session(s: &Session, harness: &Harness, model_name: impl Fn(&str) -> String) -> Timeline {
        let mut items = Vec::new();
        if s.summary.is_some() {
            items
                .push(Item::Notice { text: "The conversation before this point was summarized.".into(), error: false });
        }
        let tools = harness.tools(
            s.model.as_deref().and_then(|m| m.split_once('/')).map(|m| m.1).unwrap_or(""),
            &Default::default(),
            0,
        );
        let results: HashMap<&str, (&str, bool)> = s
            .entries
            .iter()
            .flat_map(|e| e.message.parts.iter())
            .filter_map(|p| match p {
                Part::ToolResult { id, content, error } => Some((id.as_str(), (content.as_str(), *error))),
                _ => None,
            })
            .collect();
        // A turn's label: its agent, the model of its last reply, and how long it took.
        let mut turn: Option<(u64, String)> = None;
        let mut last: Option<(u64, Option<String>)> = None;
        let close =
            |items: &mut Vec<Item>, turn: &mut Option<(u64, String)>, last: &mut Option<(u64, Option<String>)>| {
                if let (Some((start, agent)), Some((end, model))) = (turn.take(), last.take()) {
                    items.push(Item::TurnEnd {
                        agent,
                        model: model.as_deref().map(&model_name).unwrap_or_default(),
                        effort: None,
                        secs: (end > start).then(|| end - start),
                    });
                }
            };
        for e in s.entries.iter().skip(s.context_start.min(s.entries.len())) {
            if let Some(p) = &e.prompt {
                close(&mut items, &mut turn, &mut last);
                items.push(Item::User { text: p.clone() });
                turn = Some((e.time, e.agent.clone().unwrap_or_else(|| s.agent.clone())));
                continue;
            }
            if e.message.role != Role::Assistant {
                continue;
            }
            last = Some((e.time, e.message.model.clone()));
            for p in &e.message.parts {
                match p {
                    Part::Reasoning { text, .. } if !text.trim().is_empty() => match items.last_mut() {
                        Some(Item::Assistant { reasoning, text: t, .. }) if t.is_empty() => reasoning.push_str(text),
                        _ => items.push(Item::Assistant { text: String::new(), reasoning: text.clone(), done: true }),
                    },
                    Part::Text { text } if !text.trim().is_empty() => match items.last_mut() {
                        Some(Item::Assistant { text: t, .. }) if t.is_empty() => t.push_str(text),
                        _ => items.push(Item::Assistant { text: text.clone(), reasoning: String::new(), done: true }),
                    },
                    Part::ToolCall(c) => {
                        let title = tools
                            .iter()
                            .find(|t| t.name() == c.name)
                            .map(|t| t.title(&c.input, &harness.cwd))
                            .unwrap_or_else(|| c.input.to_string().chars().take(80).collect());
                        let (output, error) = results.get(c.id.as_str()).copied().unwrap_or(("", true));
                        items.push(Item::Tool(Box::new(Tool {
                            id: c.id.clone(),
                            name: c.name.clone(),
                            title,
                            state: if error { "failed" } else { "done" },
                            output: output.lines().take(60).collect::<Vec<_>>().join("\n"),
                            lines: Some(output.lines().count()),
                            input: short_input(&c.input),
                            ..Default::default()
                        })));
                    }
                    _ => {}
                }
            }
        }
        close(&mut items, &mut turn, &mut last);
        Timeline { items }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_a_reply_then_a_tool_call() {
        let mut t = Timeline::default();
        let ops = t.apply(&Event::Reasoning("think".into()));
        assert!(matches!(ops[0], Op::Push { .. }));
        let ops = t.apply(&Event::Text("Hello".into()));
        assert!(matches!(ops[0], Op::Append { index: 0, field: "text", .. }));
        let ops = t.apply(&Event::ToolPending { id: "1".into(), name: "bash".into() });
        // The reply is closed, then the call is added.
        assert_eq!(ops.len(), 2);
        t.apply(&Event::ToolStart {
            id: "1".into(),
            name: "bash".into(),
            title: "ls".into(),
            input: serde_json::json!({"command": "ls"}),
        });
        let ops = t.apply(&Event::ToolOutput { id: "1".into(), text: "a\n".into() });
        assert!(matches!(ops[0], Op::Append { index: 1, field: "live", .. }));
        let ops = t.end_turn(Item::TurnEnd { agent: "build".into(), model: "m".into(), effort: None, secs: Some(1) });
        // The call never finished: it fails, and the label follows.
        match &t.items[1] {
            Item::Tool(tool) => assert_eq!(tool.state, "failed"),
            _ => panic!(),
        }
        assert_eq!(ops.len(), 2);
    }

    #[test]
    fn reset_drops_the_unfinished_reply() {
        let mut t = Timeline::default();
        t.push(Item::User { text: "hi".into() });
        t.apply(&Event::Text("partial".into()));
        let ops = t.apply(&Event::Reset);
        assert!(matches!(ops[0], Op::Truncate { len: 1 }));
    }
}
