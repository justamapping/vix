use crate::keymap::Motion;

/// (row, col) where col is a char index.
pub type Pos = (usize, usize);

#[derive(Debug, Clone, Copy, PartialEq)]
enum Class {
    Blank,
    Word,
    Punct,
}

fn class(c: char, big: bool) -> Class {
    if c.is_whitespace() {
        Class::Blank
    } else if big || c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

fn chars(lines: &[String], row: usize) -> Vec<char> {
    lines.get(row).map(|l| l.chars().collect()).unwrap_or_default()
}

pub fn first_non_blank(line: &str) -> usize {
    line.chars().position(|c| !c.is_whitespace()).unwrap_or(0)
}

/// Where `motion` lands. Columns may equal the line length; callers clamp for normal mode.
pub fn apply(lines: &[String], pos: Pos, motion: Motion, count: Option<usize>) -> Pos {
    let n = count.unwrap_or(1).max(1);
    let last = lines.len().saturating_sub(1);
    let (row, col) = pos;
    let line_len = |r: usize| lines.get(r).map_or(0, |l| l.chars().count());
    let fnb = |r: usize| (r, lines.get(r).map_or(0, |l| first_non_blank(l)));
    match motion {
        Motion::Left => (row, col.saturating_sub(n)),
        Motion::Right => (row, (col + n).min(line_len(row))),
        Motion::Up => (row.saturating_sub(n), col),
        Motion::Down => ((row + n).min(last), col),
        Motion::LineStart => (row, 0),
        Motion::FirstNonBlank => fnb(row),
        Motion::LineEnd => {
            let r = (row + n - 1).min(last);
            (r, line_len(r).saturating_sub(1))
        }
        Motion::FirstLine => fnb(count.map_or(0, |c| c.saturating_sub(1)).min(last)),
        Motion::LastLine => fnb(count.map_or(last, |c| c.saturating_sub(1)).min(last)),
        Motion::WordFwd | Motion::BigWordFwd => {
            let big = motion == Motion::BigWordFwd;
            (0..n).fold(pos, |p, _| word_fwd(lines, p, big))
        }
        Motion::WordBack | Motion::BigWordBack => {
            let big = motion == Motion::BigWordBack;
            (0..n).fold(pos, |p, _| word_back(lines, p, big))
        }
        Motion::WordEnd | Motion::BigWordEnd => {
            let big = motion == Motion::BigWordEnd;
            (0..n).fold(pos, |p, _| word_end(lines, p, big))
        }
    }
}

fn word_fwd(lines: &[String], (mut row, mut col): Pos, big: bool) -> Pos {
    let line = chars(lines, row);
    if let Some(&c) = line.get(col) {
        let k = class(c, big);
        while col < line.len() && class(line[col], big) == k && k != Class::Blank {
            col += 1;
        }
    }
    loop {
        let line = chars(lines, row);
        while col < line.len() && class(line[col], big) == Class::Blank {
            col += 1;
        }
        if col < line.len() {
            return (row, col);
        }
        if row + 1 >= lines.len() {
            return (row, line.len());
        }
        row += 1;
        col = 0;
        if lines[row].is_empty() {
            return (row, 0);
        }
    }
}

fn word_end(lines: &[String], (mut row, mut col): Pos, big: bool) -> Pos {
    col += 1;
    loop {
        let line = chars(lines, row);
        while col < line.len() && class(line[col], big) == Class::Blank {
            col += 1;
        }
        if col < line.len() {
            let k = class(line[col], big);
            while col + 1 < line.len() && class(line[col + 1], big) == k {
                col += 1;
            }
            return (row, col);
        }
        if row + 1 >= lines.len() {
            return (row, line.len().saturating_sub(1));
        }
        row += 1;
        col = 0;
    }
}

fn word_back(lines: &[String], (mut row, mut col): Pos, big: bool) -> Pos {
    loop {
        let line = chars(lines, row);
        if col == 0 || line.is_empty() {
            if row == 0 {
                return (0, 0);
            }
            row -= 1;
            let prev = chars(lines, row);
            if prev.is_empty() {
                return (row, 0);
            }
            col = prev.len();
            continue;
        }
        col = col.min(line.len());
        while col > 0 && class(line[col - 1], big) == Class::Blank {
            col -= 1;
        }
        if col == 0 {
            continue;
        }
        let k = class(line[col - 1], big);
        while col > 0 && class(line[col - 1], big) == k {
            col -= 1;
        }
        return (row, col);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Motion::*;

    fn lines(s: &[&str]) -> Vec<String> {
        s.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn vertical() {
        let l = lines(&["foo", "bar", "buzz"]);
        assert_eq!(apply(&l, (0, 0), Down, Some(2)), (2, 0));
        assert_eq!(apply(&l, (0, 0), Down, Some(9)), (2, 0));
        assert_eq!(apply(&l, (2, 1), Up, None), (1, 1));
        assert_eq!(apply(&l, (0, 0), LastLine, None), (2, 0));
        assert_eq!(apply(&l, (2, 0), FirstLine, None), (0, 0));
        assert_eq!(apply(&l, (0, 0), LastLine, Some(2)), (1, 0));
    }

    #[test]
    fn horizontal() {
        let l = lines(&["  foo bar"]);
        assert_eq!(apply(&l, (0, 5), LineStart, None), (0, 0));
        assert_eq!(apply(&l, (0, 5), FirstNonBlank, None), (0, 2));
        assert_eq!(apply(&l, (0, 0), LineEnd, None), (0, 8));
        assert_eq!(apply(&l, (0, 7), Right, Some(5)), (0, 9));
        assert_eq!(apply(&l, (0, 1), Left, Some(5)), (0, 0));
    }

    #[test]
    fn words() {
        let l = lines(&["foo.bar baz", "", "  qux"]);
        assert_eq!(apply(&l, (0, 0), WordFwd, None), (0, 3));
        assert_eq!(apply(&l, (0, 3), WordFwd, None), (0, 4));
        assert_eq!(apply(&l, (0, 0), BigWordFwd, None), (0, 8));
        assert_eq!(apply(&l, (0, 8), WordFwd, None), (1, 0));
        assert_eq!(apply(&l, (1, 0), WordFwd, None), (2, 2));
        assert_eq!(apply(&l, (2, 2), WordFwd, None), (2, 5));
        assert_eq!(apply(&l, (0, 0), WordEnd, None), (0, 2));
        assert_eq!(apply(&l, (0, 2), WordEnd, None), (0, 3));
        assert_eq!(apply(&l, (0, 0), BigWordEnd, None), (0, 6));
        assert_eq!(apply(&l, (0, 8), WordEnd, None), (0, 10));
        assert_eq!(apply(&l, (0, 10), WordEnd, None), (2, 4));
        assert_eq!(apply(&l, (2, 2), WordBack, None), (1, 0));
        assert_eq!(apply(&l, (1, 0), WordBack, None), (0, 8));
        assert_eq!(apply(&l, (0, 8), WordBack, None), (0, 4));
        assert_eq!(apply(&l, (0, 4), WordBack, None), (0, 3));
        assert_eq!(apply(&l, (0, 8), BigWordBack, None), (0, 0));
        assert_eq!(apply(&l, (0, 0), WordBack, None), (0, 0));
    }
}
