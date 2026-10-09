use std::cmp::Reverse;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::input::Key;

/// One terminal as the picker sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: u64,
    /// 1-based list position, the number `1`-`9` jumps to
    pub index: usize,
    pub name: String,
    pub cwd: String,
}

impl Item {
    /// What the query matches against: the name, then the directory.
    fn haystack(&self) -> String {
        format!("{} {}", self.name, self.cwd)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    pub item: usize,
    /// sorted char indices into the haystack
    pub positions: Vec<u32>,
}

impl Match {
    /// Whether char `i` of the name is matched.
    pub fn in_name(&self, i: usize) -> bool {
        self.positions.binary_search(&(i as u32)).is_ok()
    }

    /// Whether char `i` of the cwd is matched.
    pub fn in_cwd(&self, item: &Item, i: usize) -> bool {
        self.in_name(item.name.chars().count() + 1 + i)
    }
}

#[derive(Debug, PartialEq)]
pub enum Pick {
    Open(u64),
    Close,
}

/// Telescope-like fuzzy finder over terminals.
pub struct Picker {
    pub items: Vec<Item>,
    pub query: String,
    pub matches: Vec<Match>,
    pub selected: usize,
    /// the selection was moved since the query last changed, so it's what to preview
    pub browsing: bool,
    matcher: Matcher,
}

impl Picker {
    pub fn new(items: Vec<Item>) -> Self {
        let mut p = Self { items, query: String::new(), matches: Vec::new(), selected: 0, browsing: false, matcher: Matcher::new(Config::DEFAULT) };
        p.rank();
        p
    }

    pub fn selection(&self) -> Option<u64> {
        self.matches.get(self.selected).map(|m| self.items[m.item].id)
    }

    pub fn key(&mut self, key: Key) -> Option<Pick> {
        match key {
            Key::Esc | Key::Ctrl('c') => return Some(Pick::Close),
            Key::Enter => return Some(self.selection().map_or(Pick::Close, Pick::Open)),
            Key::Down | Key::Tab | Key::Ctrl('n' | 'j') => {
                self.selected = (self.selected + 1).min(self.matches.len().saturating_sub(1));
                self.browsing = true;
            }
            Key::Up | Key::Ctrl('p' | 'k') => {
                self.selected = self.selected.saturating_sub(1);
                self.browsing = true;
            }
            Key::Backspace | Key::Ctrl('h') => self.edit(|q| _ = q.pop()),
            Key::Ctrl('u') => self.edit(String::clear),
            Key::Ctrl('w') => self.edit(|q| {
                let keep = q.trim_end().rfind(' ').map_or(0, |i| i + 1);
                q.truncate(keep);
            }),
            Key::Char(c) => self.edit(|q| q.push(c)),
            _ => {}
        }
        None
    }

    /// Drops a terminal that went away while picking.
    pub fn remove(&mut self, id: u64) {
        let selected = self.selection();
        self.items.retain(|i| i.id != id);
        self.rank();
        self.selected = selected.and_then(|s| self.matches.iter().position(|m| self.items[m.item].id == s)).unwrap_or(0);
    }

    fn edit(&mut self, f: impl FnOnce(&mut String)) {
        f(&mut self.query);
        self.rank();
        self.selected = 0;
        self.browsing = false;
    }

    /// Best score first; ties keep the items' order, so an empty query lists them as given.
    fn rank(&mut self) {
        let pattern = Pattern::parse(&self.query, CaseMatching::Smart, Normalization::Smart);
        let mut buf = Vec::new();
        let mut scored: Vec<(u32, Match)> = Vec::new();
        for (i, item) in self.items.iter().enumerate() {
            let haystack = item.haystack();
            let mut positions = Vec::new();
            if let Some(score) = pattern.indices(Utf32Str::new(&haystack, &mut buf), &mut self.matcher, &mut positions) {
                positions.sort_unstable();
                positions.dedup();
                scored.push((score, Match { item: i, positions }));
            }
        }
        scored.sort_by_key(|(score, _)| Reverse(*score));
        self.matches = scored.into_iter().map(|(_, m)| m).collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picker(items: &[(&str, &str)]) -> Picker {
        let items = items
            .iter()
            .enumerate()
            .map(|(i, (name, cwd))| Item { id: i as u64 * 10, index: i + 1, name: name.to_string(), cwd: cwd.to_string() })
            .collect();
        Picker::new(items)
    }

    fn typed(p: &mut Picker, s: &str) {
        for c in s.chars() {
            assert_eq!(p.key(Key::Char(c)), None);
        }
    }

    fn names(p: &Picker) -> Vec<&str> {
        p.matches.iter().map(|m| p.items[m.item].name.as_str()).collect()
    }

    const TERMS: &[(&str, &str)] = &[("web", "~/code/site"), ("Untitled", "~/code/api"), ("api", "~/code/api"), ("logs", "/var/log")];

    #[test]
    fn empty_query_keeps_order() {
        let p = picker(TERMS);
        assert_eq!(names(&p), ["web", "Untitled", "api", "logs"]);
        assert_eq!(p.selection(), Some(0));
    }

    #[test]
    fn matches_names_and_cwd() {
        let mut p = picker(TERMS);
        typed(&mut p, "api");
        assert_eq!(names(&p), ["api", "Untitled"]);
        let m = &p.matches[0];
        assert!(m.in_name(0) && m.in_name(2));

        typed(&mut p, " log");
        assert!(names(&p).is_empty());
        assert_eq!(p.key(Key::Enter), Some(Pick::Close));
    }

    #[test]
    fn cwd_positions_skip_the_name() {
        let mut p = picker(TERMS);
        typed(&mut p, "var");
        assert_eq!(names(&p), ["logs"]);
        let (m, item) = (&p.matches[0], &p.items[3]);
        assert!(m.in_cwd(item, 1) && m.in_cwd(item, 3) && !m.in_name(0));
    }

    #[test]
    fn smart_case() {
        let mut p = picker(TERMS);
        typed(&mut p, "U");
        assert_eq!(names(&p), ["Untitled"]);
    }

    #[test]
    fn moves_and_opens() {
        let mut p = picker(TERMS);
        p.key(Key::Ctrl('n'));
        p.key(Key::Down);
        assert_eq!(p.key(Key::Enter), Some(Pick::Open(20)));
        for _ in 0..9 {
            p.key(Key::Ctrl('n'));
        }
        assert_eq!(p.selection(), Some(30));
        p.key(Key::Ctrl('p'));
        assert_eq!(p.selection(), Some(20));
        assert!(p.browsing);
        typed(&mut p, "w");
        assert_eq!(p.selected, 0);
        assert!(!p.browsing);
        assert_eq!(p.key(Key::Esc), Some(Pick::Close));
    }

    #[test]
    fn edits_the_query() {
        let mut p = picker(TERMS);
        typed(&mut p, "code ap");
        p.key(Key::Backspace);
        assert_eq!(p.query, "code a");
        p.key(Key::Ctrl('w'));
        assert_eq!(p.query, "code ");
        p.key(Key::Ctrl('u'));
        assert_eq!(p.query, "");
        assert_eq!(p.matches.len(), 4);
    }

    #[test]
    fn remove_keeps_the_selection() {
        let mut p = picker(TERMS);
        p.key(Key::Ctrl('n'));
        p.key(Key::Ctrl('n'));
        p.remove(0);
        assert_eq!(p.selection(), Some(20));
        p.remove(20);
        assert_eq!(p.selection(), Some(10));
    }
}
