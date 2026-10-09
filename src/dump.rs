use vt100::{Cell, Color, Screen};

use crate::vt::{self, Parser};

const RESTORED: &[u8] = b"\r\n\x1b[2m-- restored --\x1b[m\r\n";

/// Scrollback and screen as bytes that redraw them. Under a full-screen program, dumps the main screen beneath it.
pub fn dump(parser: &mut Parser) -> Vec<u8> {
    let alt = parser.screen().alternate_screen();
    if alt {
        parser.process(b"\x1b[?47l");
    }
    let out = rows(parser.screen_mut());
    if alt {
        parser.process(b"\x1b[?47h");
    }
    out
}

/// Replays a dump above whatever the new shell prints.
pub fn restore(parser: &mut Parser, out: &[u8]) {
    if !out.is_empty() {
        parser.process(out);
        parser.process(RESTORED);
    }
}

fn rows(screen: &mut Screen) -> Vec<u8> {
    let (height, _) = screen.size();
    let total = vt::history(screen);
    let len = total + height as usize;
    let mut out = Vec::new();
    let mut blank = 0;
    let mut at = 0;
    while at < len {
        let offset = total.saturating_sub(at);
        screen.set_scrollback(offset);
        for r in (at - (total - offset)) as u16..height {
            let line = row(screen, r);
            if !line.is_empty() {
                out.extend(b"\r\n".repeat(blank));
                out.extend(line);
                blank = 0;
            }
            if !screen.row_wrapped(r) {
                blank += 1;
            }
            at += 1;
        }
    }
    screen.set_scrollback(0);
    out
}

/// One row with its colors; wrapped rows keep trailing blanks so the next row continues them.
fn row(screen: &Screen, r: u16) -> Vec<u8> {
    let (_, cols) = screen.size();
    let cells: Vec<&Cell> = (0..cols).filter_map(|c| screen.cell(r, c)).collect();
    let end = if screen.row_wrapped(r) {
        cells.len()
    } else {
        cells.iter().rposition(|c| !c.contents().trim().is_empty() || c.bgcolor() != Color::Default || c.inverse()).map_or(0, |i| i + 1)
    };
    let mut out = String::new();
    let mut pen = Pen::default();
    for cell in &cells[..end] {
        if cell.is_wide_continuation() {
            continue;
        }
        let next = Pen::of(cell);
        if next != pen {
            out += &next.sgr();
            pen = next;
        }
        out += if cell.has_contents() { cell.contents() } else { " " };
    }
    if pen != Pen::default() {
        out += "\x1b[m";
    }
    out.into_bytes()
}

#[derive(Default, PartialEq)]
struct Pen {
    fg: Color,
    bg: Color,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
}

impl Pen {
    fn of(c: &Cell) -> Self {
        Self {
            fg: c.fgcolor(),
            bg: c.bgcolor(),
            bold: c.bold(),
            dim: c.dim(),
            italic: c.italic(),
            underline: c.underline(),
            inverse: c.inverse(),
        }
    }

    fn sgr(&self) -> String {
        let mut s = String::from("\x1b[0");
        for (on, code) in [(self.bold, 1), (self.dim, 2), (self.italic, 3), (self.underline, 4), (self.inverse, 7)] {
            if on {
                s += &format!(";{code}");
            }
        }
        for (color, base) in [(self.fg, 38), (self.bg, 48)] {
            match color {
                Color::Default => {}
                Color::Idx(i) => s += &format!(";{base};5;{i}"),
                Color::Rgb(r, g, b) => s += &format!(";{base};2;{r};{g};{b}"),
            }
        }
        s + "m"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser(rows: u16, cols: u16, bytes: &[u8]) -> Parser {
        let mut p = vt100::Parser::new_with_callbacks(rows, cols, vt::SCROLLBACK, vt::Callbacks::default());
        p.process(bytes);
        p
    }

    fn round_trip(rows: u16, cols: u16, bytes: &[u8]) -> Parser {
        let out = dump(&mut parser(rows, cols, bytes));
        parser(rows, cols, &out)
    }

    #[test]
    fn keeps_scrollback_and_text() {
        let mut p = round_trip(3, 10, b"1\r\n2\r\n\r\n4\r\n5\r\n6");
        assert_eq!(vt::text(p.screen_mut()), ["1", "2", "", "4", "5", "6"]);
    }

    #[test]
    fn keeps_colors() {
        let p = round_trip(3, 20, b"a\x1b[1;31mb\x1b[38;2;1;2;3;44mc\x1b[md");
        let s = p.screen();
        assert_eq!(s.contents(), "abcd");
        assert!(!s.cell(0, 0).unwrap().bold());
        assert!(s.cell(0, 1).unwrap().bold());
        assert_eq!(s.cell(0, 1).unwrap().fgcolor(), Color::Idx(1));
        assert_eq!(s.cell(0, 2).unwrap().fgcolor(), Color::Rgb(1, 2, 3));
        assert_eq!(s.cell(0, 2).unwrap().bgcolor(), Color::Idx(4));
        assert_eq!(s.cell(0, 3).unwrap().fgcolor(), Color::Default);
    }

    #[test]
    fn keeps_wrapping_and_wide_chars() {
        let p = round_trip(3, 4, "abcdef\r\n日本".as_bytes());
        assert!(p.screen().row_wrapped(0));
        assert_eq!(p.screen().contents(), "abcdef\n日本");
    }

    #[test]
    fn drops_trailing_blank_rows() {
        let out = dump(&mut parser(5, 10, b"$ ls\r\n$ "));
        assert_eq!(out, b"$ ls\r\n$");
    }

    #[test]
    fn dumps_main_screen_under_full_screen_programs() {
        let mut p = parser(3, 10, b"shell\x1b[?1049hvim");
        assert_eq!(dump(&mut p), b"shell");
        assert!(p.screen().alternate_screen());
        assert_eq!(p.screen().contents(), "vim");
    }

    #[test]
    fn restore_marks_old_output() {
        let mut p = parser(3, 20, b"");
        restore(&mut p, b"old");
        assert_eq!(vt::text(p.screen_mut())[..2], ["old", "-- restored --"]);
        restore(&mut p, b"");
        assert_eq!(p.screen().cursor_position(), (2, 0));
    }
}
