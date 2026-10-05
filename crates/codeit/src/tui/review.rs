//! The review screen: the diff in the middle, the changed files on the right, comments under
//! the lines they are about, like a pull request. `r` has the review agent go through it.

use std::cell::Cell;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use codeit_harness::event::Event as HEvent;
use codeit_harness::review::{Comment, Review, Target, Verdict};
use codeit_harness::session::Session;
use codeit_harness::{Input, Run};
use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Paragraph, Wrap},
};
use tokio_util::sync::CancellationToken;

use super::app::{App, AppEvent, duration};

pub struct ReviewRun {
    pub id: u64,
    pub cancel: CancellationToken,
    pub started: Instant,
    /// The step the agent is on.
    pub now: String,
    /// Its reply so far: the overall assessment.
    pub text: String,
    pub model: String,
}

pub struct ReviewView {
    pub review: Arc<Mutex<Review>>,
    /// Selected file.
    pub file: usize,
    /// Selected row of the diff pane.
    pub cursor: usize,
    /// ↑/↓ move through the file list instead of the diff.
    pub files_focus: bool,
    /// Writing a comment on the selected line.
    pub typing: Option<String>,
    pub run: Option<ReviewRun>,
    /// The last thing worth telling: errors, where an export went.
    pub status: Option<String>,
    scroll: Cell<usize>,
}

/// A row of the diff pane.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Row {
    Hunk(usize),
    Line(usize, usize),
    /// Index into the review's comments.
    Comment(usize),
}

fn rows(r: &Review, file: usize) -> Vec<Row> {
    let Some(f) = r.files.get(file) else { return Vec::new() };
    let side = |l: &codeit_harness::review::DiffLine| if f.status == 'D' { l.old } else { l.new };
    let mut out = Vec::new();
    // Comments whose line is no longer in the diff go first, marked outdated.
    for (i, c) in r.comments.iter().enumerate() {
        if c.path == f.path && !f.commentable(c.line) {
            out.push(Row::Comment(i));
        }
    }
    for (h, hunk) in f.hunks.iter().enumerate() {
        out.push(Row::Hunk(h));
        for (i, l) in hunk.lines.iter().enumerate() {
            out.push(Row::Line(h, i));
            if let Some(n) = side(l) {
                for (ci, c) in r.comments.iter().enumerate() {
                    if c.path == f.path && c.line == n {
                        out.push(Row::Comment(ci));
                    }
                }
            }
        }
    }
    out
}

/// The line number of the diff row `cursor` of `file` (for a comment row, its line).
fn line_at(r: &Review, file: usize, cursor: usize) -> Option<u32> {
    let f = r.files.get(file)?;
    match rows(r, file).get(cursor)? {
        Row::Line(h, i) => {
            let l = &f.hunks[*h].lines[*i];
            if f.status == 'D' { l.old } else { l.new }
        }
        Row::Comment(i) => Some(r.comments[*i].line),
        Row::Hunk(_) => None,
    }
}

impl ReviewView {
    pub fn new(review: Review) -> Self {
        Self {
            review: Arc::new(Mutex::new(review)),
            file: 0,
            cursor: 0,
            files_focus: false,
            typing: None,
            run: None,
            status: None,
            scroll: Cell::new(0),
        }
    }

    fn rows(&self) -> Vec<Row> {
        rows(&self.review.lock().unwrap(), self.file)
    }

    /// The comment under the cursor.
    fn comment(&self) -> Option<usize> {
        match self.rows().get(self.cursor) {
            Some(Row::Comment(i)) => Some(*i),
            _ => None,
        }
    }

    /// The line number a new comment at the cursor would go on.
    fn line(&self) -> Option<u32> {
        line_at(&self.review.lock().unwrap(), self.file, self.cursor)
    }

    fn set_verdict(&mut self, v: Verdict) {
        if let Some(i) = self.comment() {
            let mut r = self.review.lock().unwrap();
            r.comments[i].verdict = v;
            let _ = r.save();
        }
    }

    fn select_file(&mut self, file: usize) {
        let n = self.review.lock().unwrap().files.len();
        if n > 0 {
            self.file = file.min(n - 1);
            self.cursor = 0;
            self.scroll.set(0);
        }
    }

    /// Moves to the next (or previous) comment, across files.
    fn jump(&mut self, forward: bool) {
        let n = self.review.lock().unwrap().files.len();
        for step in 0..=n {
            let file =
                if forward { (self.file + step) % n.max(1) } else { (self.file + n - step % n.max(1)) % n.max(1) };
            let rows = rows(&self.review.lock().unwrap(), file);
            let hit = if step == 0 {
                if forward {
                    rows.iter().enumerate().skip(self.cursor + 1).find(|(_, r)| matches!(r, Row::Comment(_)))
                } else {
                    rows.iter().enumerate().take(self.cursor).rev().find(|(_, r)| matches!(r, Row::Comment(_)))
                }
            } else if forward {
                rows.iter().enumerate().find(|(_, r)| matches!(r, Row::Comment(_)))
            } else {
                rows.iter().enumerate().rev().find(|(_, r)| matches!(r, Row::Comment(_)))
            };
            if let Some((i, _)) = hit {
                self.file = file;
                self.cursor = i;
                return;
            }
        }
        self.status = Some("No comments yet: press r to have codeit review it, or c to write one.".into());
    }
}

impl App {
    /// `/review [target]`: opens the review screen on that diff.
    pub fn open_review(&mut self, args: &str) {
        let target = Target::parse(args, &self.harness.cwd);
        match Review::open(&self.harness.cwd, target) {
            Ok(r) if r.files.is_empty() => self.notice(format!("Nothing to review: no {}.", r.target.label())),
            Ok(r) => {
                let view = ReviewView::new(r);
                self.harness.set_review(Some(view.review.clone()));
                self.review = Some(view);
            }
            Err(e) => self.error(format!("Couldn't load the diff: {e:#}")),
        }
    }

    fn close_review(&mut self) {
        if let Some(v) = self.review.take() {
            if let Some(run) = &v.run {
                run.cancel.cancel();
            }
            let _ = v.review.lock().unwrap().save();
        }
        self.harness.set_review(None);
    }

    pub fn review_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some(v) = &mut self.review else { return };
        if let Some(text) = &mut v.typing {
            match k.code {
                KeyCode::Esc => v.typing = None,
                KeyCode::Enter => {
                    let body = text.trim().to_string();
                    v.typing = None;
                    if body.is_empty() {
                        return;
                    }
                    let Some(line) = v.line() else {
                        v.status = Some("Put the cursor on a line of the diff to comment on it.".into());
                        return;
                    };
                    let mut r = v.review.lock().unwrap();
                    let path = r.files[v.file].path.clone();
                    if let Err(e) = r.add(&path, line, "note", &body, "you") {
                        v.status = Some(format!("{e:#}"));
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char(c) if !ctrl => text.push(c),
                _ => {}
            }
            return;
        }
        v.status = None;
        let count = v.rows().len();
        let files = v.review.lock().unwrap().files.len();
        match k.code {
            KeyCode::Char('c') if ctrl => self.close_review(),
            KeyCode::Esc if v.run.is_some() => {
                if let Some(run) = &v.run {
                    run.cancel.cancel();
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => self.close_review(),
            KeyCode::Tab | KeyCode::BackTab => v.files_focus = !v.files_focus,
            KeyCode::Up | KeyCode::Char('k') if v.files_focus => v.select_file(v.file.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') if v.files_focus => v.select_file(v.file + 1),
            KeyCode::Enter if v.files_focus => v.files_focus = false,
            KeyCode::Up | KeyCode::Char('k') => v.cursor = v.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => v.cursor = (v.cursor + 1).min(count.saturating_sub(1)),
            KeyCode::PageUp => v.cursor = v.cursor.saturating_sub(15),
            KeyCode::PageDown => v.cursor = (v.cursor + 15).min(count.saturating_sub(1)),
            KeyCode::Home => v.cursor = 0,
            KeyCode::End => v.cursor = count.saturating_sub(1),
            KeyCode::Char('n') | KeyCode::Char(']') => v.select_file((v.file + 1) % files.max(1)),
            KeyCode::Char('p') | KeyCode::Char('[') => v.select_file((v.file + files.max(1) - 1) % files.max(1)),
            KeyCode::Char('J') => v.jump(true),
            KeyCode::Char('K') => v.jump(false),
            KeyCode::Char('a') => v.set_verdict(Verdict::Agreed),
            KeyCode::Char('d') => v.set_verdict(Verdict::Dismissed),
            KeyCode::Char('o') => v.set_verdict(Verdict::Open),
            KeyCode::Char('D') => {
                if let Some(i) = v.comment() {
                    let mut r = v.review.lock().unwrap();
                    r.comments.remove(i);
                    let _ = r.save();
                }
            }
            KeyCode::Char('c') => {
                if v.line().is_some() {
                    v.typing = Some(String::new());
                } else {
                    v.status = Some("Put the cursor on a line of the diff to comment on it.".into());
                }
            }
            KeyCode::Char('u') => {
                let (cwd, target) = {
                    let r = v.review.lock().unwrap();
                    (r.cwd.clone(), r.target.clone())
                };
                match codeit_harness::review::load_diff(&cwd, &target) {
                    Ok((files, _)) => {
                        v.review.lock().unwrap().files = files;
                        v.select_file(v.file);
                        v.status = Some("Reloaded the diff.".into());
                    }
                    Err(e) => v.status = Some(format!("{e:#}")),
                }
            }
            KeyCode::Char('e') => {
                let r = v.review.lock().unwrap();
                let path = codeit_providers::data_dir().join("reviews").join(format!("{}.md", r.id));
                v.status = Some(match std::fs::write(&path, r.markdown()) {
                    Ok(()) => format!("Exported to {}", path.display()),
                    Err(e) => format!("Couldn't export: {e}"),
                });
            }
            KeyCode::Char('r') if v.run.is_none() => self.start_review_run(),
            KeyCode::Char('f') => self.fix_agreed(),
            _ => {}
        }
    }

    /// Has the review agent go through the diff, on the think model.
    fn start_review_run(&mut self) {
        let Some(model) = self.model_for("plan") else {
            if let Some(v) = &mut self.review {
                v.status = Some("Pick a model first (/models).".into());
            }
            return;
        };
        let Some(v) = &mut self.review else { return };
        let request = v.review.lock().unwrap().request();
        let effort = self.effort.clone().filter(|e| model.efforts.contains(e));
        let mut s = Session::new(&self.harness.cwd, "review", Some(model.key()), effort);
        // Linked to the review, so it doesn't show up in /session.
        s.parent = Some(format!("review:{}", v.review.lock().unwrap().id));
        self.next_turn += 1;
        let id = self.next_turn;
        let cancel = CancellationToken::new();
        let (htx, mut hrx) = tokio::sync::mpsc::unbounded_channel();
        let run = Run {
            harness: self.harness.clone(),
            session: Arc::new(Mutex::new(s)),
            events: htx,
            cancel: cancel.clone(),
            depth: 0,
        };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let forward = tokio::spawn(async move {
                while let Some(ev) = hrx.recv().await {
                    let _ = tx.send(AppEvent::Review(id, ev));
                }
            });
            run.start(Input::Prompt(request)).await;
            drop(run);
            let _ = forward.await;
        });
        v.status = None;
        v.run = Some(ReviewRun {
            id,
            cancel,
            started: Instant::now(),
            now: "Reading the diff".into(),
            text: String::new(),
            model: model.name.clone(),
        });
    }

    pub fn review_event(&mut self, id: u64, ev: HEvent) {
        let Some(v) = &mut self.review else { return };
        let Some(run) = v.run.as_mut().filter(|r| r.id == id) else { return };
        match ev {
            HEvent::Text(t) => run.text.push_str(&t),
            HEvent::Reset => run.text.clear(),
            HEvent::ToolStart { name, title, .. } => {
                run.now = match name.as_str() {
                    "review_comment" => format!("Commenting on {title}"),
                    "read" => format!("Reading {title}"),
                    "bash" => format!("Running {title}"),
                    "grep" | "glob" => format!("Searching {title}"),
                    _ => format!("{name} {title}"),
                };
                // Once the model calls tools, earlier text was thinking out loud.
                run.text.clear();
            }
            HEvent::Ask(a) => self.show_ask(a),
            HEvent::Error(e) => v.status = Some(e),
            HEvent::Done => {
                let run = v.run.take().expect("checked above");
                let mut r = v.review.lock().unwrap();
                if !run.text.trim().is_empty() {
                    r.summary = Some(run.text.trim().to_string());
                }
                let _ = r.save();
                let n = r.comments.iter().filter(|c| c.author != "you").count();
                drop(r);
                if v.status.is_none() {
                    v.status = Some(format!(
                        "Review finished in {}: {n} comments from codeit. J/K jump between comments.",
                        duration(run.started.elapsed().as_secs())
                    ));
                }
                self.dialog = None;
            }
            _ => {}
        }
    }

    /// `f`: closes the review and asks the build agent to address the agreed comments.
    fn fix_agreed(&mut self) {
        let Some(v) = &mut self.review else { return };
        let (label, agreed): (String, Vec<Comment>) = {
            let r = v.review.lock().unwrap();
            (r.target.label(), r.comments.iter().filter(|c| c.verdict == Verdict::Agreed).cloned().collect())
        };
        if agreed.is_empty() {
            v.status = Some("Agree with some comments first (a), then f sends them to the build agent.".into());
            return;
        }
        let mut prompt = format!("Address these review comments on the {label}:\n");
        for c in &agreed {
            prompt.push_str(&format!("- {}:{} ({}): {}\n", c.path, c.line, c.severity, c.body));
        }
        prompt.push_str("\nFix each one, then summarize what you changed.");
        self.close_review();
        self.agent = "build".into();
        self.submit_text(prompt);
    }
}

// ── drawing ─────────────────────────────────────────────────────────────────

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn severity_style(s: &str) -> Style {
    match s {
        "bug" => Style::new().red().bold(),
        "risk" => Style::new().yellow().bold(),
        "question" => Style::new().cyan().bold(),
        "note" => Style::new().blue().bold(),
        _ => Style::new().dim(),
    }
}

/// Wraps `text` to `width` columns (by words).
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.lines() {
        let mut line = String::new();
        for word in para.split_whitespace() {
            if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
                out.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        out.push(line);
    }
    out
}

fn comment_lines(c: &Comment, outdated: bool, width: usize) -> Vec<Line<'static>> {
    let bar = Span::styled("          ┃ ", severity_style(&c.severity));
    let (verdict, vstyle) = match c.verdict {
        Verdict::Open => ("open", Style::new().yellow()),
        Verdict::Agreed => ("✓ agreed", Style::new().green().bold()),
        Verdict::Dismissed => ("dismissed", Style::new().dim()),
    };
    let mut head = vec![bar.clone(), Span::styled(c.severity.clone(), severity_style(&c.severity))];
    head.push(format!(" · {} · line {}", c.author, c.line).dim());
    if outdated {
        head.push(" · outdated".dim());
    }
    head.push("  ".into());
    head.push(Span::styled(verdict, vstyle));
    let mut lines = vec![Line::from(head)];
    let body_style = if c.verdict == Verdict::Dismissed {
        Style::new().dim().add_modifier(Modifier::CROSSED_OUT)
    } else {
        Style::new()
    };
    for l in wrap(&c.body, width.saturating_sub(14).max(20)) {
        lines.push(Line::from(vec![bar.clone(), Span::styled(l, body_style)]));
    }
    lines
}

pub fn draw(app: &App, f: &mut Frame) {
    let Some(v) = &app.review else { return };
    let r = v.review.lock().unwrap();
    let area = f.area();
    let summary: Vec<String> =
        r.summary.as_deref().map(|s| wrap(s, area.width.saturating_sub(10) as usize)).unwrap_or_default();
    let dialog = app.dialog.as_ref().map(|d| super::ui::dialog_lines(d, area.width.saturating_sub(4) as usize));
    let [title, summary_area, body, bottom] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(summary.len().min(4) as u16),
        Constraint::Fill(1),
        Constraint::Length(dialog.as_ref().map(|d| d.len() as u16 + 2).unwrap_or(1)),
    ])
    .areas(area);

    // Title: target, counts, and the agent's progress.
    let open = r.comments.iter().filter(|c| c.verdict == Verdict::Open).count();
    let agreed = r.comments.iter().filter(|c| c.verdict == Verdict::Agreed).count();
    let mut spans = vec![
        " Review ".reversed().bold(),
        format!(" {}", r.target.label()).bold(),
        if r.title.is_empty() { "".into() } else { format!(" · {}", r.title).into() },
        format!("  {} files · {} comments ({open} open, {agreed} agreed)", r.files.len(), r.comments.len()).dim(),
    ];
    if let Some(run) = &v.run {
        spans.push(format!("   {} ", SPINNER[app.tick % SPINNER.len()]).cyan());
        spans.push(format!("{} ({}, {})", run.now, run.model, duration(run.started.elapsed().as_secs())).into());
        spans.push(" · esc stops".dim());
    }
    f.render_widget(Line::from(spans), title);
    let lines: Vec<Line> = summary
        .iter()
        .take(4)
        .enumerate()
        .map(|(i, l)| {
            Line::from(vec![if i == 0 { " codeit: ".magenta().bold() } else { "       ".into() }, l.clone().italic()])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), summary_area);

    let [diff_area, files_area] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(38)]).areas(body);

    // Files.
    let lines: Vec<Line> = r
        .files
        .iter()
        .enumerate()
        .map(|(i, file)| {
            let n = r.comments.iter().filter(|c| c.path == file.path && c.verdict != Verdict::Dismissed).count();
            let name = super::ui::short_tail(&file.path, 22);
            let mut spans = vec![
                format!("{} ", file.status).dim(),
                format!("{name:<22}").into(),
                format!(" +{}", file.added).green(),
                format!(" -{}", file.removed).red(),
            ];
            if n > 0 {
                spans.push(format!(" ●{n}").yellow());
            }
            let line = Line::from(spans);
            if i == v.file { line.style(Style::new().add_modifier(Modifier::REVERSED)) } else { line }
        })
        .collect();
    let border = if v.files_focus { Style::new().cyan() } else { Style::new().dim() };
    f.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().border_type(BorderType::Rounded).border_style(border).title(" Files ")),
        files_area,
    );

    // Diff.
    let Some(file) = r.files.get(v.file) else { return };
    let width = diff_area.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = Vec::new();
    let mut cursor_line = (0, 1);
    for (i, row) in rows(&r, v.file).iter().enumerate() {
        let start = lines.len();
        match row {
            Row::Hunk(h) => lines.push(Line::from(file.hunks[*h].header.clone().cyan().dim())),
            Row::Line(h, li) => {
                let l = &file.hunks[*h].lines[*li];
                let num = |n: Option<u32>| n.map(|n| format!("{n:>4}")).unwrap_or_else(|| "    ".into());
                let text = l.text.replace('\t', "    ");
                let style = match l.kind {
                    '+' => Style::new().fg(Color::Green),
                    '-' => Style::new().fg(Color::Red),
                    _ => Style::new(),
                };
                lines.push(Line::from(vec![
                    format!("{} {} ", num(l.old), num(l.new)).dim(),
                    Span::styled(format!("{}{text}", l.kind), style),
                ]));
            }
            Row::Comment(ci) => {
                let c = &r.comments[*ci];
                lines.extend(comment_lines(c, !file.commentable(c.line), width));
            }
        }
        if i == v.cursor {
            cursor_line = (start, lines.len() - start);
            for l in &mut lines[start..] {
                l.spans.insert(0, "▌".cyan());
            }
        } else {
            for l in &mut lines[start..] {
                l.spans.insert(0, " ".into());
            }
        }
    }
    if file.binary {
        lines.push(Line::from("  (binary file)".dim()));
    }
    let view = diff_area.height.saturating_sub(2) as usize;
    let mut top = v.scroll.get();
    let (first, height) = cursor_line;
    if first < top {
        top = first;
    } else if first + height > top + view {
        top = (first + height).saturating_sub(view);
    }
    v.scroll.set(top);
    let title = format!(" {}  +{} -{} ", file.path, file.added, file.removed);
    let border = if v.files_focus { Style::new().dim() } else { Style::new() };
    f.render_widget(
        Paragraph::new(lines)
            .scroll((top as u16, 0))
            .block(Block::bordered().border_type(BorderType::Rounded).border_style(border).title(title)),
        diff_area,
    );

    // Bottom: a permission question, the comment being written, or the keys.
    if let Some(lines) = dialog {
        f.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().yellow())
                    .title(" Permission "),
            ),
            bottom,
        );
    } else if let Some(text) = &v.typing {
        let line = line_at(&r, v.file, v.cursor).map(|n| format!("line {n}")).unwrap_or_default();
        f.render_widget(
            Line::from(vec![
                format!(" comment on {line}: ").bold(),
                text.clone().into(),
                "▏".dim(),
                "   enter saves · esc cancels".dim(),
            ]),
            bottom,
        );
    } else if let Some(s) = &v.status {
        f.render_widget(Line::from(format!(" {s}").yellow()), bottom);
    } else {
        f.render_widget(
            Line::from(
                " r review with codeit · ↑↓ move · n/p file · J/K comments · a agree · d dismiss · D delete · c comment · f fix agreed · e export · q close"
                    .dim(),
            ),
            bottom,
        );
    }
}
