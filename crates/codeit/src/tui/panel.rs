//! Extension panels: a popup listing what an extension's command shows (sections of rows with a
//! tone), driven by the extension's answers to the keys pressed.

use std::sync::Arc;
use std::time::Instant;

use codeit_harness::extension::{Action, Extension, Panel, Reply, Tone};
use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Padding, Paragraph, Wrap},
};

use super::app::{App, AppEvent};

pub struct Input {
    pub title: String,
    pub hint: String,
    pub secret: bool,
    pub text: String,
}

pub struct PanelView {
    ext: Arc<dyn Extension>,
    command: String,
    pub panel: Option<Panel>,
    /// Index among the rows that have an id.
    selected: usize,
    pub input: Option<Input>,
    /// Waiting for the extension's answer.
    pub busy: bool,
    /// Answers to older actions are dropped.
    seq: u64,
    last_poll: Instant,
}

pub(super) fn tone(t: Tone) -> Style {
    match t {
        Tone::Normal => Style::new(),
        Tone::Ok => Style::new().green(),
        Tone::Warn => Style::new().yellow(),
        Tone::Bad => Style::new().red(),
        Tone::Muted => Style::new().dim(),
        Tone::Accent => Style::new().cyan().bold(),
    }
}

impl PanelView {
    fn ids(&self) -> Vec<String> {
        self.panel.iter().flat_map(|p| &p.sections).flat_map(|s| &s.rows).filter_map(|r| r.id.clone()).collect()
    }

    fn selected_id(&self) -> Option<String> {
        self.ids().get(self.selected).cloned()
    }
}

impl App {
    /// Opens an extension command's panel.
    pub fn open_panel(&mut self, ext: Arc<dyn Extension>, command: &str, args: &str) {
        self.panel = Some(PanelView {
            ext,
            command: command.to_string(),
            panel: None,
            selected: 0,
            input: None,
            busy: false,
            seq: 0,
            last_poll: Instant::now(),
        });
        self.panel_act(Action::Open(args.to_string()));
    }

    fn panel_act(&mut self, action: Action) {
        let Some(v) = &mut self.panel else { return };
        v.busy = true;
        v.seq += 1;
        v.last_poll = Instant::now();
        let (ext, command, seq, tx) = (v.ext.clone(), v.command.clone(), v.seq, self.tx.clone());
        tokio::spawn(async move {
            let reply = ext.act(&command, action).await;
            let _ = tx.send(AppEvent::Panel(seq, reply));
        });
    }

    pub fn panel_reply(&mut self, seq: u64, reply: Reply) {
        let Some(v) = &mut self.panel else { return };
        if v.seq != seq {
            return;
        }
        v.busy = false;
        match reply {
            Reply::Panel(p) => {
                let ids: Vec<String> = p.sections.iter().flat_map(|s| &s.rows).filter_map(|r| r.id.clone()).collect();
                if let Some(want) = &p.select
                    && let Some(i) = ids.iter().position(|id| id == want)
                {
                    v.selected = i;
                }
                v.selected = v.selected.min(ids.len().saturating_sub(1));
                v.panel = Some(p);
            }
            Reply::Input { title, hint, secret } => v.input = Some(Input { title, hint, secret, text: String::new() }),
            Reply::Prompt { text, session } => {
                self.panel = None;
                if let Some(target) = session {
                    self.open_session_titled(&target.prefix, &target.title);
                }
                if !text.trim().is_empty() {
                    self.submit_text(text);
                }
            }
            Reply::Notice(text) => {
                self.panel = None;
                self.notice(text);
            }
            Reply::Close => self.panel = None,
        }
    }

    /// Polls a panel that asked for it (a login waiting for approval).
    pub fn panel_tick(&mut self) {
        let due = self.panel.as_ref().is_some_and(|v| {
            !v.busy
                && v.input.is_none()
                && v.panel
                    .as_ref()
                    .and_then(|p| p.poll_ms)
                    .is_some_and(|ms| v.last_poll.elapsed().as_millis() as u64 >= ms)
        });
        if due {
            self.panel_act(Action::Poll);
        }
    }

    pub fn panel_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some(v) = &mut self.panel else { return };
        if let Some(input) = &mut v.input {
            match k.code {
                KeyCode::Esc => v.input = None,
                KeyCode::Enter => {
                    let text = std::mem::take(&mut input.text);
                    v.input = None;
                    self.panel_act(Action::Text(text));
                }
                KeyCode::Backspace => {
                    input.text.pop();
                }
                KeyCode::Char(c) if !ctrl => input.text.push(c),
                _ => {}
            }
            return;
        }
        let count = v.ids().len();
        match k.code {
            KeyCode::Esc => self.panel = None,
            KeyCode::Char('c') if ctrl => self.panel = None,
            KeyCode::Up => v.selected = v.selected.saturating_sub(1),
            KeyCode::Down => v.selected = (v.selected + 1).min(count.saturating_sub(1)),
            KeyCode::PageUp => v.selected = v.selected.saturating_sub(10),
            KeyCode::PageDown => v.selected = (v.selected + 10).min(count.saturating_sub(1)),
            KeyCode::Enter if !v.busy => {
                if let Some(id) = v.selected_id() {
                    self.panel_act(Action::Enter(id));
                }
            }
            KeyCode::Char(c) if !v.busy && v.panel.as_ref().is_some_and(|p| p.keys.iter().any(|(k, _)| *k == c)) => {
                let id = v.selected_id();
                self.panel_act(Action::Key(c, id));
            }
            _ => {}
        }
    }

    /// Text pasted while a panel asks for input.
    pub fn panel_paste(&mut self, text: &str) -> bool {
        match self.panel.as_mut().and_then(|v| v.input.as_mut()) {
            Some(input) => {
                input.text.push_str(text);
                true
            }
            None => self.panel.is_some(),
        }
    }
}

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn bar(fraction: f64, width: usize) -> Vec<Span<'static>> {
    let full = (fraction * width as f64).round() as usize;
    let style = if fraction >= 0.9 {
        Style::new().red()
    } else if fraction >= 0.7 {
        Style::new().yellow()
    } else {
        Style::new().green()
    };
    vec![Span::styled("█".repeat(full), style), Span::styled("░".repeat(width - full.min(width)), Style::new().dim())]
}

pub fn draw(app: &App, f: &mut Frame) {
    let Some(v) = &app.panel else { return };
    let area = f.area();
    let w = area.width.saturating_sub(6).min(110);
    let h = area.height.saturating_sub(4).min(40);
    let popup = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    f.render_widget(Clear, popup);
    super::style::paint(f, popup);
    let title = v.panel.as_ref().map(|p| format!(" {} ", p.title)).unwrap_or_else(|| format!(" /{} ", v.command));
    let block = Block::bordered().border_type(BorderType::Rounded).title(title).padding(Padding::horizontal(1));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    let input_height = if v.input.is_some() { 3 } else { 0 };
    let [message, body, input_area, hint] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(input_height),
        Constraint::Length(1),
    ])
    .areas(inner);

    let spin = SPINNER[app.tick % SPINNER.len()];
    let msg = match (&v.panel, v.busy) {
        (_, true) => Line::from(vec![format!("{spin} ").cyan(), "working…".dim()]),
        (Some(p), _) => match &p.message {
            Some((text, t)) => Line::from(Span::styled(text.clone(), tone(*t))),
            None => Line::from(""),
        },
        (None, _) => Line::from(""),
    };
    f.render_widget(msg, message);

    let mut lines: Vec<Line> = Vec::new();
    let mut selected_line = 0;
    if let Some(p) = &v.panel {
        let label_width =
            p.sections.iter().flat_map(|s| &s.rows).map(|r| r.label.chars().count()).max().unwrap_or(0).min(34);
        let mut index = 0;
        for (si, s) in p.sections.iter().enumerate() {
            if si > 0 {
                lines.push(Line::from(""));
            }
            if !s.title.is_empty() {
                lines.push(Line::from(s.title.clone().bold()));
            }
            for r in &s.rows {
                let picked = r.id.is_some() && index == v.selected;
                if picked {
                    selected_line = lines.len();
                }
                let mut spans = vec![
                    if picked { "› ".cyan().bold() } else { "  ".into() },
                    Span::styled(
                        format!("{:<label_width$}", r.label),
                        if picked { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() },
                    ),
                    "  ".into(),
                ];
                if let Some(fr) = r.bar {
                    spans.extend(bar(fr, 20));
                    spans.push(" ".into());
                }
                spans.push(Span::styled(r.value.clone(), tone(r.tone)));
                lines.push(Line::from(spans));
                for d in &r.detail {
                    lines.push(Line::from(format!("    {d}").dim()));
                }
                if r.id.is_some() {
                    index += 1;
                }
            }
        }
    }
    let view = body.height as usize;
    let top = selected_line.saturating_sub(view.saturating_sub(3));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((top as u16, 0)), body);

    if let Some(input) = &v.input {
        let shown = if input.secret { "•".repeat(input.text.chars().count()) } else { input.text.clone() };
        f.render_widget(
            Paragraph::new(vec![
                Line::from(input.hint.clone().dim()),
                Line::from(vec![format!("{}: ", input.title).bold(), shown.into(), "▏".dim()]),
            ])
            .wrap(Wrap { trim: false }),
            input_area,
        );
    }
    let keys = match (&v.input, &v.panel) {
        (Some(_), _) => "enter confirm · esc cancel".to_string(),
        (None, Some(p)) => {
            let mut k = vec!["↑↓ move".to_string(), "enter select".to_string()];
            k.extend(p.keys.iter().map(|(c, what)| format!("{c} {what}")));
            k.push("esc close".into());
            k.join(" · ")
        }
        (None, None) => "esc close".into(),
    };
    f.render_widget(Line::from(keys.dim()), hint);
}
