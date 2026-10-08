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
}
