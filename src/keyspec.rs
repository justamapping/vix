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
    fn escape_keys() {
        assert_eq!(escape("<C-\\>").unwrap(), 0x1c);
        assert_eq!(escape("<C-Space>").unwrap(), 0);
        assert_eq!(escape("<C-a>").unwrap(), 1);
        assert_eq!(escape("<C-]>").unwrap(), 0x1d);
        assert!(escape("<C-m>").is_err());
        assert!(escape("a").is_err());
        assert!(escape("<C-a><C-b>").is_err());
    }
}
