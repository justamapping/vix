use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::buffer::{Buffer, Mode};
use crate::picker::Picker;

/// Screen column of char index `col`.
fn x_of(text: &str, col: usize) -> u16 {
    text.chars().take(col).collect::<String>().width() as u16
}

/// Char index of the cell at screen column `x`.
pub fn col_at(text: &str, x: u16) -> usize {
    let mut width = 0;
    for (i, c) in text.chars().enumerate() {
        width += c.width().unwrap_or(0);
        if width > x as usize {
            return i;
        }
    }
    text.chars().count()
}

/// The bottom row's text when no command line or message takes it over.
pub struct Bar {
    pub left: String,
    pub right: String,
}

/// The command line or a message, which take over the bottom row.
fn prompt(buf: &Buffer) -> Option<String> {
    match (&buf.mode, &buf.message) {
        (Mode::Command, _) => Some(format!("{}{}", buf.prompt, buf.cmdline)),
        (_, Some(msg)) => Some(msg.clone()),
        _ => None,
    }
}

/// What a buffer drawn over a terminal without a bar says, vim-style.
fn overlay(buf: &Buffer) -> String {
    prompt(buf).unwrap_or_else(|| buf.mode.label().into())
}

/// Reverses the visual selection's visible cells, with `top` the buffer row at y = 0.
fn highlight(frame: &mut Frame, buf: &Buffer, top: usize, height: u16) {
    let Some(((r0, c0), (r1, c1), line)) = buf.selection() else { return };
    let width = frame.area().width;
    let out = frame.buffer_mut();
    for y in 0..height {
        let row = top + y as usize;
        if row < r0 || row > r1 {
            continue;
        }
        let text = &buf.lines[row].text;
        let (x0, x1) = if line {
            (0, width)
        } else {
            let start = if row == r0 { x_of(text, c0) } else { 0 };
            let end = if row == r1 { x_of(text, c1 + 1).max(start + 1) } else { x_of(text, usize::MAX).max(1) };
            (start, end)
        };
        for x in x0..x1.min(width) {
            out[(x, y)].modifier.toggle(Modifier::REVERSED);
        }
    }
}

/// Draws the bottom row: always with a bar, else only when there's something to say.
fn bottom(frame: &mut Frame, buf: Option<&Buffer>, bar: Option<&Bar>) {
    let area = frame.area();
    let row = Rect { y: area.height.saturating_sub(1), height: 1, ..area };
    let Some(bar) = bar else {
        let text = buf.map(overlay).unwrap_or_default();
        if !text.is_empty() {
            frame.render_widget(Clear, row);
            frame.render_widget(Paragraph::new(text), row);
        }
        return;
    };
    frame.render_widget(Clear, row);
    if !buf.is_some_and(|b| b.mode == Mode::Command) {
        frame.render_widget(Paragraph::new(bar.right.as_str()).right_aligned(), row);
    }
    let left = buf.and_then(prompt).unwrap_or_else(|| bar.left.clone());
    frame.render_widget(Paragraph::new(left), row);
}

fn cursor(frame: &mut Frame, buf: &Buffer, top: usize) {
    let bottom = frame.area().height.saturating_sub(1);
    let at = if buf.mode == Mode::Command {
        Position::new(1 + buf.cmdline.width() as u16, bottom)
    } else {
        let (r, c) = buf.cursor;
        Position::new(x_of(&buf.lines[r].text, c), r.saturating_sub(top) as u16)
    };
    frame.set_cursor_position(at);
}

pub fn list(frame: &mut Frame, buf: &Buffer, bar: &Bar) {
    let area = frame.area();
    let height = area.height.saturating_sub(1);
    let tilde = Style::new().fg(Color::Blue);
    let rows: Vec<Line> = (buf.top..buf.top + height as usize)
        .map(|i| match buf.lines.get(i) {
            Some(l) => Line::raw(l.text.as_str()),
            None => Line::styled("~", tilde),
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), area);
    highlight(frame, buf, buf.top, height);
    bottom(frame, Some(buf), Some(bar));
    cursor(frame, buf, buf.top);
}

fn color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// Copies a terminal's screen (at its current scrollback offset) into the frame.
/// With `normal`, that buffer's cursor, selection, and command line are drawn over it (view n).
/// With `bar`, the screen is a row shorter than the frame and the bar takes the bottom row.
pub fn term(frame: &mut Frame, screen: &vt100::Screen, normal: Option<&Buffer>, bar: Option<&Bar>) {
    let area = frame.area();
    // like vim, scroll a line when the bottom row would cover the cursor
    let shift = match (normal, bar) {
        (Some(b), None) => u16::from(!overlay(b).is_empty() && b.cursor.0 + 1 >= b.top + area.height as usize),
        _ => 0,
    };
    let out = frame.buffer_mut();
    for y in 0..area.height {
        for x in 0..area.width {
            let o = &mut out[(x, y)];
            let Some(cell) = screen.cell(y + shift, x) else {
                o.reset();
                continue;
            };
            if cell.is_wide_continuation() {
                o.reset();
                continue;
            }
            let mut m = Modifier::empty();
            m.set(Modifier::BOLD, cell.bold());
            m.set(Modifier::DIM, cell.dim());
            m.set(Modifier::ITALIC, cell.italic());
            m.set(Modifier::UNDERLINED, cell.underline());
            m.set(Modifier::REVERSED, cell.inverse());
            let symbol = if cell.has_contents() { cell.contents() } else { " " };
            o.set_symbol(symbol)
                .set_style(Style::new().fg(color(cell.fgcolor())).bg(color(cell.bgcolor())).add_modifier(m));
        }
    }
    match normal {
        Some(buf) => {
            let top = buf.top + shift as usize;
            highlight(frame, buf, top, area.height);
            bottom(frame, Some(buf), bar);
            cursor(frame, buf, top);
        }
        None => {
            bottom(frame, None, bar);
            if !screen.hide_cursor() {
                let (row, col) = screen.cursor_position();
                frame.set_cursor_position(Position::new(col, row));
            }
        }
    }
}

/// Chars of `text` as spans, with the ones `hit` says matched in bold yellow.
fn marked(text: &str, base: Style, hit: impl Fn(usize) -> bool) -> Vec<Span<'static>> {
    let on = base.fg(Color::Yellow).add_modifier(Modifier::BOLD);
    text.chars().enumerate().map(|(i, c)| Span::styled(c.to_string(), if hit(i) { on } else { base })).collect()
}

/// A centered box over whatever is drawn: the query on top, matches below.
pub fn picker(frame: &mut Frame, p: &Picker) {
    let area = frame.area();
    let width = area.width.saturating_sub(4).min(80);
    let rows = area.height.saturating_sub(4).min(p.matches.len().max(1) as u16 + 1);
    let outer = Rect { x: (area.width - width) / 2, y: area.height.saturating_sub(rows + 2) / 2, width, height: (rows + 2).min(area.height) };
    let block = Block::bordered().title(" terminals ").title_bottom(Line::from(format!(" {}/{} ", p.matches.len(), p.items.len())).right_aligned());
    let inner = block.inner(outer);
    frame.render_widget(Clear, outer);
    frame.render_widget(block, outer);

    let shown = inner.height.saturating_sub(1) as usize;
    let top = (p.selected + 1).saturating_sub(shown);
    let name_width = p.items.iter().map(|i| i.name.width()).max().unwrap_or(0);
    let dim = Style::new().fg(Color::Indexed(245));
    let mut lines = vec![Line::from(format!("> {}", p.query))];
    for (row, m) in p.matches.iter().enumerate().skip(top).take(shown) {
        let item = &p.items[m.item];
        let selected = row == p.selected;
        let base = if selected { Style::new().bg(Color::Indexed(238)) } else { Style::new() };
        let mut spans = vec![Span::styled(format!("{} {:>2}  ", if selected { ">" } else { " " }, item.index), base.patch(dim))];
        spans.extend(marked(&item.name, base, |i| m.in_name(i)));
        spans.push(Span::styled(" ".repeat(name_width - item.name.width() + 2), base));
        spans.extend(marked(&item.cwd, base.patch(dim), |i| m.in_cwd(item, i)));
        let mut line = Line::from(spans);
        if selected {
            line = line.style(base);
        }
        lines.push(line);
    }
    frame.render_widget(Paragraph::new(lines), inner);
    frame.set_cursor_position(Position::new(inner.x + 2 + p.query.width() as u16, inner.y));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x_counts_wide_chars() {
        assert_eq!(x_of("ab", 1), 1);
        assert_eq!(x_of("日本x", 2), 4);
        assert_eq!(x_of("ab", 9), 2);
    }

    #[test]
    fn col_at_inverts_x_of() {
        assert_eq!(col_at("ab", 1), 1);
        assert_eq!(col_at("日本x", 2), 1);
        assert_eq!(col_at("日本x", 3), 1);
        assert_eq!(col_at("日本x", 4), 2);
        assert_eq!(col_at("ab", 9), 2);
    }
}
