use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::buffer::{Buffer, Mode};

/// Screen column of char index `col`.
fn x_of(text: &str, col: usize) -> u16 {
    text.chars().take(col).collect::<String>().width() as u16
}

/// What the bottom row says, vim-style.
fn status(buf: &Buffer) -> String {
    match (&buf.mode, &buf.message) {
        (Mode::Command, _) => format!("{}{}", buf.prompt, buf.cmdline),
        (_, Some(msg)) => msg.clone(),
        (Mode::Insert, _) => "-- INSERT --".into(),
        (Mode::Visual { line: false }, _) => "-- VISUAL --".into(),
        (Mode::Visual { line: true }, _) => "-- VISUAL LINE --".into(),
        (Mode::Normal, _) => String::new(),
    }
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

/// Draws the bottom row (if `always` or there's something to say) and places the cursor.
fn chrome(frame: &mut Frame, buf: &Buffer, top: usize, always: bool) {
    let area = frame.area();
    let bottom = area.height.saturating_sub(1);
    let status = status(buf);
    let row = Rect { y: bottom, height: 1, ..area };
    if always || !status.is_empty() {
        frame.render_widget(Clear, row);
        frame.render_widget(Paragraph::new(status.as_str()), row);
    }
    if always && buf.modified() {
        frame.render_widget(Paragraph::new("[+]").right_aligned(), row);
    }
    let cursor = if buf.mode == Mode::Command {
        Position::new(1 + buf.cmdline.width() as u16, bottom)
    } else {
        let (r, c) = buf.cursor;
        Position::new(x_of(&buf.lines[r].text, c), r.saturating_sub(top) as u16)
    };
    frame.set_cursor_position(cursor);
}

pub fn list(frame: &mut Frame, buf: &Buffer) {
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
    chrome(frame, buf, buf.top, true);
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
pub fn term(frame: &mut Frame, screen: &vt100::Screen, normal: Option<&Buffer>) {
    let area = frame.area();
    // like vim, scroll a line when the bottom row would cover the cursor
    let shift = normal.map_or(0, |b| {
        let covered = !status(b).is_empty() && b.cursor.0 + 1 >= b.top + area.height as usize;
        u16::from(covered)
    });
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
            chrome(frame, buf, top, false);
        }
        None if !screen.hide_cursor() => {
            let (row, col) = screen.cursor_position();
            frame.set_cursor_position(Position::new(col, row));
        }
        None => {}
    }
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
}
