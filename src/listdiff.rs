use std::collections::HashSet;

use crate::buffer::Line;

#[derive(Debug, PartialEq)]
pub enum Op {
    Keep { id: u64, name: String },
    Clone { from: u64, name: String },
    Spawn { name: String },
    Kill { id: u64 },
}

/// `:w` of `new`, edited from the list `base`, onto the running list `current`: Keep/Clone/Spawn in new line order
/// (that order is the new list), then Kills. Only terminals the edit started from can die; ones added since stay,
/// after the edited lines. Blank lines are ignored.
pub fn diff(base: &[u64], current: &[(u64, &str)], new: &[Line]) -> Vec<Op> {
    let mut seen = HashSet::new();
    let mut ops = Vec::new();
    for line in new {
        let name = line.text.trim().to_string();
        if name.is_empty() {
            continue;
        }
        ops.push(match line.id.filter(|id| current.iter().any(|(c, _)| c == id)) {
            Some(id) if seen.insert(id) => Op::Keep { id, name },
            Some(from) => Op::Clone { from, name },
            None => Op::Spawn { name },
        });
    }
    let unseen = || current.iter().filter(|(id, _)| !seen.contains(id));
    let (gone, added): (Vec<_>, Vec<_>) = unseen().partition(|(id, _)| base.contains(id));
    ops.extend(added.into_iter().map(|&(id, name)| Op::Keep { id, name: name.into() }));
    ops.extend(gone.into_iter().map(|&(id, _)| Op::Kill { id }));
    ops
}

/// Matches `wanted` names to `current` terminals by name, each to the first one not matched yet, for applying a
/// workflow to a running list. Returns each wanted name's match, and the terminals nothing matched.
pub fn by_name(current: &[(u64, &str)], wanted: &[&str]) -> (Vec<Option<u64>>, Vec<u64>) {
    let mut free: Vec<(u64, &str)> = current.to_vec();
    let matched = wanted
        .iter()
        .map(|w| free.iter().position(|(_, n)| n == w).map(|i| free.remove(i).0))
        .collect();
    (matched, free.into_iter().map(|(id, _)| id).collect())
}

/// Human summary of what `ops` would do to `old` (id, name) terminals, e.g. "spawn web, kill api". Empty if nothing.
pub fn summary(old: &[(u64, &str)], ops: &[Op]) -> String {
    let name = |id: u64| old.iter().find(|(i, _)| *i == id).map_or("?", |(_, n)| n);
    let mut parts = Vec::new();
    let mut kept = Vec::new();
    for op in ops {
        match op {
            Op::Keep { id, name: new } => {
                kept.push(*id);
                if name(*id) != new {
                    parts.push(format!("rename {} to {new}", name(*id)));
                }
            }
            Op::Clone { from, name: new } if name(*from) == new => parts.push(format!("clone {new}")),
            Op::Clone { from, name: new } => parts.push(format!("clone {} as {new}", name(*from))),
            Op::Spawn { name } => parts.push(format!("spawn {name}")),
            Op::Kill { id } => parts.push(format!("kill {}", name(*id))),
        }
    }
    let before: Vec<u64> = old.iter().map(|(id, _)| *id).filter(|id| kept.contains(id)).collect();
    if before != kept {
        parts.insert(0, "reorder".into());
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(id: Option<u64>, text: &str) -> Line {
        Line { id, text: text.to_string() }
    }

    fn keep(id: u64, name: &str) -> Op {
        Op::Keep { id, name: name.to_string() }
    }

    /// Nothing changed underneath the edit.
    fn d(old: &[u64], new: &[Line]) -> Vec<Op> {
        let current: Vec<(u64, &str)> = old.iter().map(|&id| (id, "?")).collect();
        diff(old, &current, new)
    }

    #[test]
    fn by_name_takes_the_first_unused_match() {
        let current = [(1, "web"), (2, "api"), (3, "web"), (4, "logs")];
        assert_eq!(by_name(&current, &["web", "web", "web", "api"]), (vec![Some(1), Some(3), None, Some(2)], vec![4]));
        assert_eq!(by_name(&[], &["web"]), (vec![None], vec![]));
    }

    #[test]
    fn terminals_added_since_survive() {
        let new = [line(Some(2), "b"), line(Some(4), "gone")];
        let current = [(1, "a"), (2, "b"), (3, "agent")];
        let ops = vec![keep(2, "b"), Op::Spawn { name: "gone".into() }, keep(3, "agent"), Op::Kill { id: 1 }];
        assert_eq!(diff(&[1, 2, 4], &current, &new), ops);
    }

    #[test]
    fn unchanged() {
        let new = [line(Some(1), "foo"), line(Some(2), "bar")];
        assert_eq!(d(&[1, 2], &new), vec![keep(1, "foo"), keep(2, "bar")]);
    }

    #[test]
    fn rename_and_reorder() {
        let new = [line(Some(2), "bar"), line(Some(1), "fooo")];
        assert_eq!(d(&[1, 2], &new), vec![keep(2, "bar"), keep(1, "fooo")]);
    }

    #[test]
    fn delete_kills() {
        let new = [line(Some(2), "bar")];
        assert_eq!(d(&[1, 2, 3], &new), vec![keep(2, "bar"), Op::Kill { id: 1 }, Op::Kill { id: 3 }]);
    }

    #[test]
    fn duplicate_clones() {
        let new = [line(Some(1), "foo"), line(Some(1), "foo2"), line(Some(1), "foo")];
        assert_eq!(
            d(&[1], &new),
            vec![
                keep(1, "foo"),
                Op::Clone { from: 1, name: "foo2".into() },
                Op::Clone { from: 1, name: "foo".into() },
            ]
        );
    }

    #[test]
    fn new_lines_spawn() {
        let new = [line(Some(1), "foo"), line(None, "web"), line(Some(9), "gone")];
        assert_eq!(
            d(&[1], &new),
            vec![keep(1, "foo"), Op::Spawn { name: "web".into() }, Op::Spawn { name: "gone".into() }]
        );
    }

    #[test]
    fn blank_lines_ignored() {
        let new = [line(None, ""), line(Some(1), "  "), line(Some(2), " bar ")];
        assert_eq!(d(&[1, 2], &new), vec![keep(2, "bar"), Op::Kill { id: 1 }]);
    }

    #[test]
    fn summaries() {
        let old = [(1, "foo"), (2, "api")];
        let new = [line(Some(1), "foo"), line(Some(1), "foo"), line(Some(1), "web2"), line(None, "web")];
        assert_eq!(summary(&old, &d(&[1, 2], &new)), "clone foo, clone foo as web2, spawn web, kill api");
        let new = [line(Some(2), "api"), line(Some(1), "bar")];
        assert_eq!(summary(&old, &d(&[1, 2], &new)), "reorder, rename foo to bar");
        let new = [line(Some(1), "foo"), line(None, ""), line(Some(2), "api")];
        assert_eq!(summary(&old, &d(&[1, 2], &new)), "");
    }
}
