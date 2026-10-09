use vt100::Screen;

use crate::vt;

/// Arrow keys that move a line editor's cursor to char `index` of screen row `row`, if that's on the line the cursor
/// is on, wrapped rows included. `None` anywhere else, or in a full-screen program.
pub fn arrows(screen: &Screen, row: u16, index: usize) -> Option<Vec<u8>> {
    if screen.alternate_screen() {
        return None;
    }
    let (crow, ccol) = screen.cursor_position();
    let (first, last) = line(screen, crow);
    if !(first..=last).contains(&row) {
        return None;
    }
    let cols = screen.size().1 as usize;
    let at = |r: u16, c: u16| r as usize * cols + c as usize;
    let (from, to) = (at(crow, ccol), at(row, vt::column(screen, row, index)));
    let narrow = |i: usize| !screen.cell((i / cols) as u16, (i % cols) as u16).is_some_and(|c| c.is_wide_continuation());
    let chars = (from.min(to)..from.max(to)).filter(|&i| narrow(i)).count();
    let arrow = match (to < from, screen.application_cursor()) {
        (true, false) => "\x1b[D",
        (false, false) => "\x1b[C",
        (true, true) => "\x1bOD",
        (false, true) => "\x1bOC",
    };
    Some(arrow.repeat(chars).into_bytes())
}

/// First and last rows of the line through `row`, following wraps.
fn line(screen: &Screen, row: u16) -> (u16, u16) {
    let (mut first, mut last) = (row, row);
    while first > 0 && screen.row_wrapped(first - 1) {
        first -= 1;
    }
    while last + 1 < screen.size().0 && screen.row_wrapped(last) {
        last += 1;
    }
    (first, last)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(cols: u16, bytes: &[u8]) -> vt::Parser {
        let mut p = vt::parser(cols, 5);
        p.process(bytes);
        p
    }

    fn left(n: usize) -> Option<Vec<u8>> {
        Some("\x1b[D".repeat(n).into_bytes())
    }

    #[test]
    fn moves_along_the_prompt_line() {
        let p = screen(40, b"out\r\n$ echo hello");
        assert_eq!(arrows(p.screen(), 1, 2), left(10));
        assert_eq!(arrows(p.screen(), 1, 12), Some(vec![]));
        assert_eq!(arrows(p.screen(), 1, 30), Some("\x1b[C".repeat(18).into_bytes()));
    }

    #[test]
    fn other_lines_and_full_screen_programs_stay_put() {
        let p = screen(40, b"out\r\n$ ls");
        assert_eq!(arrows(p.screen(), 0, 0), None);
        let p = screen(40, b"\x1b[?1049h$ ls");
        assert_eq!(arrows(p.screen(), 0, 0), None);
    }

    #[test]
    fn follows_wrapped_rows() {
        let p = screen(10, b"$ abcdefghijkl");
        assert_eq!(p.screen().cursor_position(), (1, 4));
        assert_eq!(arrows(p.screen(), 0, 2), left(12));
    }

    #[test]
    fn wide_chars_are_one_press() {
        let p = screen(40, "$ 日本x".as_bytes());
        assert_eq!(arrows(p.screen(), 0, 2), left(3));
    }

    #[test]
    fn application_cursor_keys() {
        let p = screen(40, b"\x1b[?1h$ ab");
        assert_eq!(arrows(p.screen(), 0, 3), Some(b"\x1bOD".to_vec()));
    }
}
