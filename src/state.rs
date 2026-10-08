use crate::input::{CTRL_BACKSLASH, Input};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum View {
    List,
    Insert(u64),
    Normal(u64),
}

/// `<C-\>` transitions. Returns the next view and bytes for the terminal.
/// The list and view n keys are driven by buffers, not here.
pub fn step(view: View, input: Input) -> (View, Vec<u8>) {
    match (view, input) {
        (View::Insert(t), Input::Bytes(b)) => (View::Insert(t), b),
        (View::Insert(t), Input::Escape) => (View::Normal(t), vec![]),
        (View::Normal(t), Input::Escape) => (View::Insert(t), vec![CTRL_BACKSLASH]),
        (view, _) => (view, vec![]),
    }
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
}
