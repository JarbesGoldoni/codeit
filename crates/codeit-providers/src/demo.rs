//! A scripted provider for trying the interface without an account: `CODEIT_DEMO=1 codeit`.
//!
//! Each prompt plays the same short coding session in the working directory: it reads
//! `calc.py`, searches, fixes `add`, runs a long failing command and a passing one, then
//! answers. Requests without tools (summaries, condensed tool output) get a canned answer, and
//! when a `review_comment` tool is offered it leaves review comments. Create the files with:
//!
//! ```sh
//! printf 'def add(a, b):\n    return a - b\n' > calc.py
//! ```

use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::{AuthStatus, ChatEvent, ChatRequest, ModelInfo, Part, Provider, Role, ToolCall, Usage};

pub struct Demo;

/// True when `CODEIT_DEMO` is set: the demo provider is added to the real ones.
pub fn enabled() -> bool {
    std::env::var_os("CODEIT_DEMO").is_some_and(|v| !v.is_empty() && v != "0")
}

fn model(id: &str, name: &str, context: u64) -> ModelInfo {
    ModelInfo {
        provider: "demo",
        id: id.into(),
        name: name.into(),
        context: Some(context),
        max_input: Some(context),
        efforts: vec!["low".into(), "high".into()],
        default_effort: None,
        vision: true,
    }
}

/// Model requests since the person's latest message.
fn step(req: &ChatRequest) -> usize {
    let last_prompt = req
        .messages
        .iter()
        .rposition(|m| m.role == Role::User && m.parts.iter().any(|p| matches!(p, Part::Text { .. })))
        .unwrap_or(0);
    req.messages[last_prompt..].iter().filter(|m| m.role == Role::Assistant).count()
}

fn call(name: &str, input: Value) -> (String, Value) {
    (name.to_string(), input)
}

#[async_trait]
impl Provider for Demo {
    fn id(&self) -> &'static str {
        "demo"
    }
    fn name(&self) -> &'static str {
        "Demo"
    }
    async fn status(&self) -> AuthStatus {
        AuthStatus::LoggedIn("scripted, no account".into())
    }
    async fn models(&self) -> Result<Vec<ModelInfo>> {
        Ok(vec![
            model("thinker", "Demo Thinker", 200_000),
            model("coder", "Demo Coder", 200_000),
            model("mini", "Demo Mini", 64_000),
        ])
    }

    async fn chat(&self, req: ChatRequest, events: UnboundedSender<ChatEvent>) -> Result<()> {
        let send = |e: ChatEvent| {
            let _ = events.send(e);
        };
        let pause = || tokio::time::sleep(Duration::from_millis(350));
        let usage = Usage { input_tokens: 4200, output_tokens: 180, cached_tokens: 3000, credits: None };

        // Summaries and condensed tool output: no tools offered.
        if req.tools.is_empty() {
            pause().await;
            let text = if req.messages.iter().any(|m| m.text().contains("<tool-output")) {
                "Ran a build of 600 modules; it failed.\nKey results:\nerror: test_add failed (expected 3, got -1)\n600 modules compiled before the failure"
            } else {
                "## Objective\nFix add in calc.py.\n## Done\nFixed the sign; tests pass."
            };
            send(ChatEvent::Text(text.into()));
            send(ChatEvent::Usage(Usage { input_tokens: 900, output_tokens: 60, ..Default::default() }));
            return Ok(());
        }

        // A review: comment on the first changed file it was told about, then summarize.
        if req.tools.iter().any(|t| t.name == "review_comment") {
            pause().await;
            let calls = if step(&req) == 0 {
                let path = req
                    .messages
                    .iter()
                    .flat_map(|m| m.text().lines().map(str::to_string).collect::<Vec<_>>())
                    .find_map(|l| {
                        l.trim().strip_prefix("- ").map(|p| p.split_whitespace().next().unwrap_or("").to_string())
                    })
                    .unwrap_or_else(|| "calc.py".into());
                vec![
                    call(
                        "review_comment",
                        json!({"path": path, "line": 2, "severity": "bug", "body": "This still subtracts: `add(1, 2)` returns -1. Use `a + b`."}),
                    ),
                    call(
                        "review_comment",
                        json!({"path": path, "line": 1, "severity": "nit", "body": "A docstring would help callers."}),
                    ),
                ]
            } else {
                vec![call(
                    "text",
                    json!("One real bug (the sign in `add`) and a small nit. Fix the bug before merging."),
                )]
            };
            emit(&send, calls);
            send(ChatEvent::Usage(usage));
            return Ok(());
        }

        let script: Vec<Vec<(String, Value)>> = vec![
            vec![
                call(
                    "reasoning",
                    json!("The user wants the bug fixed. Start by reading calc.py and finding callers of add."),
                ),
                call("read", json!({"path": "calc.py"})),
                call("grep", json!({"pattern": "add\\("})),
            ],
            vec![
                call("reasoning", json!("add subtracts; fix the operator.")),
                call("edit", json!({"path": "calc.py", "old_string": "return a - b", "new_string": "return a + b"})),
            ],
            vec![call(
                "bash",
                json!({"command": "for i in $(seq 1 600); do echo \"compiling module_$i\"; done; echo 'error: test_add failed (expected 3, got -1)'; exit 1", "description": "Run the build"}),
            )],
            vec![call(
                "bash",
                json!({"command": "python3 -c 'from calc import add; print(add(1, 2))'", "description": "Check add"}),
            )],
            vec![call(
                "text",
                json!(
                    "Fixed `add` in **calc.py**: it subtracted instead of adding.\n\n| call | before | after |\n|---|---:|---:|\n| `add(1, 2)` | -1 | 3 |\n| `add(5, 5)` | 0 | 10 |\n\nThe fix:\n\n```python\ndef add(a, b):\n    return a + b  # was a - b\n```\n\n- the build log was condensed\n- the check passes"
                ),
            )],
        ];
        let n = step(&req).min(script.len() - 1);
        pause().await;
        emit(&send, script[n].clone());
        send(ChatEvent::Usage(usage));
        Ok(())
    }
}

fn emit(send: &impl Fn(ChatEvent), calls: Vec<(String, Value)>) {
    for (name, input) in calls {
        match name.as_str() {
            "text" => send(ChatEvent::Text(input.as_str().unwrap_or_default().into())),
            "reasoning" => send(ChatEvent::Reasoning(input.as_str().unwrap_or_default().into())),
            _ => {
                let id = format!("demo_{}", uuid_like());
                send(ChatEvent::ToolStart { id: id.clone(), name: name.clone() });
                send(ChatEvent::ToolCall(ToolCall { id, name, input }));
            }
        }
    }
}

fn uuid_like() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    format!("{t:x}{}", N.fetch_add(1, Ordering::Relaxed))
}
