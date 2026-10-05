//! webfetch: fetches a URL and returns it as readable text.

use std::path::Path;
use std::time::Duration;

use anyhow::{Result, bail};
use async_trait::async_trait;
use codeit_providers::ToolSpec;
use futures_util::StreamExt;
use serde_json::{Value, json};

use super::{Ctx, Output, Tool, arg, spec};
use crate::Harness;

const MAX_BYTES: usize = 5 * 1024 * 1024;

pub struct WebFetch;

#[async_trait]
impl Tool for WebFetch {
    fn name(&self) -> &str {
        "webfetch"
    }

    fn spec(&self, _: &Harness) -> ToolSpec {
        spec(
            "webfetch",
            "Fetch a URL and return its content: HTML pages as Markdown-like text (format `text`), or raw (`html`). \
Use it for documentation and pages the user mentions; don't guess URLs. Read-only.",
            json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string" },
                    "format": { "type": "string", "enum": ["text", "html"], "description": "Default: text" }
                },
                "required": ["url"]
            }),
        )
    }

    fn read_only(&self) -> bool {
        true
    }

    fn title(&self, input: &Value, _: &Path) -> String {
        input["url"].as_str().unwrap_or_default().to_string()
    }

    async fn run(&self, input: Value, ctx: &Ctx) -> Result<Output> {
        let mut url = arg(&input, "url")?.trim().to_string();
        if let Some(rest) = url.strip_prefix("http://") {
            url = format!("https://{rest}");
        }
        if !url.starts_with("https://") {
            bail!("The URL must start with http:// or https://");
        }
        ctx.ask("webfetch", std::slice::from_ref(&url), &["*".into()], format!("fetch {url}"), None).await?;
        let res = reqwest::Client::new()
            .get(&url)
            .header("user-agent", "Mozilla/5.0 (compatible; codeit-agent)")
            .header("accept", "text/html,text/plain,text/markdown,application/json;q=0.9,*/*;q=0.8")
            .timeout(Duration::from_secs(30))
            .send()
            .await?;
        let status = res.status();
        let kind = res.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_lowercase();
        let mut body = Vec::new();
        let mut stream = res.bytes_stream();
        while let Some(chunk) = stream.next().await {
            body.extend_from_slice(&chunk?);
            if body.len() > MAX_BYTES {
                bail!("The response is larger than 5 MB.");
            }
        }
        if !status.is_success() {
            bail!("HTTP {status} from {url}");
        }
        if kind.starts_with("image/") || kind.contains("octet-stream") || kind.contains("pdf") {
            bail!("{url} is {kind}, not text.");
        }
        let raw = String::from_utf8_lossy(&body).into_owned();
        let html = kind.contains("html") || raw.trim_start().starts_with('<');
        let text = if html && input["format"] != "html" {
            html2text::from_read(raw.as_bytes(), 120).unwrap_or(raw)
        } else {
            raw
        };
        Ok(Output {
            content: ctx.limit(text.trim()),
            display: Some(format!("{} bytes from {url}", body.len())),
            diff: None,
            images: Vec::new(),
            touched: Vec::new(),
        })
    }
}

/// Exa's public search endpoint (an MCP server, as opencode uses). `EXA_API_KEY` raises its limits.
fn exa_url() -> String {
    match std::env::var("EXA_API_KEY") {
        Ok(k) if !k.is_empty() => format!("https://mcp.exa.ai/mcp?exaApiKey={k}"),
        _ => "https://mcp.exa.ai/mcp".into(),
    }
}

/// The first text item of an MCP `tools/call` result, from a JSON or SSE body.
pub(crate) fn mcp_text(body: &str) -> Option<String> {
    let from = |payload: &str| -> Option<String> {
        let v: Value = serde_json::from_str(payload.trim()).ok()?;
        v["result"]["content"]
            .as_array()?
            .iter()
            .find_map(|c| c["text"].as_str().filter(|t| !t.is_empty()).map(String::from))
    };
    from(body).or_else(|| body.lines().filter_map(|l| l.strip_prefix("data: ")).find_map(from))
}

pub struct WebSearch;

#[async_trait]
impl Tool for WebSearch {
    fn name(&self) -> &str {
        "websearch"
    }

    fn spec(&self, _: &Harness) -> ToolSpec {
        let year = crate::prompt::today().get(..4).unwrap_or("2026").to_string();
        spec(
            "websearch",
            &format!(
                "Search the web and get the content of the most relevant pages. Use it for current information, \
documentation you don't know, and anything after your knowledge cutoff. The current year is {year}: use it when \
searching for recent things. Then use webfetch for a page you need in full."
            ),
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "numResults": { "type": "integer", "description": "Results to return (default 8)" },
                    "type": { "type": "string", "enum": ["auto", "fast", "deep"], "description": "Default: auto" }
                },
                "required": ["query"]
            }),
        )
    }

    fn read_only(&self) -> bool {
        true
    }

    fn title(&self, input: &Value, _: &Path) -> String {
        format!("\"{}\"", input["query"].as_str().unwrap_or_default())
    }

    async fn run(&self, input: Value, ctx: &Ctx) -> Result<Output> {
        let query = arg(&input, "query")?.to_string();
        ctx.ask(
            "websearch",
            std::slice::from_ref(&query),
            &["*".into()],
            format!("search the web for \"{query}\""),
            None,
        )
        .await?;
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "web_search_exa",
                "arguments": {
                    "query": query,
                    "type": input["type"].as_str().unwrap_or("auto"),
                    "numResults": input["numResults"].as_u64().unwrap_or(8).clamp(1, 20),
                    "livecrawl": "fallback",
                }
            }
        });
        let res = reqwest::Client::new()
            .post(exa_url())
            .header("accept", "application/json, text/event-stream")
            .header("user-agent", concat!("codeit/", env!("CARGO_PKG_VERSION")))
            .json(&body)
            .timeout(Duration::from_secs(25))
            .send()
            .await?;
        let status = res.status();
        let text = res.text().await?;
        if !status.is_success() {
            bail!("Web search failed: HTTP {status} {}", text.chars().take(200).collect::<String>());
        }
        let result = mcp_text(&text).unwrap_or_else(|| "No results. Try a different query.".into());
        Ok(Output {
            content: ctx.limit(&result),
            display: Some(format!("results for \"{query}\"")),
            diff: None,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_mcp_results_from_json_or_sse() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"Title: Rust"}]}}"#;
        assert_eq!(mcp_text(json).as_deref(), Some("Title: Rust"));
        assert_eq!(mcp_text(&format!("event: message\ndata: {json}\n\n")).as_deref(), Some("Title: Rust"));
        assert_eq!(mcp_text("nope"), None);
    }
}
