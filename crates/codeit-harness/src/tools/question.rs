//! question: asks the user one or more multiple-choice questions and waits for the answers.

use std::path::Path;

use anyhow::{Result, bail};
use async_trait::async_trait;
use codeit_providers::ToolSpec;
use serde_json::{Value, json};

use super::{Ctx, Output, Tool, refused_by_user, spec};
use crate::Harness;
use crate::event::{Ask, Event, Question, QuestionAsk};

pub struct QuestionTool;

#[async_trait]
impl Tool for QuestionTool {
    fn name(&self) -> &str {
        "question"
    }

    fn spec(&self, _: &Harness) -> ToolSpec {
        spec(
            "question",
            "Ask the user questions when a decision is really theirs: unclear requirements, a choice between \
approaches, preferences. Each question offers 2-4 options; the user can also type their own answer, so don't add an \
\"Other\" option. Put a recommended option first and end its label with \"(Recommended)\". Don't ask what you can find \
out yourself.",
            json!({
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "question": { "type": "string" },
                                "header": { "type": "string", "description": "Very short label (max 12 chars)" },
                                "options": {
                                    "type": "array",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "label": { "type": "string" },
                                            "description": { "type": "string" }
                                        },
                                        "required": ["label"]
                                    }
                                },
                                "multiple": { "type": "boolean", "description": "Allow choosing several options" }
                            },
                            "required": ["question", "options"]
                        }
                    }
                },
                "required": ["questions"]
            }),
        )
    }

    fn title(&self, input: &Value, _: &Path) -> String {
        input["questions"][0]["question"].as_str().unwrap_or_default().to_string()
    }

    async fn run(&self, input: Value, ctx: &Ctx) -> Result<Output> {
        let Some(list) = input["questions"].as_array().filter(|l| !l.is_empty()) else {
            bail!("questions must be a non-empty array")
        };
        let questions: Vec<Question> = list
            .iter()
            .map(|q| Question {
                question: q["question"].as_str().unwrap_or_default().into(),
                header: q["header"].as_str().unwrap_or_default().into(),
                options: q["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|o| {
                        (
                            o["label"].as_str().or(o.as_str()).unwrap_or_default().to_string(),
                            o["description"].as_str().unwrap_or_default().to_string(),
                        )
                    })
                    .collect(),
                multiple: q["multiple"] == true,
            })
            .collect();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = ctx.events.send(Event::Ask(Ask::Question(QuestionAsk { questions: questions.clone(), reply: tx })));
        let answers = tokio::select! {
            a = rx => a.ok().flatten(),
            _ = ctx.cancel.cancelled() => None,
        };
        let Some(answers) = answers else {
            return Err(refused_by_user(Some("(dismissed the questions)".into())).into());
        };
        let mut out = String::from("The user answered:\n");
        for (q, a) in questions.iter().zip(answers.iter()) {
            out.push_str(&format!(
                "- {} → {}\n",
                q.question,
                if a.is_empty() { "(no answer)".into() } else { a.join(", ") }
            ));
        }
        Ok(Output::text(out.trim_end()))
    }
}
