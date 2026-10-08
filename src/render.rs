use ratatui::Frame;
use ratatui::layout::Position;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::buffer::{Buffer, Mode};

/// First visible row so that `row` stays on screen.
pub fn scroll(top: usize, row: usize, height: usize) -> usize {
    if row < top {
        row
    } else if row >= top + height {
        row + 1 - height
    } else {
        top
    }
}

pub fn list(frame: &mut Frame, buf: &Buffer, top: usize) {
    let area = frame.area();
    let height = area.height.saturating_sub(1) as usize;
    let tilde = Style::new().fg(Color::Blue);
    let rows: Vec<Line> = (top..top + height)
        .map(|i| match buf.lines.get(i) {
            Some(l) => Line::raw(l.text.as_str()),
            None => Line::styled("~", tilde),
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), area);

    let bottom = area.height.saturating_sub(1);
    let status = match (&buf.mode, &buf.message) {
        (Mode::Command, _) => format!(":{}", buf.cmdline),
        (_, Some(msg)) => msg.clone(),
        (Mode::Insert, _) => "-- INSERT --".into(),
        (Mode::Normal, _) => String::new(),
    };
    let modified = if buf.modified() { "[+]" } else { "" };
    let status_area = ratatui::layout::Rect { y: bottom, height: 1, ..area };
    frame.render_widget(Paragraph::new(status.as_str()), status_area);
    frame.render_widget(Paragraph::new(modified).right_aligned(), status_area);

    let cursor = if buf.mode == Mode::Command {
        Position::new(1 + buf.cmdline.chars().count() as u16, bottom)
    } else {
        let (row, col) = buf.cursor;
        let text = &buf.lines[row].text;
        let x = unicode_width::UnicodeWidthStr::width(text.chars().take(col).collect::<String>().as_str());
        Position::new(x as u16, (row - top) as u16)
    };
    frame.set_cursor_position(cursor);
}

fn color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// Copies a terminal's screen into the frame. `tag` is drawn bottom-right (e.g. NORMAL).
pub fn term(frame: &mut Frame, screen: &vt100::Screen, tag: Option<&str>) {
    let area = frame.area();
    let buf = frame.buffer_mut();
    for y in 0..area.height {
        for x in 0..area.width {
            let Some(cell) = screen.cell(y, x) else { continue };
            let out = &mut buf[(x, y)];
            if cell.is_wide_continuation() {
                out.reset();
                continue;
            }
            let mut m = Modifier::empty();
            m.set(Modifier::BOLD, cell.bold());
            m.set(Modifier::DIM, cell.dim());
            m.set(Modifier::ITALIC, cell.italic());
            m.set(Modifier::UNDERLINED, cell.underline());
            m.set(Modifier::REVERSED, cell.inverse());
            let symbol = if cell.has_contents() { cell.contents() } else { " " };
            out.set_symbol(symbol)
                .set_style(Style::new().fg(color(cell.fgcolor())).bg(color(cell.bgcolor())).add_modifier(m));
        }
    }
    if let Some(tag) = tag {
        let w = tag.chars().count() as u16;
        let at = ratatui::layout::Rect { x: area.width.saturating_sub(w), y: area.height.saturating_sub(1), width: w.min(area.width), height: 1 };
        frame.render_widget(Paragraph::new(tag).style(Style::new().add_modifier(Modifier::REVERSED)), at);
    }
    if !screen.hide_cursor() {
        let (row, col) = screen.cursor_position();
        frame.set_cursor_position(Position::new(col, row));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_keeps_row_visible() {
        assert_eq!(scroll(0, 3, 10), 0);
        assert_eq!(scroll(0, 10, 10), 1);
        assert_eq!(scroll(5, 2, 10), 2);
        assert_eq!(scroll(5, 14, 10), 5);
    }
}
