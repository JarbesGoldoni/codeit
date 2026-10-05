//! lsp: code intelligence from the language servers (see `crate::lsp`).

use std::path::Path;
use std::time::Duration;

use anyhow::{Result, bail};
use async_trait::async_trait;
use codeit_providers::ToolSpec;
use serde_json::{Value, json};

use super::{Ctx, Output, Tool, arg, spec};
use crate::Harness;
use crate::lsp::{self, Lsp};

const OPERATIONS: &[(&str, &str)] = &[
    ("goToDefinition", "textDocument/definition"),
    ("findReferences", "textDocument/references"),
    ("hover", "textDocument/hover"),
    ("documentSymbol", "textDocument/documentSymbol"),
    ("workspaceSymbol", "workspace/symbol"),
    ("goToImplementation", "textDocument/implementation"),
    ("prepareCallHierarchy", "textDocument/prepareCallHierarchy"),
    ("incomingCalls", "callHierarchy/incomingCalls"),
    ("outgoingCalls", "callHierarchy/outgoingCalls"),
];

pub struct LspTool;

#[async_trait]
impl Tool for LspTool {
    fn name(&self) -> &str {
        "lsp"
    }

    fn spec(&self, _: &Harness) -> ToolSpec {
        spec(
            "lsp",
            "Code intelligence from the project's language server: goToDefinition, findReferences, hover (types \
and docs), documentSymbol, workspaceSymbol (with `query`), goToImplementation, prepareCallHierarchy, incomingCalls, \
outgoingCalls. Positions are 1-based, as in `read` output: point at the symbol's name. Prefer it over grep to follow \
a symbol precisely. Fails when no language server for the file type is installed.",
            json!({
                "type": "object",
                "properties": {
                    "operation": { "type": "string", "enum": OPERATIONS.iter().map(|o| o.0).collect::<Vec<_>>() },
                    "path": { "type": "string" },
                    "line": { "type": "integer", "description": "1-based" },
                    "character": { "type": "integer", "description": "1-based column" },
                    "query": { "type": "string", "description": "For workspaceSymbol" }
                },
                "required": ["operation", "path"]
            }),
        )
    }

    fn read_only(&self) -> bool {
        true
    }

    fn title(&self, input: &Value, _: &Path) -> String {
        let op = input["operation"].as_str().unwrap_or("lsp");
        match (input["path"].as_str(), input["line"].as_u64()) {
            (Some(p), Some(l)) => format!("{op} {p}:{l}"),
            (Some(p), None) => format!("{op} {p}"),
            _ => op.to_string(),
        }
    }

    async fn run(&self, input: Value, ctx: &Ctx) -> Result<Output> {
        let operation = arg(&input, "operation")?;
        let Some((_, method)) = OPERATIONS.iter().find(|o| o.0 == operation) else {
            bail!("Unknown operation {operation}.");
        };
        let path = ctx.resolve(arg(&input, "path")?);
        ctx.check_path(&path, "read", None).await?;
        if !path.is_file() {
            bail!("{} is not a file.", ctx.display(&path));
        }
        let h = &ctx.harness;
        let clients = h.lsp.clients(&h.servers, &h.root, &path).await?;
        let line = input["line"].as_u64().unwrap_or(1).max(1) - 1;
        let character = input["character"].as_u64().unwrap_or(1).max(1) - 1;
        let doc = json!({ "uri": lsp::uri(&lsp::real(&path)) });
        let pos = json!({ "textDocument": doc, "position": { "line": line, "character": character } });
        let mut out = Vec::new();
        for c in clients {
            c.touch(&path, lsp::language(&path));
            let timeout = Duration::from_secs(30);
            let result = match operation {
                "findReferences" => {
                    let mut p = pos.clone();
                    p["context"] = json!({ "includeDeclaration": true });
                    c.request(method, p, timeout).await?
                }
                "documentSymbol" => c.request(method, json!({ "textDocument": doc }), timeout).await?,
                "workspaceSymbol" => {
                    c.request(method, json!({ "query": input["query"].as_str().unwrap_or("") }), timeout).await?
                }
                "incomingCalls" | "outgoingCalls" => {
                    let items = c.request("textDocument/prepareCallHierarchy", pos.clone(), timeout).await?;
                    match items.as_array().and_then(|a| a.first()) {
                        Some(item) => c.request(method, json!({ "item": item }), timeout).await?,
                        None => Value::Null,
                    }
                }
                _ => c.request(method, pos.clone(), timeout).await?,
            };
            out.push(lsp::format(operation, &result, ctx.cwd()));
        }
        Ok(Output::text(ctx.limit(&out.join("\n\n"))))
    }
}

/// The errors language servers report for files an edit just wrote, as a note for the model.
pub async fn errors_after_edit(h: &Harness, files: &[std::path::PathBuf]) -> Option<String> {
    let mut notes = Vec::new();
    for f in files {
        if Lsp::handles(&h.servers, f).is_empty() {
            continue;
        }
        let errors = h.lsp.errors(&h.servers, &h.root, f, Duration::from_secs(4)).await;
        if let Some(r) = lsp::report(&errors) {
            notes.push(format!(
                "<diagnostics file=\"{}\">\nThe language server reports errors in this file:\n{r}\n</diagnostics>",
                crate::util::display(&h.cwd, f)
            ));
        }
    }
    (!notes.is_empty()).then(|| notes.join("\n"))
}
