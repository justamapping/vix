use crate::keymap::{Object, TextObj};
use crate::motion::{self, Class, Pos};

/// What a text object covers: chars up to an exclusive end, or whole rows, last included.
#[derive(Debug, PartialEq)]
pub enum Span {
    Chars(Pos, Pos),
    Lines(usize, usize),
}

/// The span `obj` covers around `pos`, `n` times over; None where vim would beep.
pub fn select(lines: &[String], pos: Pos, obj: TextObj, n: usize) -> Option<Span> {
    let (row, col) = pos;
    let n = n.max(1);
    let TextObj { object, inner } = obj;
    let line: Vec<char> = lines.get(row)?.chars().collect();
    let chars = |(a, b)| Span::Chars((row, a), (row, b));
    match object {
        Object::Word | Object::BigWord => {
            let classes: Vec<Class> = line.iter().map(|&c| motion::class(c, object == Object::BigWord)).collect();
            pick(&classes, col, |c| *c == Class::Blank, inner, n).map(chars)
        }
        Object::Paragraph => {
            let blank: Vec<bool> = lines.iter().map(|l| l.trim().is_empty()).collect();
            pick(&blank, row, |b| *b, inner, n).map(|(a, b)| Span::Lines(a, b - 1))
        }
        Object::Quote(q) => quote(&line, col, q, inner).map(chars),
        Object::Bracket(open, close) => bracket(lines, pos, open, close, inner, n),
    }
}

/// Groups equal neighbours into (start, end) runs, end exclusive.
fn runs<T: PartialEq>(items: &[T]) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for i in 0..items.len() {
        match out.last_mut() {
            Some((s, e)) if items[*s] == items[i] => *e = i + 1,
            _ => out.push((i, i + 1)),
        }
    }
    out
}

/// `iw`/`ip`: `n` runs from the one at `at`. `aw`/`ap`: `n` non-blank runs plus the blank after them, else the one
/// before; from a blank run, it and the `n` non-blank runs after it.
fn pick<T: PartialEq>(items: &[T], at: usize, blank: impl Fn(&T) -> bool, inner: bool, n: usize) -> Option<(usize, usize)> {
    let runs = runs(items);
    let k = runs.iter().position(|&(s, e)| (s..e).contains(&at))?;
    let is_blank = |r: usize| blank(&items[runs[r].0]);
    let last = runs.len() - 1;
    let (mut first, mut end) = (k, k);
    if inner {
        end = (k + n - 1).min(last);
    } else {
        let mut words = usize::from(!is_blank(k));
        while words < n && end < last {
            end += 1;
            words += usize::from(!is_blank(end));
        }
        if !is_blank(k) {
            if end < last && is_blank(end + 1) {
                end += 1;
            } else if k > 0 && is_blank(k - 1) {
                first = k - 1;
            }
        }
    }
    Some((runs[first].0, runs[end].1))
}

/// Quotes pair up from the line's start, skipping escaped ones; the pair around `col`, else the next one.
fn quote(line: &[char], col: usize, q: char, inner: bool) -> Option<(usize, usize)> {
    let qs: Vec<usize> = (0..line.len()).filter(|&i| line[i] == q && (i == 0 || line[i - 1] != '\\')).collect();
    let (a, b) = qs.chunks_exact(2).map(|p| (p[0], p[1])).find(|&(_, b)| col <= b)?;
    if inner {
        return Some((a + 1, b));
    }
    let blank = |c: &&char| c.is_whitespace();
    let trailing = line[b + 1..].iter().take_while(blank).count();
    let leading = if trailing == 0 { line[..a].iter().rev().take_while(blank).count() } else { 0 };
    Some((a - leading, b + 1 + trailing))
}

/// `n` levels out from `pos`, across lines. An inner block whose brackets sit on their own lines is linewise.
fn bracket(lines: &[String], pos: Pos, open: char, close: char, inner: bool, n: usize) -> Option<Span> {
    let mut text = Vec::new();
    let mut starts = Vec::new();
    for l in lines {
        starts.push(text.len());
        text.extend(l.chars());
        text.push('\n');
    }
    let at = starts[pos.0] + pos.1.min(lines[pos.0].chars().count());
    let open_before = |end: usize| {
        let mut depth = 0;
        for i in (0..end).rev() {
            if text[i] == close {
                depth += 1;
            } else if text[i] == open {
                if depth == 0 {
                    return Some(i);
                }
                depth -= 1;
            }
        }
        None
    };
    let mut o = if text[at] == open { at } else { open_before(at)? };
    for _ in 1..n {
        o = open_before(o)?;
    }
    let mut depth = 0;
    let c = (o + 1..text.len()).find(|&i| {
        if text[i] == open {
            depth += 1;
        } else if text[i] == close {
            if depth == 0 {
                return true;
            }
            depth -= 1;
        }
        false
    })?;
    let to_pos = |i: usize| {
        let row = starts.partition_point(|&s| s <= i) - 1;
        (row, i - starts[row])
    };
    if !inner {
        return Some(Span::Chars(to_pos(o), to_pos(c + 1)));
    }
    let ((r0, _), (r1, c1)) = (to_pos(o), to_pos(c));
    let own_lines = text[o + 1] == '\n' && text[starts[r1]..c].iter().all(|c| c.is_whitespace());
    if own_lines && r0 + 1 < r1 {
        return Some(Span::Lines(r0 + 1, r1 - 1));
    }
    Some(Span::Chars(to_pos(o + 1), (r1, c1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &[&str]) -> Vec<String> {
        s.iter().map(|l| l.to_string()).collect()
    }

    fn sel(l: &[String], pos: Pos, object: Object, inner: bool, n: usize) -> Option<Span> {
        select(l, pos, TextObj { object, inner }, n)
    }

    fn cut(l: &[String], span: Option<Span>) -> String {
        match span {
            Some(Span::Chars((r0, c0), (r1, c1))) => {
                let text = l.join("\n");
                let flat = |r: usize, c: usize| l[..r].iter().map(|x| x.chars().count() + 1).sum::<usize>() + c;
                text.chars().skip(flat(r0, c0)).take(flat(r1, c1) - flat(r0, c0)).collect()
            }
            Some(Span::Lines(a, b)) => format!("lines {a}-{b}"),
            None => "none".into(),
        }
    }

    #[test]
    fn words() {
        let l = lines(&["foo.bar  baz qux", ""]);
        let w = |col, inner, n| cut(&l, sel(&l, (0, col), Object::Word, inner, n));
        assert_eq!(w(1, true, 1), "foo");
        assert_eq!(w(5, true, 1), "bar");
        assert_eq!(w(7, true, 1), "  ");
        assert_eq!(w(9, true, 3), "baz qux");
        assert_eq!(w(9, false, 1), "baz ");
        assert_eq!(w(13, false, 1), " qux");
        assert_eq!(w(3, false, 1), ".");
        assert_eq!(w(4, false, 1), "bar  ");
        assert_eq!(w(7, false, 1), "  baz");
        assert_eq!(w(9, false, 2), "  baz qux");
        assert_eq!(cut(&l, sel(&l, (0, 1), Object::BigWord, false, 1)), "foo.bar  ");
        assert_eq!(cut(&l, sel(&l, (1, 0), Object::Word, true, 1)), "none");
    }

    #[test]
    fn paragraphs() {
        let l = lines(&["a", "b", "", "  ", "c", "", "d"]);
        let p = |row, inner, n| cut(&l, sel(&l, (row, 0), Object::Paragraph, inner, n));
        assert_eq!(p(1, true, 1), "lines 0-1");
        assert_eq!(p(0, false, 1), "lines 0-3");
        assert_eq!(p(3, true, 1), "lines 2-3");
        assert_eq!(p(3, false, 1), "lines 2-4");
        assert_eq!(p(6, false, 1), "lines 5-6");
        assert_eq!(p(0, true, 3), "lines 0-4");
    }

    #[test]
    fn quotes() {
        let l = lines(&[r#"say "hi \"x\"" and "yo"  ok"#]);
        let q = |col, inner| cut(&l, sel(&l, (0, col), Object::Quote('"'), inner, 1));
        assert_eq!(q(5, true), r#"hi \"x\""#);
        assert_eq!(q(0, true), r#"hi \"x\""#);
        assert_eq!(q(16, true), "yo");
        assert_eq!(q(20, false), "\"yo\"  ");
        assert_eq!(q(4, false), "\"hi \\\"x\\\"\" ");
        let l = lines(&["x=''"]);
        assert_eq!(cut(&l, sel(&l, (0, 0), Object::Quote('\''), true, 1)), "");
        assert_eq!(cut(&l, sel(&l, (0, 3), Object::Quote('\''), false, 1)), "''");
        assert_eq!(cut(&l, sel(&l, (0, 0), Object::Quote('"'), true, 1)), "none");
    }

    #[test]
    fn brackets() {
        let l = lines(&["f(a, g(b), c)", "x"]);
        let b = |col, inner, n| cut(&l, sel(&l, (0, col), Object::Bracket('(', ')'), inner, n));
        assert_eq!(b(7, true, 1), "b");
        assert_eq!(b(7, false, 1), "(b)");
        assert_eq!(b(6, true, 1), "b");
        assert_eq!(b(8, true, 1), "b");
        assert_eq!(b(3, true, 1), "a, g(b), c");
        assert_eq!(b(7, true, 2), "a, g(b), c");
        assert_eq!(b(7, true, 3), "none");
        assert_eq!(b(0, true, 1), "none");
        assert_eq!(cut(&l, sel(&l, (1, 0), Object::Bracket('(', ')'), true, 1)), "none");
    }

    #[test]
    fn brackets_across_lines() {
        let l = lines(&["fn x() {", "    a", "    b", "}", "{ c", "d }"]);
        let b = |row, col, inner| cut(&l, sel(&l, (row, col), Object::Bracket('{', '}'), inner, 1));
        assert_eq!(b(1, 4, true), "lines 1-2");
        assert_eq!(b(2, 0, false), "{\n    a\n    b\n}");
        assert_eq!(b(5, 0, true), " c\nd ");
        assert_eq!(b(0, 2, true), "none");
    }
}
