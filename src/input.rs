pub const CTRL_BACKSLASH: u8 = 0x1c;

#[derive(Debug, PartialEq)]
pub enum Input {
    Bytes(Vec<u8>),
    Escape,
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

/// Splits a stdin chunk on `<C-\>`, raw or kitty-encoded (`CSI 92;5u`).
pub fn split(bytes: &[u8]) -> Vec<Input> {
    let mut out = Vec::new();
    let mut pending = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let hit = if bytes[i] == CTRL_BACKSLASH {
            Some((1, true))
        } else {
            kitty_ctrl_backslash(&bytes[i..])
        };
        match hit {
            Some((len, press)) => {
                if !pending.is_empty() {
                    out.push(Input::Bytes(std::mem::take(&mut pending)));
                }
                if press {
                    out.push(Input::Escape);
                }
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

/// Matches `CSI 92[;mods[:event]] u` with ctrl as the only modifier. Returns (len, is_press).
fn kitty_ctrl_backslash(bytes: &[u8]) -> Option<(usize, bool)> {
    let body = bytes.strip_prefix(b"\x1b[")?;
    let end = body.iter().position(|b| !(b.is_ascii_digit() || *b == b';' || *b == b':'))?;
    if body[end] != b'u' {
        return None;
    }
    let params = std::str::from_utf8(&body[..end]).ok()?;
    let mut fields = params.split(';');
    let key = fields.next()?.split(':').next()?;
    let mut mods = fields.next().unwrap_or("1").split(':');
    let modifiers: u32 = mods.next()?.parse().ok()?;
    let event: u32 = mods.next().unwrap_or("1").parse().ok()?;
    // ignore caps/num lock bits
    let ctrl_only = modifiers.checked_sub(1)? & 0b11_1111 == 4;
    (key == "92" && ctrl_only).then_some((2 + end + 1, event != 3))
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
        assert_eq!(split(b"ls\r"), vec![bytes(b"ls\r")]);
        assert_eq!(split(b"\x1b[A"), vec![bytes(b"\x1b[A")]);
    }

    #[test]
    fn raw_escape() {
        assert_eq!(split(b"\x1c"), vec![Input::Escape]);
        assert_eq!(split(b"\x1c\x1c"), vec![Input::Escape, Input::Escape]);
        assert_eq!(split(b"ab\x1ccd"), vec![bytes(b"ab"), Input::Escape, bytes(b"cd")]);
    }

    #[test]
    fn kitty_escape() {
        assert_eq!(split(b"\x1b[92;5u"), vec![Input::Escape]);
        assert_eq!(split(b"\x1b[92;5:1u"), vec![Input::Escape]);
        assert_eq!(split(b"\x1b[92;5:2u"), vec![Input::Escape]);
        assert_eq!(split(b"\x1b[92;69u"), vec![Input::Escape]);
        assert_eq!(split(b"x\x1b[92;5ui"), vec![bytes(b"x"), Input::Escape, bytes(b"i")]);
    }

    #[test]
    fn kitty_release_is_swallowed() {
        assert_eq!(split(b"\x1b[92;5:3u"), vec![]);
    }

    #[test]
    fn kitty_other_keys_pass_through() {
        assert_eq!(split(b"\x1b[92u"), vec![bytes(b"\x1b[92u")]);
        assert_eq!(split(b"\x1b[92;7u"), vec![bytes(b"\x1b[92;7u")]);
        assert_eq!(split(b"\x1b[97;5u"), vec![bytes(b"\x1b[97;5u")]);
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
