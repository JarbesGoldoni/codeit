//! A session as Markdown, for `/export`.

use codeit_harness::session::Session;
use codeit_providers::{Part, Role};

pub fn markdown(s: &Session) -> String {
    let mut out = format!("# {}\n\n", if s.title.is_empty() { "Session" } else { &s.title });
    out.push_str(&format!(
        "- folder: `{}`\n- model: {}\n- session: `{}`\n\n",
        s.cwd.display(),
        s.model.as_deref().unwrap_or("?"),
        s.id
    ));
    if let Some(sum) = &s.summary {
        out.push_str(&format!("## Earlier (summarized)\n\n{sum}\n\n"));
    }
    for e in s.entries.iter().skip(s.context_start.min(s.entries.len())) {
        if let Some(p) = &e.prompt {
            out.push_str(&format!("## You\n\n{p}\n\n"));
            continue;
        }
        for p in &e.message.parts {
            match (e.message.role, p) {
                (Role::Assistant, Part::Text { text }) if !text.trim().is_empty() => {
                    out.push_str(&format!("## codeit\n\n{}\n\n", text.trim()));
                }
                (Role::Assistant, Part::ToolCall(c)) => {
                    let args: String = c.input.to_string().chars().take(300).collect();
                    out.push_str(&format!("> **{}** `{args}`\n\n", c.name));
                }
                (Role::User, Part::ToolResult { content, error, .. }) => {
                    let lines: Vec<&str> = content.lines().collect();
                    let shown = lines.iter().take(20).copied().collect::<Vec<_>>().join("\n");
                    let more =
                        if lines.len() > 20 { format!("\n... {} more lines", lines.len() - 20) } else { String::new() };
                    let tag = if *error { " (error)" } else { "" };
                    out.push_str(&format!(
                        "<details><summary>result{tag}</summary>\n\n```\n{shown}{more}\n```\n\n</details>\n\n"
                    ));
                }
                _ => {}
            }
        }
    }
    out
}
