//! The look shared by every screen: colors, bubbles and frames, wrapping.

use ratatui::{
    style::{Color, Style, Stylize},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Your messages and the input box.
pub const BLUE: Color = Color::Rgb(0, 135, 175);
/// The agent's answers and the side bubble.
pub const AGENT: Color = Color::DarkGray;
/// The frame around actions: barely there.
pub const FAINT: Color = Color::Rgb(62, 62, 62);
/// Failures, without shouting.
pub const SOFT_RED: Color = Color::Rgb(170, 95, 95);
pub const ADD: Color = Color::Rgb(110, 170, 110);
/// Waiting on something.
pub const SOFT_YELLOW: Color = Color::Rgb(190, 170, 95);
/// Something is broken.
pub const RED: Color = Color::Rgb(215, 75, 75);
pub const SELECT: Color = Color::Rgb(44, 48, 66);
pub const ADD_BG: Color = Color::Rgb(18, 48, 24);
pub const DEL_BG: Color = Color::Rgb(60, 20, 20);

/// The screen's background unless the config says otherwise: a soft dark gray, like the
/// macOS Terminal's.
const BACKGROUND: Color = Color::Rgb(30, 30, 30);
static BG: std::sync::OnceLock<Color> = std::sync::OnceLock::new();

/// Sets the background from the config's `tui.background`: `#rrggbb`, or `none` for the
/// terminal's own. Returns a problem to report when the value isn't understood.
pub fn set_background(value: Option<&str>) -> Option<String> {
    let (color, problem) = match value.map(str::trim) {
        None => (BACKGROUND, None),
        Some("none") => (Color::Reset, None),
        Some(v) => match parse_hex(v) {
            Some(c) => (c, None),
            None => (BACKGROUND, Some(format!("tui.background: `{v}` isn't #rrggbb or none"))),
        },
    };
    let _ = BG.set(color);
    problem
}

fn parse_hex(v: &str) -> Option<Color> {
    let h = v.strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(h, 16).ok()?;
    Some(Color::Rgb((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

/// The screen's background.
pub fn bg() -> Color {
    *BG.get().unwrap_or(&BACKGROUND)
}

/// Sets the terminal's own background to ours, so the margin around the grid (which no
/// cell covers) matches too; `restore_terminal_bg` undoes it.
pub fn set_terminal_bg() {
    if let Color::Rgb(r, g, b) = bg() {
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b]11;#{r:02x}{g:02x}{b:02x}\x1b\\");
        let _ = out.flush();
    }
}

pub fn restore_terminal_bg() {
    if matches!(bg(), Color::Rgb(..)) {
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b]111\x1b\\");
        let _ = out.flush();
    }
}

/// Paints the whole area with the background, under what is drawn next.
pub fn paint(f: &mut ratatui::Frame, area: ratatui::layout::Rect) {
    f.buffer_mut().set_style(area, Style::new().bg(bg()));
}

pub fn width(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.content.width()).sum()
}

/// Spans padded with `style` to `w` columns.
pub fn pad(mut spans: Vec<Span<'static>>, w: usize, style: Style) -> Line<'static> {
    let n = w.saturating_sub(width(&spans));
    spans.push(Span::styled(" ".repeat(n), style));
    Line::from(spans)
}

/// `left`, then `right` pushed to the right edge of `w` columns.
pub fn right(mut left: Vec<Span<'static>>, right: Vec<Span<'static>>, w: usize) -> Line<'static> {
    let n = w.saturating_sub(width(&left) + width(&right)).max(1);
    left.push(" ".repeat(n).into());
    left.extend(right);
    Line::from(left)
}

/// At most `max` columns, ending in `…` when cut.
pub fn cut(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw + 1 > max {
            break;
        }
        used += cw;
        out.push(c);
    }
    out.push('…');
    out
}

/// Words wrapped to `w` columns; line breaks kept.
pub fn wrap(text: &str, w: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.lines() {
        let mut line = String::new();
        for word in para.split_whitespace() {
            if !line.is_empty() && line.width() + 1 + word.width() > w.max(10) {
                out.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        out.push(line);
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out
}

/// Wraps a styled line to `w` columns, breaking at spaces where it can. Continuation lines
/// start under the text: after leading spaces and a list marker.
pub fn wrap_spans(line: Line<'static>, w: usize) -> Vec<Line<'static>> {
    let base = line.style;
    let cells: Vec<(char, Style)> =
        line.spans.iter().flat_map(|s| s.content.chars().map(move |c| (c, base.patch(s.style)))).collect();
    let cw = |c: char| c.width().unwrap_or(0);
    let text: String = cells.iter().map(|c| c.0).collect();
    if text.width() <= w {
        return vec![line];
    }
    let body = text.trim_start();
    let marker = ["• ", "◦ ", "▪ ", "- ", "* ", "│ "]
        .iter()
        .find(|m| body.starts_with(**m))
        .map(|m| m.width())
        .or_else(|| {
            let digits = body.chars().take_while(|c| c.is_ascii_digit()).count();
            (digits > 0 && body[digits..].starts_with(". ")).then_some(digits + 2)
        })
        .unwrap_or(0);
    let hang = (text.width() - body.width() + marker).min(w / 2);
    let mut rows: Vec<Vec<(char, Style)>> = vec![Vec::new()];
    let mut used = 0;
    for (c, st) in cells {
        if used + cw(c) > w {
            let row = rows.last_mut().unwrap();
            let next = match row.iter().rposition(|(ch, _)| *ch == ' ') {
                Some(i) if i > hang => {
                    let rest = row.split_off(i + 1);
                    row.pop();
                    rest
                }
                _ => Vec::new(),
            };
            let mut next_row: Vec<(char, Style)> = vec![(' ', Style::new()); hang];
            next_row.extend(next);
            used = next_row.iter().map(|(c, _)| cw(*c)).sum();
            rows.push(next_row);
            if c == ' ' && rows.last().unwrap().len() == hang {
                continue;
            }
        }
        used += cw(c);
        rows.last_mut().unwrap().push((c, st));
    }
    rows.into_iter()
        .map(|row| {
            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut cur = String::new();
            let mut style = None;
            for (c, st) in row {
                if style.is_some_and(|s| s != st) {
                    spans.push(Span::styled(std::mem::take(&mut cur), style.unwrap()));
                }
                style = Some(st);
                cur.push(c);
            }
            if let Some(st) = style {
                spans.push(Span::styled(cur, st));
            }
            Line::from(spans)
        })
        .collect()
}

/// A rounded box `w` wide around `rows` (each with a tag, kept on every wrapped line), with
/// `label` in the bottom-right corner.
pub fn frame<T: Clone>(
    rows: Vec<(Line<'static>, T)>,
    w: usize,
    color: Color,
    label: Vec<Span<'static>>,
    none: T,
) -> Vec<(Line<'static>, T)> {
    let border = Style::new().fg(color);
    let inner = w.saturating_sub(4);
    let mut out =
        vec![(Line::from(Span::styled(format!("╭{}╮", "─".repeat(w.saturating_sub(2))), border)), none.clone())];
    for (l, tag) in rows {
        let bg = l.style.bg;
        for l in wrap_spans(l, inner) {
            let mut spans = vec![Span::styled("│ ", border)];
            let n = inner.saturating_sub(width(&l.spans));
            spans.extend(l.spans);
            spans.push(Span::styled(" ".repeat(n), Style::new().bg(bg.unwrap_or(self::bg()))));
            spans.push(Span::styled(" │", border));
            out.push((Line::from(spans), tag.clone()));
        }
    }
    let mut bottom = vec![Span::styled("╰", border)];
    let tail = if label.is_empty() { 0 } else { width(&label) + 2 };
    bottom.push(Span::styled("─".repeat(w.saturating_sub(tail + 3)), border));
    if !label.is_empty() {
        bottom.push(" ".into());
        bottom.extend(label);
        bottom.push(" ".into());
    }
    bottom.push(Span::styled("─╯", border));
    out.push((Line::from(bottom), none));
    out
}

/// A thin bar `w` wide, `pct` full; soft red past 90%.
pub fn meter(pct: u64, w: usize) -> Vec<Span<'static>> {
    let fill = (w as u64 * pct.min(100) / 100) as usize;
    let color = if pct >= 90 { SOFT_RED } else { BLUE };
    vec![
        "━".repeat(fill).fg(color),
        "━".repeat(w.saturating_sub(fill)).fg(Color::Rgb(66, 66, 66)),
        format!(" {pct:>2}%").fg(Color::Gray),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn wraps_lists_under_their_text_and_frames_rows() {
        let lines = wrap_spans(Line::from("• one two three four"), 10);
        assert_eq!(lines.iter().map(text).collect::<Vec<_>>(), ["• one two", "  three", "  four"]);
        let boxed = frame(vec![(Line::from("hi"), 1)], 8, BLUE, vec!["x".into()], 0);
        let t: Vec<String> = boxed.iter().map(|r| text(&r.0)).collect();
        assert_eq!(t, ["╭──────╮", "│ hi   │", "╰── x ─╯"]);
        assert_eq!(boxed.iter().map(|r| r.1).collect::<Vec<_>>(), [0, 1, 0]);
        assert_eq!(cut("abcdef", 4), "abc…");
        assert_eq!(parse_hex("#1e1e1e"), Some(Color::Rgb(30, 30, 30)));
        assert_eq!(parse_hex("1e1e1e"), None);
    }
}
