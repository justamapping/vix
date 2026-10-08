use crate::input::Key;

#[derive(Debug, Clone, PartialEq)]
pub struct Map {
    pub lhs: Vec<Key>,
    pub rhs: Vec<Key>,
}

/// Runs typed keys through non-recursive mappings. Returns keys to run and keys waiting on a longer mapping.
/// `flush` (after the timeout) stops waiting and takes the longest mapping that matches, if any.
pub fn resolve(maps: &[Map], typed: &[Key], flush: bool) -> (Vec<Key>, Vec<Key>) {
    let mut out = Vec::new();
    let mut i = 0;
    while i < typed.len() {
        let rest = &typed[i..];
        if !flush && maps.iter().any(|m| m.lhs.len() > rest.len() && m.lhs.starts_with(rest)) {
            return (out, rest.to_vec());
        }
        match maps.iter().filter(|m| rest.starts_with(&m.lhs)).max_by_key(|m| m.lhs.len()) {
            Some(m) => {
                out.extend_from_slice(&m.rhs);
                i += m.lhs.len();
            }
            None => {
                out.push(rest[0]);
                i += 1;
            }
        }
    }
    (out, Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyspec::parse;

    fn maps(pairs: &[(&str, &str)]) -> Vec<Map> {
        pairs.iter().map(|(l, r)| Map { lhs: parse(l).unwrap(), rhs: parse(r).unwrap() }).collect()
    }

    fn run(maps: &[Map], typed: &str, flush: bool) -> (Vec<Key>, Vec<Key>) {
        resolve(maps, &parse(typed).unwrap(), flush)
    }

    fn keys(s: &str) -> Vec<Key> {
        if s.is_empty() { vec![] } else { parse(s).unwrap() }
    }

    #[test]
    fn single_key() {
        let m = maps(&[("q", "-"), ("<C-j>", "J")]);
        assert_eq!(run(&m, "q", false), (keys("-"), keys("")));
        assert_eq!(run(&m, "<C-j>", false), (keys("J"), keys("")));
        assert_eq!(run(&m, "jk", false), (keys("jk"), keys("")));
    }

    #[test]
    fn not_recursive() {
        let m = maps(&[("a", "b"), ("b", "c")]);
        assert_eq!(run(&m, "a", false), (keys("b"), keys("")));
    }

    #[test]
    fn prefix_waits_then_resolves() {
        let m = maps(&[("gx", ":q<CR>")]);
        assert_eq!(run(&m, "g", false), (keys(""), keys("g")));
        assert_eq!(run(&m, "gx", false), (keys(":q<CR>"), keys("")));
        assert_eq!(run(&m, "gj", false), (keys("gj"), keys("")));
        assert_eq!(run(&m, "gg", false), (keys("g"), keys("g")));
        assert_eq!(run(&m, "g", true), (keys("g"), keys("")));
    }

    #[test]
    fn longest_wins_on_flush() {
        let m = maps(&[("g", "G"), ("gx", "x")]);
        assert_eq!(run(&m, "g", false), (keys(""), keys("g")));
        assert_eq!(run(&m, "g", true), (keys("G"), keys("")));
        assert_eq!(run(&m, "gx", false), (keys("x"), keys("")));
        assert_eq!(run(&m, "gj", false), (keys("Gj"), keys("")));
    }
}
