use vt100::{MouseProtocolEncoding, MouseProtocolMode};

pub type Parser = vt100::Parser<Callbacks>;

pub const SCROLLBACK: usize = 10_000;

/// Collects what vt100 doesn't handle itself: replies to terminal queries and the cursor shape.
#[derive(Default)]
pub struct Callbacks {
    pub replies: Vec<u8>,
    pub cursor_shape: u16,
}

impl vt100::Callbacks for Callbacks {
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
