pub const CTRL_BACKSLASH: u8 = 0x1c;

#[derive(Debug, PartialEq)]
pub enum Input {
    Bytes(Vec<u8>),
    Escape,
    Mouse(Mouse),
}

/// An SGR (`CSI < b;x;y M/m`) mouse event; x and y are 1-based.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mouse {
    pub button: u16,
    pub x: u16,
    pub y: u16,
    pub release: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MouseKind {
    WheelUp,
    WheelDown,
    Press,
    Drag,
    Other,
}

impl Mouse {
    pub fn kind(&self) -> MouseKind {
        if self.release {
            return MouseKind::Other;
        }
        // without the shift/meta/ctrl bits
        match self.button & !0b11100 {
            64 => MouseKind::WheelUp,
            65 => MouseKind::WheelDown,
            0 => MouseKind::Press,
            32 => MouseKind::Drag,
            _ => MouseKind::Other,
        }
    }

    /// 0-based screen cell.
    pub fn cell(&self) -> (u16, u16) {
        (self.x.saturating_sub(1), self.y.saturating_sub(1))
    }

    pub fn encode(&self) -> Vec<u8> {
        format!("\x1b[<{};{};{}{}", self.button, self.x, self.y, if self.release { 'm' } else { 'M' }).into_bytes()
    }
}

/// Matches an SGR mouse event at the start of `bytes`. Returns its length.
fn sgr_mouse(bytes: &[u8]) -> Option<(usize, Mouse)> {
    let body = bytes.strip_prefix(b"\x1b[<")?;
    let end = body.iter().position(|b| !(b.is_ascii_digit() || *b == b';'))?;
    let release = match body[end] {
        b'M' => false,
        b'm' => true,
        _ => return None,
    };
    let params = std::str::from_utf8(&body[..end]).ok()?;
    let mut nums = params.split(';').map(|n| n.parse::<u16>().ok());
    let (button, x, y) = (nums.next()??, nums.next()??, nums.next()??);
    if nums.next().is_some() {
        return None;
    }
    Some((3 + end + 1, Mouse { button, x, y, release }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Enter,
    Esc,
    Backspace,
    Tab,
    Up,
    Down,
    Left,
    Right,
}

/// Splits a stdin chunk on the escape key's byte, raw or kitty-encoded (`CSI 92;5u` for `<C-\>`), and on SGR mouse events.
pub fn split(bytes: &[u8], esc: u8) -> Vec<Input> {
    let mut out = Vec::new();
    let mut pending = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let hit = if bytes[i] == esc {
            Some((1, Some(Input::Escape)))
        } else if let Some((len, m)) = sgr_mouse(&bytes[i..]) {
            Some((len, Some(Input::Mouse(m))))
        } else {
            kitty_ctrl(&bytes[i..], esc).map(|(len, press)| (len, press.then_some(Input::Escape)))
        };
        match hit {
            Some((len, input)) => {
                if !pending.is_empty() {
                    out.push(Input::Bytes(std::mem::take(&mut pending)));
                }
                out.extend(input);
                i += len;
            }
            None => {
                pending.push(bytes[i]);
                i += 1;
            }
        }
    }
    if !pending.is_empty() {
        out.push(Input::Bytes(pending));
    }
    out
}

/// Whether kitty codepoint `key` with ctrl held is the key that sends legacy byte `esc`.
fn is_ctrl_of(key: u32, esc: u8) -> bool {
    match esc {
        0 => key == 32 || key == 64,
        1..=26 => key == 0x60 + esc as u32,
        0x1c..=0x1f => key == 0x40 + esc as u32,
        _ => false,
    }
}

/// Matches `CSI key[;mods[:event]] u` for the escape key with ctrl as the only modifier. Returns (len, is_press).
fn kitty_ctrl(bytes: &[u8], esc: u8) -> Option<(usize, bool)> {
    let body = bytes.strip_prefix(b"\x1b[")?;
    let end = body.iter().position(|b| !(b.is_ascii_digit() || *b == b';' || *b == b':'))?;
    if body[end] != b'u' {
        return None;
    }
    let params = std::str::from_utf8(&body[..end]).ok()?;
    let mut fields = params.split(';');
    let key: u32 = fields.next()?.split(':').next()?.parse().ok()?;
    let mut mods = fields.next().unwrap_or("1").split(':');
    let modifiers: u32 = mods.next()?.parse().ok()?;
    let event: u32 = mods.next().unwrap_or("1").parse().ok()?;
    // ignore caps/num lock bits
    let ctrl_only = modifiers.checked_sub(1)? & 0b11_1111 == 4;
    (is_ctrl_of(key, esc) && ctrl_only).then_some((2 + end + 1, event != 3))
}

#[cfg(test)]
pub fn keys(bytes: &[u8]) -> Vec<Key> {
    spans(bytes).into_iter().map(|(k, _)| k).collect()
}

/// Decodes legacy terminal input into keys, each with the byte offset where it ends. A lone ESC in a chunk is `<Esc>`.
pub fn spans(bytes: &[u8]) -> Vec<(Key, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        i += 1;
        let key = match b {
            0x1b => match bytes.get(i) {
                Some(b'[' | b'O') => {
                    let start = i + 1;
                    let end = bytes[start..]
                        .iter()
                        .position(|b| (0x40..=0x7e).contains(b))
                        .map_or(bytes.len(), |p| start + p);
                    i = (end + 1).min(bytes.len());
                    match bytes.get(end) {
                        Some(b'A') => Key::Up,
                        Some(b'B') => Key::Down,
                        Some(b'C') => Key::Right,
                        Some(b'D') => Key::Left,
                        _ => continue,
                    }
                }
                _ => Key::Esc,
            },
            b'\r' => Key::Enter,
            b'\t' => Key::Tab,
            0x7f | 0x08 => Key::Backspace,
            0x00 => Key::Ctrl('@'),
            0x01..=0x1a => Key::Ctrl((b'a' + b - 1) as char),
            0x1c..=0x1f => Key::Ctrl((b'\\' + b - 0x1c) as char),
            _ => {
                let len = match b {
                    0xc0..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xf7 => 4,
                    _ => 1,
                };
                let end = (i - 1 + len).min(bytes.len());
                let s = String::from_utf8_lossy(&bytes[i - 1..end]);
                i = end;
                match s.chars().next() {
                    Some(c) => Key::Char(c),
                    None => continue,
                }
            }
        };
        out.push((key, i));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(b: &[u8]) -> Input {
        Input::Bytes(b.to_vec())
    }

    #[test]
    fn plain_bytes_pass_through() {
        assert_eq!(split(b"ls\r", CTRL_BACKSLASH), vec![bytes(b"ls\r")]);
        assert_eq!(split(b"\x1b[A", CTRL_BACKSLASH), vec![bytes(b"\x1b[A")]);
    }

    #[test]
    fn raw_escape() {
        assert_eq!(split(b"\x1c", CTRL_BACKSLASH), vec![Input::Escape]);
        assert_eq!(split(b"\x1c\x1c", CTRL_BACKSLASH), vec![Input::Escape, Input::Escape]);
        assert_eq!(split(b"ab\x1ccd", CTRL_BACKSLASH), vec![bytes(b"ab"), Input::Escape, bytes(b"cd")]);
    }

    #[test]
    fn kitty_escape() {
        assert_eq!(split(b"\x1b[92;5u", CTRL_BACKSLASH), vec![Input::Escape]);
        assert_eq!(split(b"\x1b[92;5:1u", CTRL_BACKSLASH), vec![Input::Escape]);
        assert_eq!(split(b"\x1b[92;5:2u", CTRL_BACKSLASH), vec![Input::Escape]);
        assert_eq!(split(b"\x1b[92;69u", CTRL_BACKSLASH), vec![Input::Escape]);
        assert_eq!(split(b"x\x1b[92;5ui", CTRL_BACKSLASH), vec![bytes(b"x"), Input::Escape, bytes(b"i")]);
    }

    #[test]
    fn other_escape_keys() {
        assert_eq!(split(b"a\x00b\x1c", 0), vec![bytes(b"a"), Input::Escape, bytes(b"b\x1c")]);
        assert_eq!(split(b"\x1b[32;5u\x1b[97;5u", 0), vec![Input::Escape, bytes(b"\x1b[97;5u")]);
        assert_eq!(split(b"\x1b[97;5u", 1), vec![Input::Escape]);
    }

    #[test]
    fn sgr_mouse_events() {
        let m = |button, x, y, release| Input::Mouse(Mouse { button, x, y, release });
        assert_eq!(split(b"\x1b[<64;10;5M", CTRL_BACKSLASH), vec![m(64, 10, 5, false)]);
        assert_eq!(split(b"a\x1b[<0;1;2mb", CTRL_BACKSLASH), vec![bytes(b"a"), m(0, 1, 2, true), bytes(b"b")]);
        assert_eq!(split(b"\x1b[<0;1M", CTRL_BACKSLASH), vec![bytes(b"\x1b[<0;1M")]);
        let kind = |button, release| Mouse { button, x: 1, y: 1, release }.kind();
        assert_eq!(kind(64, false), MouseKind::WheelUp);
        assert_eq!(kind(65 | 16, false), MouseKind::WheelDown);
        assert_eq!(kind(32, false), MouseKind::Drag);
        assert_eq!(kind(0, true), MouseKind::Other);
        assert_eq!(kind(2, false), MouseKind::Other);
        assert_eq!(Mouse { button: 65, x: 3, y: 4, release: true }.encode(), b"\x1b[<65;3;4m");
    }

    #[test]
    fn kitty_release_is_swallowed() {
        assert_eq!(split(b"\x1b[92;5:3u", CTRL_BACKSLASH), vec![]);
    }

    #[test]
    fn kitty_other_keys_pass_through() {
        assert_eq!(split(b"\x1b[92u", CTRL_BACKSLASH), vec![bytes(b"\x1b[92u")]);
        assert_eq!(split(b"\x1b[92;7u", CTRL_BACKSLASH), vec![bytes(b"\x1b[92;7u")]);
        assert_eq!(split(b"\x1b[97;5u", CTRL_BACKSLASH), vec![bytes(b"\x1b[97;5u")]);
    }

    #[test]
    fn decodes_keys() {
        use Key::*;
        assert_eq!(keys(b"jj\r"), vec![Char('j'), Char('j'), Enter]);
        assert_eq!(keys(b"\x1b"), vec![Esc]);
        assert_eq!(keys(b"\x1b[A\x1bOB"), vec![Up, Down]);
        assert_eq!(keys(b"\x7f\x12\x1c"), vec![Backspace, Ctrl('r'), Ctrl('\\')]);
        assert_eq!(keys("é".as_bytes()), vec![Char('é')]);
        assert_eq!(keys(b"\x1b[200~x"), vec![Char('x')]);
        assert_eq!(spans(b"i\x1b[Ax"), vec![(Char('i'), 1), (Up, 4), (Char('x'), 5)]);
    }
}
