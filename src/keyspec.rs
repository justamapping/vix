use anyhow::{Result, bail};

use crate::input::Key;

/// Parses vim key notation (`gg`, `<C-d>`, `<CR>`, `<lt>`) into keys. A `<` that starts no known key is literal.
pub fn parse(s: &str) -> Result<Vec<Key>> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        if c == '<'
            && let Some(end) = rest.find('>')
            && let Some(key) = special(&rest[1..end])
        {
            out.push(key);
            rest = &rest[end + 1..];
            continue;
        }
        out.push(Key::Char(c));
        rest = &rest[c.len_utf8()..];
    }
    if out.is_empty() {
        bail!("empty key sequence");
    }
    Ok(out)
}

fn special(name: &str) -> Option<Key> {
    let lower = name.to_ascii_lowercase();
    let key = match lower.as_str() {
        "cr" | "enter" | "return" => Key::Enter,
        "esc" => Key::Esc,
        "bs" => Key::Backspace,
        "tab" => Key::Tab,
        "space" => Key::Char(' '),
        "lt" => Key::Char('<'),
        "bslash" => Key::Char('\\'),
        "bar" => Key::Char('|'),
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        _ => {
            let c = lower.strip_prefix("c-")?;
            let mut chars = c.chars();
            let c = match (chars.next()?, chars.next()) {
                (c, None) => c,
                _ if c == "space" => '@',
                _ => return None,
            };
            // the terminal sends these as other keys
            match c {
                'i' => Key::Tab,
                'm' => Key::Enter,
                '[' => Key::Esc,
                'h' => Key::Backspace,
                'a'..='z' | '@' | '\\' | ']' | '^' | '_' => Key::Ctrl(c),
                _ => return None,
            }
        }
    };
    Some(key)
}

/// The byte a terminal sends for a ctrl key.
pub fn ctrl_byte(key: Key) -> Option<u8> {
    match key {
        Key::Ctrl('@') => Some(0),
        Key::Ctrl(c @ 'a'..='z') => Some(c as u8 - b'a' + 1),
        Key::Ctrl(c @ ('\\' | ']' | '^' | '_')) => Some(c as u8 - b'\\' + 0x1c),
        _ => None,
    }
}

/// The ctrl key a terminal sends as byte `b`, the inverse of `ctrl_byte`.
pub fn ctrl_key(b: u8) -> Option<Key> {
    match b {
        0 => Some(Key::Ctrl('@')),
        1..=26 => Some(Key::Ctrl((b'a' + b - 1) as char)),
        0x1c..=0x1f => Some(Key::Ctrl((b'\\' + b - 0x1c) as char)),
        _ => None,
    }
}

/// Keys in vim notation, the inverse of `parse`.
pub fn notation(keys: &[Key]) -> String {
    keys.iter()
        .map(|&key| match key {
            Key::Char('<') => "<lt>".into(),
            Key::Char(' ') => "<Space>".into(),
            Key::Char(c) => c.to_string(),
            Key::Ctrl('@') => "<C-Space>".into(),
            Key::Ctrl(c) => format!("<C-{c}>"),
            Key::Enter => "<CR>".into(),
            Key::Esc => "<Esc>".into(),
            Key::Backspace => "<BS>".into(),
            Key::Tab => "<Tab>".into(),
            Key::Up => "<Up>".into(),
            Key::Down => "<Down>".into(),
            Key::Left => "<Left>".into(),
            Key::Right => "<Right>".into(),
        })
        .collect()
}

/// What a terminal sends for `keys`, arrows in normal cursor mode.
pub fn bytes(keys: &[Key]) -> Vec<u8> {
    let mut out = Vec::new();
    for &key in keys {
        match key {
            Key::Char(c) => out.extend(c.to_string().as_bytes()),
            Key::Ctrl(_) => out.extend(ctrl_byte(key)),
            Key::Enter => out.push(b'\r'),
            Key::Esc => out.push(0x1b),
            Key::Backspace => out.push(0x7f),
            Key::Tab => out.push(b'\t'),
            Key::Up => out.extend(b"\x1b[A"),
            Key::Down => out.extend(b"\x1b[B"),
            Key::Right => out.extend(b"\x1b[C"),
            Key::Left => out.extend(b"\x1b[D"),
        }
    }
    out
}

/// Parses the escape key, which must be a single ctrl key so it's one byte on the wire.
pub fn escape(s: &str) -> Result<u8> {
    match parse(s)?.as_slice() {
        [key] => ctrl_byte(*key).ok_or_else(|| anyhow::anyhow!("escape must be a ctrl key like <C-\\>, got {s}")),
        _ => bail!("escape must be a single key, got {s}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Key::*;

    #[test]
    fn notation_round_trips() {
        for s in ["gg", "<C-d>", "<CR>", "<lt>a", "<Space>", "<Tab><Esc><BS>", "<C-\\>", "<C-Space>", "<Up>"] {
            assert_eq!(notation(&parse(s).unwrap()), s);
        }
    }

    #[test]
    fn plain_and_special() {
        assert_eq!(parse("gg").unwrap(), vec![Char('g'), Char('g')]);
        assert_eq!(parse(":q<CR>").unwrap(), vec![Char(':'), Char('q'), Enter]);
        assert_eq!(parse("<C-d><c-U>").unwrap(), vec![Ctrl('d'), Ctrl('u')]);
        assert_eq!(parse("<lt><Space><Esc>").unwrap(), vec![Char('<'), Char(' '), Esc]);
        assert_eq!(parse("<C-\\><C-^>").unwrap(), vec![Ctrl('\\'), Ctrl('^')]);
    }

    #[test]
    fn unknown_brackets_are_literal() {
        assert_eq!(parse("<foo>").unwrap(), "<foo>".chars().map(Char).collect::<Vec<_>>());
        assert_eq!(parse("a<b").unwrap(), vec![Char('a'), Char('<'), Char('b')]);
        assert!(parse("").is_err());
    }

    #[test]
    fn aliases_match_input() {
        assert_eq!(parse("<C-i><C-m><C-[><C-h>").unwrap(), vec![Tab, Enter, Esc, Backspace]);
    }

    #[test]
    fn keys_to_bytes() {
        assert_eq!(bytes(&parse("npm test<CR>").unwrap()), b"npm test\r");
        assert_eq!(bytes(&parse("<C-c><Up><BS><Esc>é").unwrap()), "\x03\x1b[A\x7f\x1bé".as_bytes());
        assert_eq!(bytes(&parse("<C-\\>").unwrap()), b"\x1c");
    }

    #[test]
    fn escape_keys() {
        assert_eq!(escape("<C-\\>").unwrap(), 0x1c);
        for b in [0, 1, 0x1c, 0x1d, 0x1e, 0x1f] {
            assert_eq!(ctrl_key(b).and_then(ctrl_byte), Some(b));
        }
        assert_eq!(escape("<C-Space>").unwrap(), 0);
        assert_eq!(escape("<C-a>").unwrap(), 1);
        assert_eq!(escape("<C-]>").unwrap(), 0x1d);
        assert!(escape("<C-m>").is_err());
        assert!(escape("a").is_err());
        assert!(escape("<C-a><C-b>").is_err());
    }
}
