//! Markdown replies as terminal lines: headings, emphasis, inline code, links, nested lists,
//! quotes, rules, tables (columns sized to fit) and code blocks with syntax highlighting.

use std::sync::OnceLock;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use unicode_width::UnicodeWidthStr;

fn syntaxes() -> &'static SyntaxSet {
    static S: OnceLock<SyntaxSet> = OnceLock::new();
    S.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme() -> &'static Theme {
    static T: OnceLock<Theme> = OnceLock::new();
    T.get_or_init(|| ThemeSet::load_defaults().themes.remove("base16-eighties.dark").expect("bundled theme"))
}

/// A code block's lines, highlighted for its language (plain cyan when unknown).
fn code_lines(code: &str, lang: &str) -> Vec<Vec<Span<'static>>> {
    let ss = syntaxes();
    let syntax = ss
        .find_syntax_by_token(lang)
        .or_else(|| ss.find_syntax_by_extension(lang))
        .or_else(|| (lang == "ts" || lang == "typescript").then(|| ss.find_syntax_by_extension("js")).flatten());
    let Some(syntax) = syntax else {
        return code.lines().map(|l| vec![Span::styled(l.to_string(), Style::new().cyan())]).collect();
    };
    let mut h = HighlightLines::new(syntax, theme());
    let mut out = Vec::new();
    for line in syntect::util::LinesWithEndings::from(code) {
        let spans = match h.highlight_line(line, ss) {
            Ok(parts) => parts
                .into_iter()
                .map(|(st, text)| {
                    let c = st.foreground;
                    Span::styled(text.trim_end_matches('\n').to_string(), Style::new().fg(Color::Rgb(c.r, c.g, c.b)))
                })
                .collect(),
            Err(_) => vec![Span::raw(line.trim_end_matches('\n').to_string())],
        };
        out.push(spans);
    }
    out
}

struct Renderer {
    width: usize,
    lines: Vec<Line<'static>>,
    /// The line being built.
    cur: Vec<Span<'static>>,
    styles: Vec<Style>,
    /// Open lists: the next number, or `None` for bullets.
    lists: Vec<Option<u64>>,
    quote: usize,
    /// A list item just started: its marker goes before the first text.
    item: Option<String>,
    code: Option<(String, String)>,
    link: Option<String>,
    table: Option<Table>,
}

#[derive(Default)]
struct Table {
    align: Vec<Alignment>,
    rows: Vec<Vec<String>>,
    cell: String,
    head: bool,
}

impl Renderer {
    fn style(&self) -> Style {
        self.styles.iter().fold(Style::new(), |a, s| a.patch(*s))
    }

    /// Indentation for quotes and lists.
    fn prefix(&self) -> Vec<Span<'static>> {
        let mut p = Vec::new();
        for _ in 0..self.quote {
            p.push("▎ ".dim());
        }
        if !self.lists.is_empty() {
            p.push(Span::raw("  ".repeat(self.lists.len() - 1)));
        }
        p
    }

    fn text(&mut self, t: &str) {
        if let Some(tb) = &mut self.table {
            tb.cell.push_str(t);
            return;
        }
        if self.cur.is_empty() {
            self.cur = self.prefix();
            if let Some(m) = self.item.take() {
                self.cur.push(m.dim());
            } else if !self.lists.is_empty() {
                self.cur.push(Span::raw("  "));
            }
        }
        self.cur.push(Span::styled(t.to_string(), self.style()));
    }

    fn flush(&mut self) {
        if !self.cur.is_empty() {
            self.lines.push(Line::from(std::mem::take(&mut self.cur)));
        }
    }

    fn blank(&mut self) {
        self.flush();
        if self.lines.last().is_some_and(|l| !l.spans.iter().all(|s| s.content.trim().is_empty())) {
            self.lines.push(Line::from(""));
        }
    }

    fn table_lines(&mut self, t: Table) {
        let cols = t.rows.iter().map(Vec::len).max().unwrap_or(0);
        if cols == 0 {
            return;
        }
        let mut widths = vec![0usize; cols];
        for r in &t.rows {
            for (i, c) in r.iter().enumerate() {
                widths[i] = widths[i].max(c.width());
            }
        }
        // Shrink the widest columns until the table fits.
        let budget = self.width.saturating_sub(cols * 3 + 1).max(cols * 3);
        while widths.iter().sum::<usize>() > budget {
            let (i, _) = widths.iter().enumerate().max_by_key(|(_, w)| **w).expect("has columns");
            if widths[i] <= 3 {
                break;
            }
            widths[i] -= 1;
        }
        let fit = |s: &str, w: usize, a: Alignment| -> String {
            let mut s = s.to_string();
            if s.width() > w {
                while s.width() > w.saturating_sub(1) {
                    s.pop();
                }
                s.push('…');
            }
            let pad = w.saturating_sub(s.width());
            match a {
                Alignment::Right => format!("{}{s}", " ".repeat(pad)),
                Alignment::Center => format!("{}{s}{}", " ".repeat(pad / 2), " ".repeat(pad - pad / 2)),
                _ => format!("{s}{}", " ".repeat(pad)),
            }
        };
        let rule = |l: &str, m: &str, r: &str| -> Line<'static> {
            Line::from(format!("{l}{}{r}", widths.iter().map(|w| "─".repeat(w + 2)).collect::<Vec<_>>().join(m)).dim())
        };
        let prefix = self.prefix();
        let push = |line: Line<'static>, lines: &mut Vec<Line<'static>>| {
            let mut spans = prefix.clone();
            spans.extend(line.spans);
            lines.push(Line::from(spans));
        };
        push(rule("┌", "┬", "┐"), &mut self.lines);
        for (ri, r) in t.rows.iter().enumerate() {
            let mut spans = vec!["│".dim()];
            for (i, w) in widths.iter().enumerate() {
                let cell = fit(
                    r.get(i).map(String::as_str).unwrap_or(""),
                    *w,
                    t.align.get(i).copied().unwrap_or(Alignment::None),
                );
                spans.push(" ".into());
                spans.push(if ri == 0 && t.head { cell.bold() } else { cell.into() });
                spans.push(" │".dim());
            }
            push(Line::from(spans), &mut self.lines);
            if ri == 0 && t.head {
                push(rule("├", "┼", "┤"), &mut self.lines);
            }
        }
        push(rule("└", "┴", "┘"), &mut self.lines);
    }

    fn event(&mut self, ev: Event) {
        match ev {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    self.blank();
                    self.styles.push(match level {
                        HeadingLevel::H1 => Style::new().bold().underlined(),
                        HeadingLevel::H2 => Style::new().bold(),
                        _ => Style::new().bold().italic(),
                    });
                }
                Tag::Paragraph => {}
                Tag::Emphasis => self.styles.push(Style::new().add_modifier(Modifier::ITALIC)),
                Tag::Strong => self.styles.push(Style::new().add_modifier(Modifier::BOLD)),
                Tag::Strikethrough => self.styles.push(Style::new().add_modifier(Modifier::CROSSED_OUT)),
                Tag::Link { dest_url, .. } => {
                    self.link = Some(dest_url.to_string());
                    self.styles.push(Style::new().underlined().cyan());
                }
                Tag::BlockQuote(_) => {
                    self.flush();
                    self.quote += 1;
                    self.styles.push(Style::new().italic());
                }
                Tag::List(start) => {
                    self.flush();
                    self.lists.push(start);
                }
                Tag::Item => {
                    self.flush();
                    let marker = match self.lists.last_mut() {
                        Some(Some(n)) => {
                            let m = format!("{n}. ");
                            *n += 1;
                            m
                        }
                        _ => if self.lists.len() > 1 { "◦ " } else { "• " }.to_string(),
                    };
                    self.item = Some(marker);
                }
                Tag::CodeBlock(kind) => {
                    self.flush();
                    let lang = match kind {
                        CodeBlockKind::Fenced(l) => l.split([',', ' ']).next().unwrap_or("").to_string(),
                        CodeBlockKind::Indented => String::new(),
                    };
                    self.code = Some((lang, String::new()));
                }
                Tag::Table(align) => {
                    self.flush();
                    self.table = Some(Table { align, ..Default::default() });
                }
                Tag::TableHead => {
                    if let Some(t) = &mut self.table {
                        t.head = true;
                        t.rows.push(Vec::new());
                    }
                }
                Tag::TableRow => {
                    if let Some(t) = &mut self.table {
                        t.rows.push(Vec::new());
                    }
                }
                Tag::TableCell => {
                    if let Some(t) = &mut self.table {
                        t.cell.clear();
                    }
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Heading(_) => {
                    self.styles.pop();
                    self.flush();
                }
                TagEnd::Paragraph => {
                    self.flush();
                    if self.lists.is_empty() {
                        self.lines.push(Line::from(""));
                    }
                }
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                    self.styles.pop();
                }
                TagEnd::Link => {
                    self.styles.pop();
                    if let Some(url) = self.link.take() {
                        let shown: String = self.cur.iter().map(|s| s.content.as_ref()).collect();
                        if !shown.contains(&url) && !url.starts_with('#') {
                            self.cur.push(format!(" ({url})").dim());
                        }
                    }
                }
                TagEnd::BlockQuote(_) => {
                    self.flush();
                    self.quote = self.quote.saturating_sub(1);
                    self.styles.pop();
                }
                TagEnd::List(_) => {
                    self.flush();
                    self.lists.pop();
                    if self.lists.is_empty() {
                        self.lines.push(Line::from(""));
                    }
                }
                TagEnd::Item => self.flush(),
                TagEnd::CodeBlock => {
                    if let Some((lang, code)) = self.code.take() {
                        let label = if lang.is_empty() { String::new() } else { format!(" {lang} ") };
                        let mut head = self.prefix();
                        head.push(format!("╭─{label}").dim());
                        self.lines.push(Line::from(head));
                        for spans in code_lines(&code, &lang) {
                            let mut l = self.prefix();
                            l.push("│ ".dim());
                            l.extend(spans);
                            self.lines.push(Line::from(l));
                        }
                        let mut foot = self.prefix();
                        foot.push("╰─".dim());
                        self.lines.push(Line::from(foot));
                        if self.lists.is_empty() {
                            self.lines.push(Line::from(""));
                        }
                    }
                }
                TagEnd::TableCell => {
                    if let Some(t) = &mut self.table {
                        let cell = std::mem::take(&mut t.cell);
                        if let Some(r) = t.rows.last_mut() {
                            r.push(cell.trim().to_string());
                        }
                    }
                }
                TagEnd::Table => {
                    if let Some(t) = self.table.take() {
                        self.table_lines(t);
                        self.lines.push(Line::from(""));
                    }
                }
                _ => {}
            },
            Event::Text(t) => match &mut self.code {
                Some((_, code)) => code.push_str(&t),
                None => {
                    let parts: Vec<&str> = t.split('\n').collect();
                    for (i, p) in parts.iter().enumerate() {
                        if i > 0 {
                            self.flush();
                        }
                        if !p.is_empty() {
                            self.text(p);
                        }
                    }
                }
            },
            Event::Code(c) => {
                if let Some(t) = &mut self.table {
                    t.cell.push_str(&c);
                } else {
                    self.text("");
                    self.cur.push(Span::styled(c.to_string(), Style::new().cyan()));
                }
            }
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => self.flush(),
            Event::Rule => {
                self.flush();
                self.lines.push(Line::from("─".repeat(self.width.min(60)).dim()));
            }
            Event::TaskListMarker(done) => {
                if let Some(m) = &mut self.item {
                    *m = if done { "☑ ".into() } else { "☐ ".into() };
                }
            }
            _ => {}
        }
    }
}

/// Renders Markdown to lines no wider than `width` where it matters (tables, rules).
pub fn render(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut r = Renderer {
        width: width.max(20),
        lines: Vec::new(),
        cur: Vec::new(),
        styles: Vec::new(),
        lists: Vec::new(),
        quote: 0,
        item: None,
        code: None,
        link: None,
        table: None,
    };
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for ev in Parser::new_ext(text, opts) {
        r.event(ev);
    }
    // An unclosed code block while streaming: show what arrived.
    if let Some((lang, code)) = r.code.take() {
        for spans in code_lines(&code, &lang) {
            let mut l = vec!["│ ".dim()];
            l.extend(spans);
            r.lines.push(Line::from(l));
        }
    }
    r.flush();
    while r.lines.last().is_some_and(|l| l.spans.iter().all(|s| s.content.trim().is_empty())) {
        r.lines.pop();
    }
    r.lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    #[test]
    fn renders_lists_tables_and_code() {
        let md = "# Title\n\nSome **bold** and `code`.\n\n- one\n- two\n  1. nested\n\n| a | bb |\n|---|---:|\n| x | 1 |\n\n```rust\nfn main() {}\n```\n";
        let out = plain(&render(md, 80));
        assert_eq!(
            out,
            [
                "Title",
                "Some bold and code.",
                "",
                "• one",
                "• two",
                "  1. nested",
                "",
                "┌───┬────┐",
                "│ a │ bb │",
                "├───┼────┤",
                "│ x │  1 │",
                "└───┴────┘",
                "",
                "╭─ rust ",
                "│ fn main() {}",
                "╰─",
            ]
        );
    }

    #[test]
    fn shrinks_wide_tables_and_keeps_partial_code() {
        let md = "| col | long |\n|---|---|\n| x | aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa |\n";
        let out = plain(&render(md, 30));
        assert!(out.iter().all(|l| l.width() <= 30), "{out:?}");
        assert!(out[3].contains('…'));
        // A code block still streaming is shown framed.
        assert_eq!(plain(&render("```py\nprint(1)", 40)), ["╭─ py ", "│ print(1)", "╰─"]);
    }
}
