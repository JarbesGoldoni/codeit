//! Plugin panels on the wire: an extension's [`Reply`] as JSON for the web app, and the app's
//! actions back.

use codeit_harness::extension::{Action, Panel, Reply, Tone};
use serde::Deserialize;
use serde_json::{Value, json};

pub fn tone(t: Tone) -> &'static str {
    match t {
        Tone::Normal => "normal",
        Tone::Ok => "ok",
        Tone::Warn => "warn",
        Tone::Bad => "bad",
        Tone::Muted => "muted",
        Tone::Accent => "accent",
    }
}

fn panel(p: &Panel) -> Value {
    json!({
        "title": p.title,
        "sections": p.sections.iter().map(|s| json!({
            "title": s.title,
            "rows": s.rows.iter().map(|r| json!({
                "id": r.id,
                "label": r.label,
                "value": r.value,
                "tone": tone(r.tone),
                "detail": r.detail,
                "bar": r.bar,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "message": p.message.as_ref().map(|(m, t)| json!({"text": m, "tone": tone(*t)})),
        "keys": p.keys.iter().map(|(k, what)| json!({"key": k.to_string(), "label": what})).collect::<Vec<_>>(),
        "poll_ms": p.poll_ms,
        "select": p.select,
    })
}

/// The reply as the app gets it. `Prompt` is handled by the server before this (it opens the
/// session and sends the text), so the app only hears that the panel closed.
pub fn reply(r: &Reply) -> Value {
    match r {
        Reply::Panel(p) => json!({"kind": "panel", "panel": panel(p)}),
        Reply::Input { title, hint, secret } => {
            json!({"kind": "input", "title": title, "hint": hint, "secret": secret})
        }
        Reply::Notice(text) => json!({"kind": "notice", "text": text}),
        Reply::Prompt { .. } | Reply::Close => json!({"kind": "close"}),
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WireAction {
    Open { args: String },
    Enter { id: String },
    Key { key: String, id: Option<String> },
    Text { text: String },
    Poll,
}

impl WireAction {
    pub fn action(self) -> Option<Action> {
        Some(match self {
            WireAction::Open { args } => Action::Open(args),
            WireAction::Enter { id } => Action::Enter(id),
            WireAction::Key { key, id } => Action::Key(key.chars().next()?, id),
            WireAction::Text { text } => Action::Text(text),
            WireAction::Poll => Action::Poll,
        })
    }
}
