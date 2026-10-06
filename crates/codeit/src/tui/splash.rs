//! The short animation shown when the TUI opens: a large `>_ codeit` appears faint and a light passes
//! over it; then the UI opens. Any key skips it; `CODEIT_SPLASH=off` turns it off.

use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event, KeyEventKind},
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Line,
};

type Rgb = (u8, u8, u8);
const BLUE: Rgb = (0, 135, 175);
const WHITE: Rgb = (230, 230, 230);
const LOGO: [&str; 3] = ["▀▄       ┏━╸┏━┓╺┳┓┏━╸╻╺┳╸", "  █      ┃  ┃ ┃ ┃┃┣╸ ┃ ┃ ", "▄▀  ━━━  ┗━╸┗━┛╺┻┛┗━╸╹ ╹ "];
/// Milestones, in milliseconds: faint, lit, held.
const FAINT: u64 = 450;
const LIT: u64 = 1600;
const HELD: u64 = 2600;

pub fn run(terminal: &mut DefaultTerminal) -> Result<()> {
    if std::env::var("CODEIT_SPLASH").as_deref() == Ok("off") {
        return Ok(());
    }
    let start = Instant::now();
    loop {
        let t = start.elapsed().as_millis() as u64;
        if t >= HELD {
            return Ok(());
        }
        terminal.draw(|f| {
            super::style::paint(f, f.area());
            logo(f, t)
        })?;
        if event::poll(Duration::from_millis(30))?
            && let Event::Key(k) = event::read()?
            && k.kind == KeyEventKind::Press
        {
            return Ok(());
        }
    }
}

/// The logo at time `t`: ">_" and "it" in blue, "code" in white.
fn logo(f: &mut Frame, t: u64) {
    let w = LOGO[0].chars().count();
    let area = f.area();
    let x = area.x + area.width.saturating_sub(w as u16) / 2;
    let y = (area.y + area.height / 2).saturating_sub(1);
    let light = -3.0 + (w as f32 + 6.0) * ease(prog(t, FAINT, LIT));
    for (r, row) in LOGO.iter().enumerate() {
        for (i, c) in row.chars().enumerate() {
            let (cx, cy) = (x + i as u16, y + r as u16);
            if c == ' ' || cx >= area.right() || cy >= area.bottom() {
                continue;
            }
            let color = if (9..21).contains(&i) { WHITE } else { BLUE };
            let (bright, glow) = match light - i as f32 {
                _ if t < FAINT => (0.18 * prog(t, 0, FAINT), 0.0),
                d if d > 1.5 => (1.0, 0.0),
                d if d > -1.5 => (1.0, 1.0 - d.abs() / 1.5),
                _ => (0.18, 0.0),
            };
            put(f, cx, cy, &c.to_string(), mix(color, (255, 255, 255), glow), bright);
        }
    }
}

// --- helpers -------------------------------------------------------------------------------------

/// Writes `s` at `(x, y)` in `color` at brightness `k`, clipped to the screen.
fn put(f: &mut Frame, x: u16, y: u16, s: &str, color: Rgb, k: f32) {
    let area = f.area();
    if s.is_empty() || k <= 0.01 || y >= area.bottom() || x >= area.right() {
        return;
    }
    let style = Style::new().fg(rgb(color, k)).add_modifier(Modifier::BOLD);
    let room = (area.right() - x) as usize;
    f.render_widget(Line::styled(s.to_string(), style), Rect::new(x, y, room.min(s.chars().count()) as u16, 1));
}

fn rgb((r, g, b): Rgb, k: f32) -> Color {
    let k = k.clamp(0.0, 1.0);
    Color::Rgb((r as f32 * k) as u8, (g as f32 * k) as u8, (b as f32 * k) as u8)
}

fn mix(a: Rgb, b: Rgb, p: f32) -> Rgb {
    let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * p.clamp(0.0, 1.0)) as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

/// How far `t` is from `a` to `b`, from 0 to 1.
fn prog(t: u64, a: u64, b: u64) -> f32 {
    (t.saturating_sub(a) as f32 / (b - a) as f32).min(1.0)
}

fn ease(p: f32) -> f32 {
    p * p * (3.0 - 2.0 * p)
}
