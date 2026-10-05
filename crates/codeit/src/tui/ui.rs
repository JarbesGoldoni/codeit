//! Drawing: the conversation on the left with the status line, input box and footer under it;
//! the side bubble on the right (title, context, branch, folder; the logo in its bottom edge);
//! popups in the middle.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Position, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Padding, Paragraph, Wrap},
};

use super::app::{App, Catalog, Dialog, ModelRole, PickerKind, duration, tokens};
use super::style::{AGENT, BLUE, SELECT, cut, meter, right, wrap};
use codeit_harness::session::Approval;

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Columns of the side bubble; it shows when the terminal is at least `SIDE_MIN` wide.
const SIDE: u16 = 30;
const SIDE_MIN: u16 = 100;

/// The small logo, set in the side bubble's bottom edge: ">_" and "it" in blue, "code" in white.
fn badge() -> Line<'static> {
    Line::from(vec![" >_ ".fg(BLUE).bold(), "code".white().bold(), "it ".fg(BLUE).bold()])
}

pub fn draw(app: &App, f: &mut Frame) {
    if app.review.is_some() {
        return super::review::draw(app, f);
    }
    let area = f.area();
    let (left, side) = if area.width >= SIDE_MIN {
        let [l, _, s] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(1), Constraint::Length(SIDE)]).areas(area);
        (l, Some(s))
    } else {
        (area, None)
    };
    if let Some(side) = side {
        draw_side(app, f, side);
    }

    let commands = app.command_matches();
    let inner_width = left.width.saturating_sub(4).max(1) as usize;
    let (composer_rows, cursor) = composer_rows(&app.input, app.cursor, inner_width);
    let dialog_lines = app.dialog.as_ref().map(|d| dialog_lines(d, inner_width));
    let bottom_height = match &dialog_lines {
        Some(lines) => (lines.len() as u16 + 2).min(left.height / 2 + 4),
        None => (composer_rows.len() as u16).clamp(1, 8) + 2,
    };
    let queued = app.queued.len().min(3) as u16;
    // Twelve rows at most, scrolled to keep the highlighted command in view.
    let at = app.command_selected();
    let first = (at + 1).saturating_sub(12);
    let mut suggestion_lines: Vec<Line> = commands
        .iter()
        .enumerate()
        .skip(first)
        .take(12)
        .map(|(i, c)| {
            let line = Line::from(vec![
                format!("  {:<14}", c.name).fg(BLUE),
                format!("{:<26}", c.args).dim(),
                c.help.clone().dim(),
            ]);
            if i == at && commands.len() > 1 { line.bg(SELECT) } else { line }
        })
        .collect();
    // The only match's longer help (extension commands).
    if let [only] = commands.as_slice()
        && !only.long.is_empty()
    {
        for l in wrap_words(&only.long, inner_width.saturating_sub(4)) {
            suggestion_lines.push(Line::from(format!("    {l}").italic().dim()));
        }
    }

    let [history, status, queue, suggestions, bottom, footer] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(queued),
        Constraint::Length(suggestion_lines.len() as u16),
        Constraint::Length(bottom_height),
        Constraint::Length(1),
    ])
    .areas(left);

    draw_history(app, f, history);

    // What the agent is doing now; the agent and model show with the answer.
    if let Some(turn) = &app.turn {
        let spin = SPINNER[app.tick % SPINNER.len()];
        let what = super::chat::doing(app).unwrap_or_else(|| "Working".into());
        f.render_widget(
            Line::from(vec![
                format!(" {spin} ").fg(BLUE),
                cut(&what, status.width.saturating_sub(30) as usize).bold(),
                format!(" ({} • esc to interrupt)", duration(turn.started.elapsed().as_secs())).dim(),
            ]),
            status,
        );
    }

    let lines: Vec<Line> = app
        .queued
        .iter()
        .take(3)
        .map(|q| Line::from(vec!["  ↳ queued: ".dim(), short(q, 80).dim().italic()]))
        .collect();
    f.render_widget(Paragraph::new(lines), queue);
    f.render_widget(Paragraph::new(suggestion_lines), suggestions);

    match dialog_lines {
        Some(lines) => {
            let title = match &app.dialog {
                Some(Dialog::Question { index, questions, .. }) => {
                    format!(" Question {}/{} ", index + 1, questions.len())
                }
                _ => " Permission ".to_string(),
            };
            f.render_widget(
                Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                    Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().yellow()).title(title),
                ),
                bottom,
            );
        }
        None => draw_composer(app, f, bottom, &composer_rows, cursor),
    }
    draw_footer(app, f, footer);

    if app.picker.is_some() {
        draw_picker(app, f, area);
    }
    if app.panel.is_some() {
        super::panel::draw(app, f);
    }
}

fn draw_composer(app: &App, f: &mut Frame, area: Rect, rows: &[String], cursor: (usize, usize)) {
    let visible = area.height as usize - 2;
    let first = cursor.0.saturating_sub(visible.saturating_sub(1));
    let prompt = || "› ".fg(BLUE).bold();
    let body: Vec<Line> = if app.input.is_empty() {
        let hint = if app.busy() {
            "Type to queue a message"
        } else {
            "Ask codeit anything — @path attaches a file, / for commands"
        };
        vec![Line::from(vec![prompt(), hint.dim()])]
    } else {
        rows.iter()
            .enumerate()
            .skip(first)
            .take(visible)
            .map(|(i, row)| Line::from(vec![if i == 0 { prompt() } else { "  ".into() }, row.clone().into()]))
            .collect()
    };
    let mut block = Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(BLUE));
    if let Some(n) = app.rewind {
        block = block
            .title(format!(
                " editing an earlier message: sending rewinds {n} turn{} · esc cancels ",
                if n == 1 { "" } else { "s" }
            ))
            .border_style(Style::new().yellow());
    }
    f.render_widget(Paragraph::new(body).block(block), area);
    if app.picker.is_none() {
        f.set_cursor_position(Position::new(area.x + 3 + cursor.1 as u16, area.y + 1 + (cursor.0 - first) as u16));
    }
}

fn draw_footer(app: &App, f: &mut Frame, area: Rect) {
    let agent_style = if app.agent == "plan" { Style::new().magenta().bold() } else { Style::new().cyan().bold() };
    let mut left = vec![Span::styled(format!(" {} ", app.agent), agent_style)];
    if app.approval == Approval::Ask {
        left.push(" ask before changes".yellow());
    }
    left.push(format!("  tab agent · ctrl+o actions: {} · /help", super::chat::LEVELS[app.level]).dim());
    let model = match &app.model {
        Some(m) => format!("{}{} ", m.key(), app.effort.as_ref().map(|e| format!(" · {e}")).unwrap_or_default()),
        None => "no model: /model ".into(),
    };
    f.render_widget(right(left, vec![model.dim()], area.width as usize), area);
}

/// The side bubble: the session's title and context on top, branch and folder under them.
fn draw_side(app: &App, f: &mut Frame, area: Rect) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(AGENT))
        .padding(Padding::horizontal(1))
        .title_bottom(badge().right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = inner.width as usize;
    let dim = |t: String| Line::from(t.fg(Color::DarkGray));
    let gray = |t: String| Line::from(t.fg(Color::Gray));
    let bar = w.saturating_sub(5);

    let title = {
        let s = app.session.lock().unwrap();
        if s.title.is_empty() { "New session".to_string() } else { s.title.clone() }
    };
    let mut top: Vec<Line> = wrap(&title, w).into_iter().take(3).map(|l| Line::from(l.white().bold())).collect();
    match app.context {
        Some((used, usable)) if usable > 0 => {
            top.push(Line::from(meter(used * 100 / usable, bar)));
            top.push(dim(format!("{} of {} context", tokens(used), tokens(usable))));
        }
        _ => top.push(dim("context: nothing sent yet".into())),
    }
    top.extend([Line::default(), Line::default()]);
    if let Some(b) = &app.side.branch {
        let (added, changed) = app.side.changes;
        let mut l = vec![cut(b, w.saturating_sub(10)).fg(Color::Gray)];
        if added + changed > 0 {
            l.push(format!("  +{added} ~{changed}").fg(Color::DarkGray));
        }
        top.push(Line::from(l));
    }
    // The last two folders of the path.
    let parts: Vec<&str> = app.cwd.trim_end_matches('/').rsplit('/').take(2).collect();
    top.push(gray(cut(&parts.into_iter().rev().collect::<Vec<_>>().join("/"), w)));

    f.render_widget(Paragraph::new(top), inner);
}

fn draw_history(app: &App, f: &mut Frame, area: Rect) {
    let w = area.width as usize;
    app.width.set(w);
    let rows = super::chat::rows(app, w);
    let view = area.height as usize;
    let max_top = rows.len().saturating_sub(view);
    let mut top = max_top.saturating_sub(app.scroll.get() as usize);
    // Keep the selected action in view.
    if let Some(sel) = &app.selected {
        let at: Vec<usize> =
            rows.iter().enumerate().filter(|(_, r)| r.1.as_ref() == Some(sel)).map(|(i, _)| i).collect();
        if let (Some(&first), Some(&last)) = (at.first(), at.last()) {
            if first < top || last - first + 1 > view {
                top = first.saturating_sub(1);
            } else if last >= top + view {
                top = (last + 2).saturating_sub(view);
            }
            top = top.min(max_top);
            app.scroll.set((max_top - top) as u16);
        }
    }
    let lines: Vec<Line> = rows.into_iter().skip(top).take(view).map(|r| r.0).collect();
    f.render_widget(Paragraph::new(lines), area);
}

// ── popups ──────────────────────────────────────────────────────────────────

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(4));
    let h = h.min(area.height.saturating_sub(2));
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

/// A popup in the middle, like a command palette: what you type in a blue box on top, the list
/// in a gray box under it.
fn draw_picker(app: &App, f: &mut Frame, area: Rect) {
    let Some(picker) = &app.picker else { return };
    // Dim everything behind it.
    let buf = f.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(c) = buf.cell_mut((x, y)) {
                c.set_fg(Color::Rgb(70, 70, 70));
                c.set_bg(Color::Reset);
                c.modifier = Modifier::empty();
            }
        }
    }
    // Short fixed lists fit their rows; the others leave room to scroll.
    let height = match &picker.kind {
        PickerKind::Effort | PickerKind::LoginMethod(_) => app.picker_rows().len().max(1) as u16 + 5,
        _ => 24,
    };
    let popup = centered(area, 76, height);
    f.render_widget(Clear, popup);
    let [input, list] = Layout::vertical([Constraint::Length(3), Constraint::Fill(1)]).areas(popup);

    let (title, secret, help): (String, bool, Option<String>) = match &picker.kind {
        PickerKind::Model(ModelRole::Code) => (" /model ".into(), false, None),
        PickerKind::Model(role) => (format!(" /roles · {} model: {} ", role.name(), role.purpose()), false, None),
        PickerKind::Roles => (" /roles ".into(), false, None),
        PickerKind::Session(_) => (" /session ".into(), false, None),
        PickerKind::Login(_) => (" /login ".into(), false, None),
        PickerKind::LoginMethod(c) => (format!(" /login · {} ", c.name), false, None),
        PickerKind::Key(c) => (
            format!(" {} API key ", c.name),
            true,
            Some(format!(
                "Paste your {} key and press enter. codeit keeps it in ~/.local/share/codeit/auth.json, readable only by you.",
                c.name
            )),
        ),
        PickerKind::Effort => {
            (format!(" /effort · {} ", app.model.as_ref().map(|m| m.name.as_str()).unwrap_or_default()), false, None)
        }
        PickerKind::Enterprise => (
            " GitHub Enterprise ".into(),
            false,
            Some("Your GitHub Enterprise domain, like company.ghe.com, then enter.".into()),
        ),
    };
    let typed = if secret { "•".repeat(picker.filter.chars().count()) } else { picker.filter.clone() };
    let query = if typed.is_empty() && help.is_none() {
        Line::from(vec!["› ".fg(BLUE).bold(), "type to filter".fg(Color::DarkGray)])
    } else {
        Line::from(vec!["› ".fg(BLUE).bold(), typed.white(), "▏".fg(BLUE)])
    };
    f.render_widget(
        Paragraph::new(query).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(BLUE))
                .title(Line::from(title.fg(BLUE)))
                .padding(Padding::horizontal(1)),
        ),
        input,
    );
    let hint = if help.is_some() { " enter save · esc close " } else { " ↑↓ move · enter choose · esc close " };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(AGENT))
        .title_bottom(Line::from(hint.fg(Color::DarkGray)))
        .padding(Padding::horizontal(1));
    let inner = block.inner(list);
    f.render_widget(block, list);
    let w = inner.width as usize;

    if let Some(help) = help {
        let lines: Vec<Line> = wrap(&help, w).into_iter().map(|l| Line::from(l.fg(Color::Gray))).collect();
        f.render_widget(Paragraph::new(lines), inner);
        return;
    }
    let rows = app.picker_rows();
    let mut status: Vec<Line> = Vec::new();
    if matches!(picker.kind, PickerKind::Model(_)) {
        let loading = app.catalogs.values().filter(|c| matches!(c, Catalog::Loading)).count();
        if loading > 0 {
            status.push(Line::from(format!("loading {loading} providers…").fg(Color::DarkGray)));
        }
        for (id, c) in &app.catalogs {
            if let Catalog::Failed(e) = c {
                status.push(Line::from(
                    cut(&format!("{id}: {}", e.lines().next().unwrap_or_default()), w).fg(super::style::SOFT_RED),
                ));
            }
        }
        if rows.is_empty() && loading == 0 {
            status.push(Line::from("No models yet: /login adds a provider.".fg(Color::DarkGray)));
        }
    }
    if matches!(picker.kind, PickerKind::Login(None)) {
        status.push(Line::from("loading providers…".fg(Color::DarkGray)));
    }
    let height = (inner.height as usize).saturating_sub(status.len());
    let first =
        picker.selected.saturating_sub(height.saturating_sub(2).max(1) - 1).min(rows.len().saturating_sub(height));
    let mut lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(first)
        .take(height)
        .map(|(i, (label, detail, marked))| {
            let picked = i == picker.selected;
            let mark = if *marked { "● ".fg(BLUE) } else { "  ".into() };
            let name = if picked { label.clone().white().bold() } else { label.clone().fg(Color::Gray) };
            let detail = cut(detail, w / 2);
            let line = right(
                vec![mark, Span::styled(cut(label, w.saturating_sub(detail.chars().count() + 4)), name.style)],
                vec![detail.fg(Color::DarkGray)],
                w,
            );
            if picked { Line::from(line.spans.into_iter().map(|s| s.bg(SELECT)).collect::<Vec<_>>()) } else { line }
        })
        .collect();
    if rows.is_empty() && status.is_empty() {
        lines.push(Line::from("  nothing matches".fg(Color::DarkGray)));
    }
    lines.extend(status);
    f.render_widget(Paragraph::new(lines), inner);
}

/// Splits the input into rows of at most `width` chars; returns the rows and the cursor's (row, col).
fn composer_rows(input: &str, cursor: usize, width: usize) -> (Vec<String>, (usize, usize)) {
    let mut rows = vec![String::new()];
    let mut pos = (0, 0);
    let mut col = 0;
    for (i, c) in input.char_indices() {
        if i == cursor {
            pos = (rows.len() - 1, col);
        }
        if c == '\n' {
            rows.push(String::new());
            col = 0;
            continue;
        }
        if col == width {
            rows.push(String::new());
            col = 0;
        }
        rows.last_mut().unwrap().push(c);
        col += 1;
    }
    if cursor >= input.len() {
        if col == width {
            rows.push(String::new());
            col = 0;
        }
        pos = (rows.len() - 1, col);
    }
    (rows, pos)
}

/// Wraps text to `width` columns by words.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width.max(20) {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

/// The end of a path, when it is too long: `…/tui/review.rs`.
pub(super) fn short_tail(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max { s.to_string() } else { format!("…{}", s.chars().skip(n + 1 - max).collect::<String>()) }
}

fn short(s: &str, max: usize) -> String {
    let first = s.lines().next().unwrap_or_default();
    if first.chars().count() > max {
        format!("{}…", first.chars().take(max.saturating_sub(1)).collect::<String>())
    } else if s.contains('\n') {
        format!("{first} …")
    } else {
        first.to_string()
    }
}

/// A unified diff as numbered, colored lines (at most `max`).
fn diff_lines(diff: &str, max: usize) -> (Vec<Line<'static>>, usize, usize) {
    let mut out = Vec::new();
    let (mut added, mut removed) = (0, 0);
    let (mut old_no, mut new_no) = (0usize, 0usize);
    let mut hidden = 0;
    for l in diff.lines() {
        if l.starts_with("+++") || l.starts_with("---") {
            continue;
        }
        if let Some(h) = l.strip_prefix("@@ ") {
            // @@ -a,b +c,d @@
            let nums: Vec<usize> = h
                .split_whitespace()
                .take(2)
                .filter_map(|p| p[1..].split(',').next().and_then(|n| n.parse().ok()))
                .collect();
            if let [o, n] = nums[..] {
                old_no = o;
                new_no = n;
            }
            if !out.is_empty() && out.len() < max {
                out.push(Line::from("    ⋮".dim()));
            }
            continue;
        }
        let (line, style) = match l.chars().next() {
            Some('+') => {
                added += 1;
                new_no += 1;
                (format!("{:>5} +{}", new_no - 1, &l[1..]), Style::new().fg(Color::Green))
            }
            Some('-') => {
                removed += 1;
                old_no += 1;
                (format!("{:>5} -{}", old_no - 1, &l[1..]), Style::new().fg(Color::Red))
            }
            _ => {
                old_no += 1;
                new_no += 1;
                (format!("{:>5}  {}", new_no - 1, l.get(1..).unwrap_or("")), Style::new().dim())
            }
        };
        if out.len() < max {
            out.push(Line::from(Span::styled(line, style)));
        } else {
            hidden += 1;
        }
    }
    if hidden > 0 {
        out.push(Line::from(format!("    … +{hidden} lines").dim()));
    }
    (out, added, removed)
}

// ── dialogs ─────────────────────────────────────────────────────────────────

pub(super) fn dialog_lines(d: &Dialog, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match d {
        Dialog::Permission { ask, selected, feedback } => {
            lines.push(Line::from(vec!["Allow codeit to ".bold(), ask.title.clone().bold(), "?".bold()]));
            if let Some(detail) = &ask.detail {
                if detail.contains("\n@@") || detail.starts_with("---") {
                    lines.extend(diff_lines(detail, 12).0);
                } else if *detail != ask.title {
                    for l in detail.lines().take(6) {
                        lines.push(Line::from(format!("  {}", short(l, width.saturating_sub(4))).cyan()));
                    }
                }
            }
            lines.push(Line::from(""));
            let scope = if ask.always.iter().any(|a| a == "*") {
                match ask.permission.as_str() {
                    "edit" => "any edit".to_string(),
                    "webfetch" => "any fetch".to_string(),
                    "websearch" => "any web search".to_string(),
                    p => format!("any {p}"),
                }
            } else {
                format!("{} {}", ask.permission, short(&ask.always.join(", "), 50))
            };
            let always = format!("Yes, and don't ask again this session for {scope}");
            let options = ["Yes".to_string(), always, "No, and tell codeit what to do instead".to_string()];
            for (i, o) in options.iter().enumerate() {
                let line = format!("{} {}. {o}", if i == *selected { "›" } else { " " }, i + 1);
                lines.push(if i == *selected { Line::from(line.cyan().bold()) } else { Line::from(line) });
            }
            match feedback {
                Some(text) => {
                    lines.push(Line::from(""));
                    lines.push(Line::from(vec![
                        "What should codeit do instead? ".bold(),
                        text.clone().into(),
                        "▏".dim(),
                    ]));
                    lines.push(Line::from("enter sends (empty: just no) · esc just no".dim()));
                }
                None => lines.push(Line::from("y yes · a always · n no · esc no".dim())),
            }
        }
        Dialog::Question { questions, index, selected, chosen, typing, .. } => {
            let q = &questions[*index];
            let head = if q.header.is_empty() { String::new() } else { format!("{}: ", q.header) };
            lines.push(Line::from(vec![head.cyan().bold(), q.question.clone().bold()]));
            lines.push(Line::from(""));
            for (i, (label, desc)) in q.options.iter().enumerate() {
                let check = if q.multiple { if chosen.contains(&i) { "[x] " } else { "[ ] " } } else { "" };
                let text = format!("{} {}. {check}{label}", if i == *selected { "›" } else { " " }, i + 1);
                let mut spans = vec![if i == *selected { text.cyan().bold() } else { text.into() }];
                if !desc.is_empty() {
                    spans.push(format!("  {desc}").dim());
                }
                lines.push(Line::from(spans));
            }
            let custom = q.options.len();
            let text = format!("{} {}. Type your own answer", if *selected == custom { "›" } else { " " }, custom + 1);
            lines.push(if *selected == custom { Line::from(text.cyan().bold()) } else { Line::from(text) });
            match typing {
                Some(t) => {
                    lines.push(Line::from(vec!["  answer: ".bold(), t.clone().into(), "▏".dim()]));
                    lines.push(Line::from("enter sends · esc back".dim()));
                }
                None => lines.push(Line::from(
                    if q.multiple {
                        "↑↓ move · space select · enter confirm · esc dismiss"
                    } else {
                        "↑↓ move · enter choose · esc dismiss"
                    }
                    .dim(),
                )),
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_and_places_cursor() {
        let (rows, pos) = composer_rows("abcdef\ngh", 9, 4);
        assert_eq!(rows, ["abcd", "ef", "gh"]);
        assert_eq!(pos, (2, 2));
        let (_, pos) = composer_rows("abcd", 4, 4);
        assert_eq!(pos, (1, 0));
        let (_, pos) = composer_rows("abcdef", 2, 4);
        assert_eq!(pos, (0, 2));
    }

    #[test]
    fn numbers_diff_lines() {
        let diff = "--- a\n+++ a\n@@ -1,3 +1,3 @@\n x\n-old\n+new\n y\n";
        let (lines, added, removed) = diff_lines(diff, 10);
        assert_eq!((added, removed), (1, 1));
        let text: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        assert_eq!(text, ["    1  x", "    2 -old", "    2 +new", "    3  y"]);
    }
}
