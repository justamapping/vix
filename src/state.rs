use crate::input::{CTRL_BACKSLASH, Input};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum View {
    List,
    Insert(u64),
    Normal(u64),
}

/// Terminal-side transitions. Returns the next view and bytes for the terminal.
/// The list view is driven by the buffer, not here.
pub fn step(view: View, input: Input) -> (View, Vec<u8>) {
    match (view, input) {
        (View::Insert(t), Input::Bytes(b)) => (View::Insert(t), b),
        (View::Insert(t), Input::Escape) => (View::Normal(t), vec![]),
        (View::Normal(t), Input::Escape) => (View::Insert(t), vec![CTRL_BACKSLASH]),
        (View::Normal(t), Input::Bytes(b)) => normal(t, &b),
        (View::List, _) => (View::List, vec![]),
    }
}

fn normal(t: u64, bytes: &[u8]) -> (View, Vec<u8>) {
    for (i, b) in bytes.iter().enumerate() {
        match b {
            b'i' | b'a' => return (View::Insert(t), bytes[i + 1..].to_vec()),
            b'-' => return (View::List, vec![]),
            _ => {}
        }
    }
    (View::Normal(t), vec![])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(b: &[u8]) -> Input {
        Input::Bytes(b.to_vec())
    }

    #[test]
    fn insert_passes_through() {
        assert_eq!(step(View::Insert(1), bytes(b"ls")), (View::Insert(1), b"ls".to_vec()));
    }

    #[test]
    fn escape_to_normal() {
        assert_eq!(step(View::Insert(1), Input::Escape), (View::Normal(1), vec![]));
    }

    #[test]
    fn double_escape_sends_literal() {
        assert_eq!(step(View::Normal(1), Input::Escape), (View::Insert(1), vec![0x1c]));
    }

    #[test]
    fn i_and_a_back_to_insert() {
        assert_eq!(step(View::Normal(1), bytes(b"i")), (View::Insert(1), vec![]));
        assert_eq!(step(View::Normal(1), bytes(b"xals")), (View::Insert(1), b"ls".to_vec()));
    }

    #[test]
    fn dash_goes_up_to_list() {
        assert_eq!(step(View::Normal(1), bytes(b"-")), (View::List, vec![]));
        assert_eq!(step(View::Normal(1), bytes(b"jk")), (View::Normal(1), vec![]));
    }
}
