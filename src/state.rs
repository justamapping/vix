use crate::input::Input;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum View {
    List,
    Insert(u64),
    Normal(u64),
}

/// Escape key transitions (`<C-\>` by default). Returns the next view and bytes for the terminal.
/// The list and view n keys are driven by buffers, not here.
pub fn step(view: View, input: Input, esc: u8) -> (View, Vec<u8>) {
    match (view, input) {
        (View::Insert(t), Input::Bytes(b)) => (View::Insert(t), b),
        (View::Insert(t), Input::Escape) => (View::Normal(t), vec![]),
        (View::Normal(t), Input::Escape) => (View::Insert(t), vec![esc]),
        (view, _) => (view, vec![]),
    }
}

/// Chrome's cmd+1-9: index of the nth terminal, with 9 always the last.
pub fn goto(n: usize, len: usize) -> Option<usize> {
    let i = if n == 9 { len.checked_sub(1)? } else { n.checked_sub(1)? };
    (i < len).then_some(i)
}

/// `<Tab>`: the next terminal after `current` that rang, else the next with unseen output, wrapping around.
/// Same priority as the bar's yellow then bold tabs. `flags` is `(bell, activity)` per terminal in list order.
pub fn notifying(flags: &[(bool, bool)], current: Option<usize>) -> Option<usize> {
    let start = current.map_or(0, |c| c + 1);
    let order = || (0..flags.len()).map(|i| (start + i) % flags.len()).filter(|&i| Some(i) != current);
    order().find(|&i| flags[i].0).or_else(|| order().find(|&i| flags[i].1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifying_prefers_bells_then_wraps() {
        let (none, bell, out) = ((false, false), (true, false), (false, true));
        assert_eq!(notifying(&[out, none, bell], Some(1)), Some(2));
        assert_eq!(notifying(&[bell, out, bell], Some(0)), Some(2));
        assert_eq!(notifying(&[bell, out, none], Some(2)), Some(0));
        assert_eq!(notifying(&[none, out, none], None), Some(1));
        assert_eq!(notifying(&[bell, none], Some(0)), None);
        assert_eq!(notifying(&[], None), None);
    }

    #[test]
    fn goto_is_chrome_like() {
        assert_eq!(goto(1, 3), Some(0));
        assert_eq!(goto(3, 3), Some(2));
        assert_eq!(goto(4, 3), None);
        assert_eq!(goto(9, 3), Some(2));
        assert_eq!(goto(9, 12), Some(11));
        assert_eq!(goto(9, 0), None);
    }

    fn bytes(b: &[u8]) -> Input {
        Input::Bytes(b.to_vec())
    }

    #[test]
    fn insert_passes_through() {
        assert_eq!(step(View::Insert(1), bytes(b"ls"), 0x1c), (View::Insert(1), b"ls".to_vec()));
    }

    #[test]
    fn escape_to_normal() {
        assert_eq!(step(View::Insert(1), Input::Escape, 0x1c), (View::Normal(1), vec![]));
    }

    #[test]
    fn double_escape_sends_literal() {
        assert_eq!(step(View::Normal(1), Input::Escape, 0x1c), (View::Insert(1), vec![0x1c]));
    }
}
