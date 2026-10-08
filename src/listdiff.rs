use std::collections::HashSet;

use crate::buffer::Line;

#[derive(Debug, PartialEq)]
pub enum Op {
    Keep { id: u64, name: String },
    Clone { from: u64, name: String },
    Spawn { name: String },
    Kill { id: u64 },
}

/// `:w`: Keep/Clone/Spawn in new line order (that order is the new list), then Kills. Blank lines are ignored.
pub fn diff(old: &[u64], new: &[Line]) -> Vec<Op> {
    let mut seen = HashSet::new();
    let mut ops = Vec::new();
    for line in new {
        let name = line.text.trim().to_string();
        if name.is_empty() {
            continue;
        }
        ops.push(match line.id.filter(|id| old.contains(id)) {
            Some(id) if seen.insert(id) => Op::Keep { id, name },
            Some(from) => Op::Clone { from, name },
            None => Op::Spawn { name },
        });
    }
    ops.extend(old.iter().filter(|id| !seen.contains(*id)).map(|&id| Op::Kill { id }));
    ops
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

    #[test]
    fn unchanged() {
        let new = [line(Some(1), "foo"), line(Some(2), "bar")];
        assert_eq!(diff(&[1, 2], &new), vec![keep(1, "foo"), keep(2, "bar")]);
    }

    #[test]
    fn rename_and_reorder() {
        let new = [line(Some(2), "bar"), line(Some(1), "fooo")];
        assert_eq!(diff(&[1, 2], &new), vec![keep(2, "bar"), keep(1, "fooo")]);
    }

    #[test]
    fn delete_kills() {
        let new = [line(Some(2), "bar")];
        assert_eq!(diff(&[1, 2, 3], &new), vec![keep(2, "bar"), Op::Kill { id: 1 }, Op::Kill { id: 3 }]);
    }

    #[test]
    fn duplicate_clones() {
        let new = [line(Some(1), "foo"), line(Some(1), "foo2"), line(Some(1), "foo")];
        assert_eq!(
            diff(&[1], &new),
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
            diff(&[1], &new),
            vec![keep(1, "foo"), Op::Spawn { name: "web".into() }, Op::Spawn { name: "gone".into() }]
        );
    }

    #[test]
    fn blank_lines_ignored() {
        let new = [line(None, ""), line(Some(1), "  "), line(Some(2), " bar ")];
        assert_eq!(diff(&[1, 2], &new), vec![keep(2, "bar"), Op::Kill { id: 1 }]);
    }

    #[test]
    fn summaries() {
        let old = [(1, "foo"), (2, "api")];
        let new = [line(Some(1), "foo"), line(Some(1), "foo"), line(Some(1), "web2"), line(None, "web")];
        assert_eq!(summary(&old, &diff(&[1, 2], &new)), "clone foo, clone foo as web2, spawn web, kill api");
        let new = [line(Some(2), "api"), line(Some(1), "bar")];
        assert_eq!(summary(&old, &diff(&[1, 2], &new)), "reorder, rename foo to bar");
        let new = [line(Some(1), "foo"), line(None, ""), line(Some(2), "api")];
        assert_eq!(summary(&old, &diff(&[1, 2], &new)), "");
    }
}
