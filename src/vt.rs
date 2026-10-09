use base64::Engine;
use vt100::{MouseProtocolEncoding, MouseProtocolMode, Screen};

pub type Parser = vt100::Parser<Callbacks>;

pub const SCROLLBACK: usize = 10_000;

/// Collects what vt100 doesn't handle itself: replies to terminal queries, the cursor shape, and bells.
#[derive(Default)]
pub struct Callbacks {
    pub replies: Vec<u8>,
    pub cursor_shape: u16,
    pub bell: bool,
}

impl vt100::Callbacks for Callbacks {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.bell = true;
    }

    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        _i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let first = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        match (i1, c, first) {
            // primary device attributes: VT100 with advanced video
            (None, 'c', 0) => self.replies.extend(b"\x1b[?1;2c"),
            (None, 'n', 5) => self.replies.extend(b"\x1b[0n"),
            (None, 'n', 6) => {
                let (row, col) = screen.cursor_position();
                self.replies.extend(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
            }
            (Some(b' '), 'q', shape) => self.cursor_shape = shape,
            _ => {}
        }
    }
}

pub fn parser(cols: u16, rows: u16) -> Parser {
    vt100::Parser::new_with_callbacks(rows, cols, SCROLLBACK, Callbacks::default())
}

/// Rows of scrollback above the screen.
pub fn history(screen: &mut Screen) -> usize {
    screen.set_scrollback(usize::MAX);
    let n = screen.scrollback();
    screen.set_scrollback(0);
    n
}

/// Scrollback then screen, one string per row.
pub fn text(screen: &mut Screen) -> Vec<String> {
    let (rows, cols) = screen.size();
    let total = history(screen);
    let len = total + rows as usize;
    let mut lines = Vec::with_capacity(len);
    while lines.len() < len {
        let offset = total.saturating_sub(lines.len());
        screen.set_scrollback(offset);
        let first = total - offset;
        let skip = lines.len() - first;
        lines.extend(screen.rows(0, cols).skip(skip));
    }
    screen.set_scrollback(0);
    lines
}

/// Char index into a row's text of column `col`; a wide char is one char over two columns.
pub fn index(screen: &Screen, row: u16, col: u16) -> usize {
    (0..col).filter(|&c| !screen.cell(row, c).is_some_and(|cell| cell.is_wide_continuation())).count()
}

/// Column of char `index` on `row`, or the row's width past its end.
pub fn column(screen: &Screen, row: u16, index: usize) -> u16 {
    let cols = screen.size().1;
    (0..cols).filter(|&c| !screen.cell(row, c).is_some_and(|cell| cell.is_wide_continuation())).nth(index).unwrap_or(cols)
}

/// Sets the outer terminal's clipboard.
pub fn osc52(text: &str) -> Vec<u8> {
    format!("\x1b]52;c;{}\x07", base64::engine::general_purpose::STANDARD.encode(text)).into_bytes()
}

/// Outer-terminal state a program expects while it has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Modes {
    pub app_keypad: bool,
    pub app_cursor: bool,
    pub bracketed_paste: bool,
    pub mouse: u16,
    pub mouse_encoding: u16,
    pub cursor_shape: u16,
}

pub fn modes(parser: &Parser) -> Modes {
    let s = parser.screen();
    Modes {
        app_keypad: s.application_keypad(),
        app_cursor: s.application_cursor(),
        bracketed_paste: s.bracketed_paste(),
        mouse: match s.mouse_protocol_mode() {
            MouseProtocolMode::None => 0,
            MouseProtocolMode::Press => 9,
            MouseProtocolMode::PressRelease => 1000,
            MouseProtocolMode::ButtonMotion => 1002,
            MouseProtocolMode::AnyMotion => 1003,
        },
        mouse_encoding: match s.mouse_protocol_encoding() {
            MouseProtocolEncoding::Default => 0,
            MouseProtocolEncoding::Utf8 => 1005,
            MouseProtocolEncoding::Sgr => 1006,
        },
        cursor_shape: parser.callbacks().cursor_shape,
    }
}

/// Escape codes that set every mode explicitly.
pub fn mode_bytes(m: Modes) -> Vec<u8> {
    let flag = |on: bool| if on { 'h' } else { 'l' };
    let mut s = String::new();
    s += if m.app_keypad { "\x1b=" } else { "\x1b>" };
    s += &format!("\x1b[?1{}\x1b[?2004{}", flag(m.app_cursor), flag(m.bracketed_paste));
    s += "\x1b[?9l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l";
    for mode in [m.mouse, m.mouse_encoding] {
        if mode != 0 {
            s += &format!("\x1b[?{mode}h");
        }
    }
    s += &format!("\x1b[{} q", m.cursor_shape);
    s.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(bytes: &[u8]) -> Parser {
        let mut p = parser(80, 24);
        p.process(bytes);
        p
    }

    #[test]
    fn text_includes_scrollback() {
        let mut p = vt100::Parser::new_with_callbacks(3, 10, SCROLLBACK, Callbacks::default());
        p.process(b"1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7");
        let s = p.screen_mut();
        assert_eq!(history(s), 4);
        assert_eq!(text(s), ["1", "2", "3", "4", "5", "6", "7"]);
        assert_eq!(s.scrollback(), 0);
    }

    #[test]
    fn text_without_scrollback() {
        let mut p = feed(b"hi");
        assert_eq!(text(p.screen_mut()).len(), 24);
    }

    #[test]
    fn wide_chars_index_once() {
        let p = feed("a日b".as_bytes());
        assert_eq!(index(p.screen(), 0, 3), 2);
        assert_eq!(column(p.screen(), 0, 2), 3);
        assert_eq!(column(p.screen(), 0, 100), 80);
    }

    #[test]
    fn osc52_encodes() {
        assert_eq!(osc52("hi"), b"\x1b]52;c;aGk=\x07");
    }

    #[test]
    fn answers_queries() {
        let p = feed(b"\x1b[c\x1b[5n\x1b[3;7H\x1b[6n");
        assert_eq!(p.callbacks().replies, b"\x1b[?1;2c\x1b[0n\x1b[3;7R");
    }

    #[test]
    fn tracks_modes() {
        let m = modes(&feed(b"\x1b[?1h\x1b[?2004h\x1b[?1002h\x1b[?1006h\x1b[6 q"));
        assert_eq!(
            m,
            Modes { app_keypad: false, app_cursor: true, bracketed_paste: true, mouse: 1002, mouse_encoding: 1006, cursor_shape: 6 }
        );
    }

    #[test]
    fn mode_bytes_enable_requested() {
        let b = String::from_utf8(mode_bytes(Modes { mouse: 1000, mouse_encoding: 1006, ..Default::default() })).unwrap();
        assert!(b.ends_with("\x1b[?1000h\x1b[?1006h\x1b[0 q"));
        assert!(b.contains("\x1b[?2004l"));
    }
}
