//! The conversation as lines: your messages in blue bubbles, the agent's thinking as gray text,
//! its finished actions in a faint frame, and its answer in a gray bubble with the agent, model,
//! effort and time in the corner. An action still running isn't drawn here: the status line
//! shows it until it finishes.
//!
//! Actions show at one of three levels (ctrl+o): one summary line per group, a list, or each
//! call with its output trimmed. ↑/↓ select an action (or a folded group); Enter opens it in
//! full or closes it.

use ratatui::{
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
};

use super::app::{App, Item, ToolItem, ToolState, TurnInfo, duration};
use super::style::{
    ADD, ADD_BG, AGENT, BLUE, DEL_BG, FAINT, SELECT, SOFT_RED, cut, frame, pad, right, wrap, wrap_spans,
};

/// A line, and the action (or folded group) it belongs to: `a:<tool id>` or `g:<first tool id>`.
pub type Row = (Line<'static>, Option<String>);

pub const LEVELS: [&str; 3] = ["folded", "list", "open"];

pub fn action_key(t: &ToolItem) -> String {
    format!("a:{}", t.id)
}

fn group_key(first: &ToolItem) -> String {
    format!("g:{}", first.id)
}

fn bash_exit(output: &str) -> Option<&str> {
    output.lines().next()?.strip_prefix("Exit code ")
}

/// Whether an action went wrong (a failed call, or a command that exited non-zero).
pub fn failed(t: &ToolItem) -> bool {
    t.state == ToolState::Failed || (t.name == "bash" && bash_exit(&t.output).is_some())
}

fn diff_counts(diff: &str) -> (usize, usize) {
    let lines = diff.lines().filter(|l| !l.starts_with("+++") && !l.starts_with("---"));
    lines.fold((0, 0), |(a, r), l| match l.chars().next() {
        Some('+') => (a + 1, r),
        Some('-') => (a, r + 1),
        _ => (a, r),
    })
}

fn plural(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

fn took(t: &ToolItem) -> String {
    match t.elapsed {
        Some(d) if d.as_millis() >= 500 => {
            let s = d.as_secs_f64();
            if s >= 60.0 { duration(d.as_secs()) } else { format!("{s:.1}s") }
        }
        _ => String::new(),
    }
}

fn output_lines(t: &ToolItem) -> usize {
    let body = t.output.trim();
    if body.is_empty() || body == "(no output)" {
        return 0;
    }
    t.lines.unwrap_or_else(|| body.lines().count().saturating_sub(bash_exit(body).is_some() as usize))
}

/// What an action came to, on the right of its row.
fn result(t: &ToolItem) -> Vec<Span<'static>> {
    let gray = |s: String| vec![s.fg(Color::DarkGray)];
    if failed(t) {
        let what = match bash_exit(&t.output) {
            Some(code) => format!("exit {code}"),
            None if t.output == "Interrupted." => "interrupted".into(),
            None => "failed".into(),
        };
        return vec![what.fg(SOFT_RED)];
    }
    match t.name.as_str() {
        "edit" | "write" | "apply_patch" => match &t.diff {
            Some(d) => {
                let (a, r) = diff_counts(d);
                vec![format!("+{a}").fg(ADD), " ".into(), format!("−{r}").fg(SOFT_RED)]
            }
            None if t.output.starts_with("Created") => gray("new file".into()),
            None => Vec::new(),
        },
        "read" => gray(plural(output_lines(t), "line")),
        "grep" => gray(plural(output_lines(t), "match").replace("matchs", "matches")),
        "glob" => gray(plural(output_lines(t), "file")),
        "bash" => match output_lines(t) {
            0 => gray("ok".into()),
            n => gray(plural(n, "line")),
        },
        "task" => gray("done".into()),
        _ => Vec::new(),
    }
}

fn header(t: &ToolItem, w: usize, opened: bool) -> Line<'static> {
    let name = cut(&t.name, 14);
    let target =
        if t.title.is_empty() { String::new() } else { t.title.lines().next().unwrap_or_default().to_string() };
    let mut left = vec![
        Span::styled(format!("{name:<5}"), Style::new().fg(Color::Gray)),
        " ".into(),
        cut(&target, w.saturating_sub(name.len().max(5) + 24)).into(),
    ];
    if opened {
        left.push("  ▾".dim());
    }
    let mut r = result(t);
    let time = took(t);
    if !time.is_empty() {
        r.push(format!("  {time}").fg(Color::DarkGray));
    }
    right(left, r, w)
}

fn is_error_line(l: &str) -> bool {
    let low = l.to_lowercase();
    ["error", "fail", "panic", "denied", "not found", "exception", "traceback"].iter().any(|k| low.contains(k))
}

/// An action's output under its row: trimmed (the errors, or the end), or in full.
fn body(t: &ToolItem, full: bool, w: usize) -> Vec<Line<'static>> {
    let lead = "  │ ";
    let room = w.saturating_sub(5);
    let mut out = Vec::new();
    let line = |text: &str, st: Style| pad(vec![lead.dim(), Span::styled(cut(text, room), st)], w, Style::new());
    let (all, diff): (Vec<String>, bool) = match (&t.diff, t.name.as_str()) {
        (Some(d), "edit" | "write" | "apply_patch") => {
            (d.lines().filter(|l| !l.starts_with("+++") && !l.starts_with("---")).map(String::from).collect(), true)
        }
        _ => {
            let text = t.output.trim_end();
            let text = text.split("\n\n(Output was long:").next().unwrap_or(text);
            let mut lines: Vec<String> = text.lines().map(String::from).collect();
            if bash_exit(text).is_some() {
                lines.remove(0);
            }
            if lines.len() == 1 && lines[0] == "(no output)" {
                lines.clear();
            }
            (lines, false)
        }
    };
    if all.is_empty() {
        return out;
    }
    let shown: Vec<&String> = if full {
        all.iter().take(400).collect()
    } else if diff {
        all.iter().filter(|l| l.starts_with(['+', '-'])).take(4).collect()
    } else if failed(t) {
        let errors: Vec<&String> = all.iter().filter(|l| is_error_line(l)).take(3).collect();
        if errors.is_empty() { all.iter().rev().take(3).rev().collect() } else { errors }
    } else if t.name == "bash" {
        all.iter().rev().take(2).rev().collect()
    } else {
        all.iter().take(2).collect()
    };
    for l in &shown {
        let st = match (diff, l.chars().next()) {
            (true, Some('+')) => Style::new().bg(ADD_BG).fg(Color::Rgb(160, 230, 160)),
            (true, Some('-')) => Style::new().bg(DEL_BG).fg(Color::Rgb(240, 160, 160)),
            (true, Some('@')) => Style::new().fg(Color::DarkGray),
            _ if failed(t) && is_error_line(l) => Style::new().fg(SOFT_RED),
            _ => Style::new().fg(Color::Gray),
        };
        let text = if diff && l.starts_with("@@") { "⋮".to_string() } else { l.to_string() };
        let mut row = line(&text, st);
        if st.bg.is_some() {
            row = pad(vec![lead.dim(), Span::styled(cut(&text, room), st)], w, st);
        }
        out.push(row);
    }
    let hidden = all.len().saturating_sub(shown.len());
    if full && t.digest.is_some() {
        out.push(Line::from(vec!["  ╰ ".dim(), "the model got a condensed version of this".dim().italic()]));
    } else if hidden > 0 {
        let what = if failed(t) && !full { "the errors of " } else { "" };
        out.push(Line::from(vec![
            "  ╰ ".dim(),
            format!("{what}{} · {hidden} more · enter shows all", plural(all.len(), "line")).dim().italic(),
        ]));
    }
    out
}

/// `▸ 4 actions · read 2 · edited 1 · ran 1 · 1 failed · 8.2s`.
fn summary(tools: &[&ToolItem], open: bool) -> Line<'static> {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for t in tools {
        let word = match t.name.as_str() {
            "read" => "read",
            "grep" | "glob" => "searched",
            "edit" | "write" | "apply_patch" => "edited",
            "bash" => "ran",
            "webfetch" => "fetched",
            "websearch" => "searched the web",
            "task" => "delegated",
            "skill" => "loaded skills",
            _ => "called",
        };
        match counts.iter_mut().find(|c| c.0 == word) {
            Some(c) => c.1 += 1,
            None => counts.push((word, 1)),
        }
    }
    let mut out =
        vec![if open { "▾ ".dim() } else { "▸ ".dim() }, plural(tools.len(), "action").fg(Color::Gray).bold()];
    for (word, n) in counts {
        out.push(" · ".dim());
        out.push(format!("{word} {n}").fg(Color::Gray));
    }
    let bad = tools.iter().filter(|t| failed(t)).count();
    if bad > 0 {
        out.push(" · ".dim());
        out.push(format!("{bad} failed").fg(SOFT_RED));
    }
    let secs: f64 = tools.iter().filter_map(|t| t.elapsed).map(|d| d.as_secs_f64()).sum();
    if secs >= 0.5 {
        out.push(format!(" · {secs:.1}s").dim());
    }
    Line::from(out)
}

/// The rows of one group of finished actions, before framing.
fn group_rows(app: &App, tools: &[&ToolItem], w: usize) -> Vec<Row> {
    let mut out: Vec<Row> = Vec::new();
    let key = group_key(tools[0]);
    let unfolded = app.open.contains(&key);
    if app.level == 0 {
        out.push((summary(tools, unfolded), Some(key)));
        if !unfolded {
            return out;
        }
    }
    for t in tools {
        let k = action_key(t);
        let full = app.open.contains(&k);
        out.push((header(t, w, full), Some(k.clone())));
        if full || app.level == 2 {
            out.extend(body(t, full, w).into_iter().map(|l| (l, Some(k.clone()))));
        }
    }
    out
}

/// The label in the corner of an answer: `build · GLM-5.1 · high · 38s`.
pub fn turn_label(info: &TurnInfo) -> Vec<Span<'static>> {
    let mut rest = format!(" · {}", info.model);
    if let Some(e) = &info.effort {
        rest.push_str(&format!(" · {e}"));
    }
    if let Some(s) = info.secs {
        rest.push_str(&format!(" · {}", duration(s)));
    }
    vec![info.agent.clone().fg(Color::Cyan), rest.dim()]
}

/// Gray text lined up with the text inside the bubbles.
fn gray_text(text: &str, w: usize, italic: bool, out: &mut Vec<Row>) {
    for l in
        super::markdown::render(text, w.saturating_sub(4)).into_iter().flat_map(|l| wrap_spans(l, w.saturating_sub(4)))
    {
        let mut spans = vec![Span::raw("  ")];
        spans.extend(l.spans.into_iter().map(|s| {
            let mut st = s.style.fg(Color::DarkGray);
            if italic {
                st = st.add_modifier(Modifier::ITALIC);
            }
            Span::styled(s.content, st)
        }));
        out.push((Line::from(spans), None));
    }
}

fn todo_rows(todos: &[codeit_harness::session::Todo], out: &mut Vec<Row>) {
    for t in todos {
        let (icon, style) = match t.status.as_str() {
            "completed" => ("✔ ", Style::new().fg(Color::DarkGray).add_modifier(Modifier::CROSSED_OUT)),
            "in_progress" => ("◼ ", Style::new().fg(Color::Gray).bold()),
            "cancelled" => ("✗ ", Style::new().fg(Color::DarkGray).add_modifier(Modifier::CROSSED_OUT)),
            _ => ("□ ", Style::new().fg(Color::Gray)),
        };
        out.push((
            Line::from(vec!["  ".into(), Span::styled(icon, style), Span::styled(t.content.clone(), style)]),
            None,
        ));
    }
}

fn blank(out: &mut Vec<Row>) {
    if out.last().is_some_and(|r| !r.0.spans.iter().all(|s| s.content.trim().is_empty())) {
        out.push((Line::default(), None));
    }
}

/// Highlights the selected action's lines (before they are framed).
fn mark(rows: Vec<Row>, selected: Option<&str>, w: usize) -> Vec<Row> {
    rows.into_iter()
        .map(|(l, k)| {
            if k.is_some() && k.as_deref() == selected {
                let spans: Vec<Span<'static>> =
                    l.spans.into_iter().map(|s| if s.style.bg.is_none() { s.bg(SELECT) } else { s }).collect();
                (pad(spans, w, Style::new().bg(SELECT)), k)
            } else {
                (l, k)
            }
        })
        .collect()
}

/// The whole conversation, `w` columns wide.
pub fn rows(app: &App, w: usize) -> Vec<Row> {
    let mut out: Vec<Row> = Vec::new();
    let items = &app.items;
    if items.is_empty() {
        out.push((Line::from("  Describe a task to start. /model picks a model, /login adds a provider.".dim()), None));
        out.push((Line::from("  /help lists commands; tab switches between build and plan (read-only).".dim()), None));
        return out;
    }
    let selected = app.selected.as_deref();
    let inner = w.saturating_sub(4);
    let last_todos = items.iter().rposition(|i| matches!(i, Item::Todos(_)));
    let mut group: Vec<&ToolItem> = Vec::new();
    let flush = |group: &mut Vec<&ToolItem>, out: &mut Vec<Row>| {
        if group.is_empty() {
            return;
        }
        blank(out);
        let rows = mark(group_rows(app, group, inner), selected, inner);
        out.extend(frame(rows, w, FAINT, Vec::new(), None));
        group.clear();
    };
    let mut labelled = false;
    for (i, item) in items.iter().enumerate() {
        if !matches!(item, Item::Tool(_)) {
            flush(&mut group, &mut out);
        }
        match item {
            Item::User(text) => {
                blank(&mut out);
                let lines = wrap(text, inner).into_iter().map(|l| (Line::from(l.white()), None)).collect();
                out.extend(frame(lines, w, BLUE, Vec::new(), None));
                labelled = false;
            }
            Item::Assistant { text, reasoning, done } => {
                if !reasoning.trim().is_empty() {
                    blank(&mut out);
                    gray_text(reasoning.trim(), w, true, &mut out);
                }
                if text.trim().is_empty() {
                    continue;
                }
                // The answer is the text no action follows in its turn; text between actions
                // is the agent talking while it works, shown like its thinking.
                let rest = items[i + 1..].iter().take_while(|x| !matches!(x, Item::User(_)));
                let answer =
                    rest.clone().all(|x| !matches!(x, Item::Tool(_))) && (*done || !app.busy() || i + 1 == items.len());
                blank(&mut out);
                if answer {
                    let label = rest
                        .clone()
                        .find_map(|x| if let Item::TurnEnd(t) = x { Some(turn_label(t)) } else { None })
                        .unwrap_or_default();
                    labelled = !label.is_empty();
                    let body = super::markdown::render(text, inner).into_iter().map(|l| (l, None)).collect();
                    out.extend(frame(body, w, AGENT, label, None));
                } else {
                    gray_text(text.trim(), w, false, &mut out);
                }
            }
            Item::Tool(t) if t.running() => {}
            Item::Tool(t) if t.name == "todo" && !failed(t) => {}
            Item::Tool(t) => group.push(t),
            Item::Todos(todos) if Some(i) == last_todos => {
                blank(&mut out);
                todo_rows(todos, &mut out);
            }
            Item::Todos(_) => {}
            // An answer carries its turn's label; a turn that ended without one gets it alone.
            Item::TurnEnd(info) if !labelled => {
                let mut l = turn_label(info);
                l.insert(0, Span::raw("  "));
                out.push((Line::from(l), None));
            }
            Item::TurnEnd(_) => {}
            Item::Notice(text) => {
                blank(&mut out);
                for l in wrap(text, inner) {
                    out.push((Line::from(format!("  {l}").fg(Color::DarkGray)), None));
                }
            }
            Item::Error(text) => {
                blank(&mut out);
                for l in wrap(text, inner) {
                    out.push((Line::from(format!("  {l}").fg(SOFT_RED)), None));
                }
            }
        }
    }
    flush(&mut group, &mut out);
    out
}

/// The actions and folded groups ↑/↓ move through, in order.
pub fn selectable(rows: &[Row]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (_, k) in rows {
        if let Some(k) = k
            && out.last() != Some(k)
        {
            out.push(k.clone());
        }
    }
    out
}

/// What the agent is doing now, for the status line: the running action, or thinking.
pub fn doing(app: &App) -> Option<String> {
    let verb = |name: &str| match name {
        "read" => "Reading",
        "grep" | "glob" => "Searching",
        "edit" | "write" | "apply_patch" => "Editing",
        "bash" => "Running",
        "webfetch" => "Fetching",
        "websearch" => "Searching the web for",
        "task" => "Delegating",
        "skill" => "Loading skill",
        "lsp" => "Looking up",
        "question" => "Asking",
        "todo" => "Planning",
        _ => "Calling",
    };
    let running: Vec<&ToolItem> = app
        .items
        .iter()
        .filter_map(|i| if let Item::Tool(t) = i { Some(t) } else { None })
        .filter(|t| t.state == ToolState::Running)
        .collect();
    if let Some(t) = running.last() {
        let what = if t.name == "bash" { format!("$ {}", t.title) } else { t.title.clone() };
        let mut s = format!("{} {}", verb(&t.name), cut(what.lines().next().unwrap_or_default(), 60));
        if let Some(p) = t.progress.last() {
            s.push_str(&format!(" — {}", cut(p, 40)));
        }
        if running.len() > 1 {
            s.push_str(&format!(" (+{} more)", running.len() - 1));
        }
        return Some(s);
    }
    match app.items.last() {
        Some(Item::Assistant { text, done: false, .. }) if !text.is_empty() => Some("Writing".into()),
        Some(Item::Assistant { done: false, .. }) => Some("Thinking".into()),
        Some(Item::Tool(t)) if t.state == ToolState::Pending => Some(format!("Preparing {}", t.name)),
        _ => Some("Working".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(id: &str, name: &str, output: &str) -> ToolItem {
        let mut t = ToolItem::new(id.into(), name.into(), "x".into(), ToolState::Done);
        t.output = output.into();
        t
    }

    #[test]
    fn rows_show_results_and_trim_failures_to_their_errors() {
        let ok = tool("1", "bash", "a\nb\nc");
        assert_eq!(result(&ok)[0].content, "3 lines");
        let bad = tool("2", "bash", "Exit code 101\nCompiling\ntest x ... FAILED\nthread panicked\nok line");
        assert!(failed(&bad));
        assert_eq!(result(&bad)[0].content, "exit 101");
        let trimmed: Vec<String> = body(&bad, false, 60).iter().map(|l| l.to_string()).collect();
        assert!(trimmed[0].contains("FAILED") && trimmed[1].contains("panicked"));
        assert!(trimmed.last().unwrap().contains("enter shows all"));
        let full = body(&bad, true, 60);
        assert_eq!(full.len(), 4);
    }
}
