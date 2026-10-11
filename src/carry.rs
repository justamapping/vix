use serde::{Deserialize, Serialize};
use vt100::Screen;

use crate::{prompt, vt};

/// Where on a row `i a I A` put a line editor's cursor.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Spot {
    /// a char index into the row's text
    At(usize),
    Start,
    End,
}

/// Arrow keys that move a line editor's cursor to `spot` on screen row `row`, never past its input's ends. `None`
/// off the input, or in a full-screen program.
pub fn arrows(screen: &Screen, row: u16, spot: Spot) -> Option<Vec<u8>> {
    let input = prompt::input(screen)?;
    let to = match spot {
        Spot::At(index) => input.index(row, vt::column(screen, row, index))?,
        Spot::Start => input.index(row, 0).and(Some(0))?,
        Spot::End => input.index(row, 0).and(Some(input.len()))?,
    };
    Some(arrow(to < input.cursor, screen.application_cursor()).repeat(to.abs_diff(input.cursor)).into_bytes())
}

/// Left or right arrow, in application cursor mode or not.
pub fn arrow(left: bool, app: bool) -> &'static str {
    match (left, app) {
        (true, false) => "\x1b[D",
        (false, false) => "\x1b[C",
        (true, true) => "\x1bOD",
        (false, true) => "\x1bOC",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::tests::fixture;

    fn screen(cols: u16, bytes: &[u8]) -> vt::Parser {
        let mut p = vt::parser(cols, 5);
        p.process(bytes);
        p
    }

    fn left(n: usize) -> Option<Vec<u8>> {
        Some("\x1b[D".repeat(n).into_bytes())
    }

    fn right(n: usize) -> Option<Vec<u8>> {
        Some("\x1b[C".repeat(n).into_bytes())
    }

    #[test]
    fn moves_along_the_prompt_line() {
        let p = screen(40, b"out\r\n$ echo hello");
        assert_eq!(arrows(p.screen(), 1, Spot::At(2)), left(10));
        assert_eq!(arrows(p.screen(), 1, Spot::At(12)), Some(vec![]));
    }

    #[test]
    fn stops_at_the_inputs_ends() {
        let p = screen(40, b"out\r\n$ echo hello\x1b[3D");
        assert_eq!(arrows(p.screen(), 1, Spot::At(0)), left(7));
        assert_eq!(arrows(p.screen(), 1, Spot::At(30)), right(3));
        assert_eq!(arrows(p.screen(), 1, Spot::Start), left(7));
        assert_eq!(arrows(p.screen(), 1, Spot::End), right(3));
    }

    #[test]
    fn other_lines_and_full_screen_programs_stay_put() {
        let p = screen(40, b"out\r\n$ ls");
        assert_eq!(arrows(p.screen(), 0, Spot::At(0)), None);
        assert_eq!(arrows(p.screen(), 0, Spot::Start), None);
        let p = screen(40, b"\x1b[?1049h$ ls");
        assert_eq!(arrows(p.screen(), 0, Spot::At(0)), None);
    }

    #[test]
    fn follows_wrapped_rows() {
        let p = screen(10, b"$ abcdefghijkl");
        assert_eq!(p.screen().cursor_position(), (1, 4));
        assert_eq!(arrows(p.screen(), 0, Spot::At(2)), left(12));
    }

    #[test]
    fn wide_chars_are_one_press() {
        let p = screen(40, "$ 日本x".as_bytes());
        assert_eq!(arrows(p.screen(), 0, Spot::At(2)), left(3));
    }

    #[test]
    fn application_cursor_keys() {
        let p = screen(40, b"\x1b[?1h$ ab");
        assert_eq!(arrows(p.screen(), 0, Spot::At(3)), Some(b"\x1bOD".to_vec()));
    }

    // left on claude code's empty prompt opens its agent view
    #[test]
    fn claude_code_empty_prompt_gets_no_left() {
        let p = fixture("claude_empty", 14, 50);
        assert_eq!(arrows(p.screen(), 7, Spot::At(0)), Some(vec![]));
        assert_eq!(arrows(p.screen(), 7, Spot::Start), Some(vec![]));
        assert_eq!(arrows(p.screen(), 7, Spot::End), Some(vec![]));
    }

    #[test]
    fn claude_code_wrapped_rows_are_one_input() {
        let p = fixture("claude_wrap", 14, 50);
        assert_eq!(arrows(p.screen(), 8, Spot::Start), left(55));
        assert_eq!(arrows(p.screen(), 9, Spot::End), right(27));
        assert_eq!(arrows(p.screen(), 7, Spot::At(0)), left(55));
        assert_eq!(arrows(p.screen(), 11, Spot::Start), None);
    }
}
