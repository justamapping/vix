use serde::{Deserialize, Serialize};
use vt100::{Cell, Color, Screen};

use crate::carry::arrow;
use crate::motion::Pos;
use crate::vt::{self, Modes};

/// Last char of a prompt, when a blank follows it.
const GLYPHS: [&str; 9] = ["$", "%", "#", ">", "❯", "›", "»", "➜", "λ"];

/// A program's input, read off the screen: the text after its prompt through the rows it wraps onto.
#[derive(Debug, PartialEq)]
pub struct Input {
    pub text: String,
    /// The screen cell of each char, then of the end.
    pub cells: Vec<(u16, u16)>,
    /// The program's cursor, as a char index.
    pub cursor: usize,
}

impl Input {
    pub fn len(&self) -> usize {
        self.cells.len() - 1
    }

    /// Char index of cell `(row, col)`, clamped to the input's part of that row; `None` on rows it doesn't cover.
    pub fn index(&self, row: u16, col: u16) -> Option<usize> {
        let on_row = |&(r, _): &(u16, u16)| r == row;
        self.cells.iter().rposition(|&c| on_row(&c) && c.1 <= col).or_else(|| self.cells.iter().position(on_row))
    }
}

/// An input as view n sees it, for editing there.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub text: String,
    pub cursor: usize,
    /// The view n position of each char (row counting scrollback, char index into it), then of the end.
    pub at: Vec<Pos>,
}

impl Field {
    /// Char index of view n position `pos`; anywhere on an empty input's row is its start.
    pub fn index(&self, pos: Pos) -> Option<usize> {
        let empty = self.text.is_empty() && pos.0 == self.at[0].0;
        self.at.iter().position(|&p| p == pos).or(empty.then_some(0))
    }
}

/// `input` in view n's coordinates.
pub fn field(screen: &mut Screen) -> Option<Field> {
    let history = vt::history(screen);
    let Input { text, cells, cursor } = input(screen)?;
    let at = cells.into_iter().map(|(r, c)| (history + r as usize, vt::index(screen, r, c))).collect();
    Some(Field { text, cursor, at })
}

/// An edit made in view n: the input `old`, with the program's cursor at `cursor`, becomes `new` with it at
/// `new_cursor`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    pub old: String,
    pub cursor: usize,
    pub new: String,
    pub new_cursor: usize,
}

impl Change {
    /// The keys that make it, if `screen` still shows the input it was made from.
    pub fn keys(&self, screen: &Screen, modes: Modes) -> Option<Vec<u8>> {
        input(screen).filter(|i| i.text == self.old && i.cursor == self.cursor)?;
        keys(&self.old, self.cursor, &self.new, self.new_cursor, modes)
    }
}

/// The input under the program's cursor. It starts after a prompt: the first glyph and blank inside a styled prefix
/// (`>>> `), else the end of that prefix (`user@host ~ `), else the first glyph and blank on the row (`$ `). vt wraps
/// join rows with nothing; the program's own wraps, rows indented to where the text starts, join with a space. Dim
/// text from the cursor on is a placeholder or an autosuggestion, not input. `None` under full-screen programs.
pub fn input(screen: &Screen) -> Option<Input> {
    if screen.alternate_screen() {
        return None;
    }
    let ((rows, cols), (crow, ccol)) = (screen.size(), screen.cursor_position());
    let top = prompt_row(screen, line(screen, crow).0);
    let mut start = prompt_end(screen, top).unwrap_or(0);
    if top == crow {
        start = start.min(ccol);
    }
    let (mut text, mut cells) = (String::new(), Vec::new());
    let (mut row, mut from) = (top, start);
    loop {
        let mut end = if screen.row_wrapped(row) { cols } else { (from..cols).rfind(|&c| !blank(screen.cell(row, c))).map_or(from, |c| c + 1) };
        if row == crow {
            end = end.max(ccol);
        }
        let mut ghost = false;
        for col in from..end {
            let cell = screen.cell(row, col);
            if (row, col) >= (crow, ccol) && cell.is_some_and(faint) {
                (end, ghost) = (col, true);
                break;
            }
            if cell.is_some_and(|c| c.is_wide_continuation()) {
                continue;
            }
            for ch in cell.map(|c| c.contents()).filter(|s| !s.is_empty()).unwrap_or(" ").chars() {
                text.push(ch);
                cells.push((row, col));
            }
        }
        if !ghost && row + 1 < rows && screen.row_wrapped(row) {
            (row, from) = (row + 1, 0);
            continue;
        }
        cells.push((row, end));
        if ghost || row + 1 >= rows || start == 0 || indent(screen, row + 1) != Some(start) {
            break;
        }
        text.push(' ');
        (row, from) = (row + 1, start);
    }
    let cursor = cells.iter().position(|&c| c == (crow, ccol))?;
    Some(Input { text, cells, cursor })
}

/// First and last rows of the vt-wrapped line through `row`.
pub fn line(screen: &Screen, row: u16) -> (u16, u16) {
    let (mut first, mut last) = (row, row);
    while first > 0 && screen.row_wrapped(first - 1) {
        first -= 1;
    }
    while last + 1 < screen.size().0 && screen.row_wrapped(last) {
        last += 1;
    }
    (first, last)
}

/// The prompt's row for an input whose line starts at `first`: the row above a run of rows indented to where its
/// text starts, else `first` itself.
fn prompt_row(screen: &Screen, first: u16) -> u16 {
    let Some(k) = indent(screen, first).filter(|&k| k > 0) else { return first };
    let mut r = first;
    while r > 0 && indent(screen, r - 1) == Some(k) && !screen.row_wrapped(r - 1) {
        r -= 1;
    }
    match r.checked_sub(1) {
        Some(p) if !screen.row_wrapped(p) && prompt_end(screen, p) == Some(k) => p,
        _ => first,
    }
}

/// Column where the input starts on a prompt's row, if it has a prompt.
fn prompt_end(screen: &Screen, row: u16) -> Option<u16> {
    let cols = screen.size().1;
    let cell = |c: u16| screen.cell(row, c);
    let glyph = |c: &u16| cell(*c).is_some_and(|x| GLYPHS.contains(&x.contents())) && blank(cell(c + 1));
    let lead = (0..cols).take_while(|&c| blank(cell(c)) || cell(c).is_some_and(styled)).count() as u16;
    if (0..lead).any(|c| !blank(cell(c))) {
        return Some((0..lead).find(glyph).map_or(lead, |c| c + 2));
    }
    (0..cols).find(glyph).map(|c| c + 2)
}

fn indent(screen: &Screen, row: u16) -> Option<u16> {
    (0..screen.size().1).find(|&c| !blank(screen.cell(row, c)))
}

fn blank(cell: Option<&Cell>) -> bool {
    cell.is_none_or(|c| c.contents().chars().all(char::is_whitespace))
}

fn styled(cell: &Cell) -> bool {
    cell.fgcolor() != Color::Default || cell.bgcolor() != Color::Default || cell.bold()
}

/// Ghost text: dim, or zsh-autosuggestions' default bright black.
fn faint(cell: &Cell) -> bool {
    cell.dim() || cell.fgcolor() == Color::Idx(8)
}

/// Keys that turn a program's input `old`, cursor at char `cursor`, into `new` with the cursor at `new_cursor`:
/// arrows to the end of the change, backspaces over it, the new text, arrows to the new cursor. Never moves right
/// past the end, since that accepts autosuggestions. `None` if the new text can't be typed safely.
pub fn keys(old: &str, cursor: usize, new: &str, new_cursor: usize, modes: Modes) -> Option<Vec<u8>> {
    let (old, new): (Vec<char>, Vec<char>) = (old.chars().collect(), new.chars().collect());
    if cursor > old.len() || new_cursor > new.len() {
        return None;
    }
    if old == new {
        return Some(moves(cursor, new_cursor, modes.app_cursor));
    }
    // a block cursor is the program's own vi normal mode, where backspace and text are commands
    if matches!(modes.cursor_shape, 1 | 2) {
        return None;
    }
    let pre = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let room = old.len().min(new.len()) - pre;
    let suf = old.iter().rev().zip(new.iter().rev()).take(room).take_while(|(a, b)| a == b).count();
    let (end, added) = (old.len() - suf, &new[pre..new.len() - suf]);
    let mut out = moves(cursor, end, modes.app_cursor);
    out.extend("\x7f".repeat(end - pre).bytes());
    out.extend(typed(added, modes.bracketed_paste)?);
    out.extend(moves(pre + added.len(), new_cursor, modes.app_cursor));
    Some(out)
}

fn moves(from: usize, to: usize, app: bool) -> Vec<u8> {
    arrow(to < from, app).repeat(from.abs_diff(to)).into_bytes()
}

/// Plain keystrokes, or a bracketed paste when there's a newline or tab, which would otherwise run or complete.
fn typed(text: &[char], paste: bool) -> Option<Vec<u8>> {
    let s: String = text.iter().collect();
    let special = |c: char| matches!(c, '\n' | '\t');
    if s.chars().any(|c| c.is_control() && !special(c)) {
        return None;
    }
    if !s.chars().any(special) {
        return Some(s.into_bytes());
    }
    paste.then(|| format!("\x1b[200~{s}\x1b[201~").into_bytes())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::vt;

    /// A screen captured with `DRIVE_CMD=... DRIVE_SAVE=tests/screens/name.vt` (zsh is the author's config).
    pub fn fixture(name: &str, rows: u16, cols: u16) -> vt::Parser {
        let mut p = vt::parser(cols, rows);
        p.process(&std::fs::read(format!("{}/tests/screens/{name}.vt", env!("CARGO_MANIFEST_DIR"))).unwrap());
        p
    }

    fn read(name: &str, rows: u16, cols: u16) -> Option<(String, usize)> {
        input(fixture(name, rows, cols).screen()).map(|i| (i.text, i.cursor))
    }

    fn input_of(bytes: &str) -> Option<(String, usize)> {
        let mut p = vt::parser(20, 5);
        p.process(bytes.as_bytes());
        input(p.screen()).map(|i| (i.text, i.cursor))
    }

    fn at(text: &str, cursor: usize) -> Option<(String, usize)> {
        Some((text.into(), cursor))
    }

    #[test]
    fn shells_and_repls() {
        assert_eq!(read("zsh_empty", 8, 40), at("", 0));
        assert_eq!(read("zsh", 8, 40), at("echo hello world", 16));
        assert_eq!(read("zsh_wrap", 8, 40), at("echo the quick brown fox jumps over the lazy dog", 45));
        assert_eq!(read("bash", 8, 40), at("ls -la", 6));
        assert_eq!(read("python", 8, 40), at("print(1 > 0)", 12));
        assert_eq!(read("node", 8, 40), at("let x = 1", 9));
    }

    #[test]
    fn claude_code() {
        assert_eq!(read("claude_empty", 14, 50), at("", 0));
        assert_eq!(read("claude", 14, 50), at("fix the bug", 8));
        let text = "the quick brown fox jumps over the lazy dog and keeps running far away second line";
        assert_eq!(read("claude_wrap", 14, 50), at(text, 55));
    }

    #[test]
    fn cells_and_clamping() {
        let p = fixture("claude_wrap", 14, 50);
        let i = input(p.screen()).unwrap();
        assert_eq!((i.cells[0], i.cells[43], i.cells[44], i.cells[i.len()]), ((7, 2), (7, 45), (8, 2), (9, 13)));
        assert_eq!(i.index(7, 0), Some(0));
        assert_eq!(i.index(8, 49), Some(70));
        assert_eq!(i.index(6, 2), None);
        assert_eq!(i.index(10, 0), None);
    }

    #[test]
    fn no_prompt_glyph_or_trailing_space() {
        assert_eq!(input_of("plain text"), at("plain text", 10));
        assert_eq!(input_of("$ ls "), at("ls ", 3));
        assert_eq!(input_of("$ echo a > b"), at("echo a > b", 10));
        assert_eq!(input_of("\x1b[?1049h$ ls"), None);
    }

    #[test]
    fn autosuggestions_are_not_input() {
        assert_eq!(input_of("$ ec\x1b[90mho hi\x1b[m\x1b[5D"), at("ec", 2));
        assert_eq!(input_of("$ ec\x1b[2mho hi\x1b[m\x1b[5D"), at("ec", 2));
    }

    #[test]
    fn field_is_in_view_n_coordinates() {
        let mut p = vt::parser(20, 3);
        p.process("out\r\nput\r\nmore\r\n$ 日本x".as_bytes());
        let f = field(p.screen_mut()).unwrap();
        assert_eq!((f.text.as_str(), f.cursor), ("日本x", 3));
        assert_eq!(f.at, vec![(3, 2), (3, 3), (3, 4), (3, 5)]);
        assert_eq!((f.index((3, 3)), f.index((3, 0)), f.index((2, 2))), (Some(1), None, None));
        let mut p = vt::parser(20, 3);
        p.process(b"$ ");
        assert_eq!(field(p.screen_mut()).unwrap().index((0, 0)), Some(0));
    }

    #[test]
    fn changes_only_apply_to_the_input_they_were_made_from() {
        let mut p = vt::parser(20, 3);
        p.process(b"$ echo hi");
        let change = |old: &str, cursor| Change { old: old.into(), cursor, new: "echo".into(), new_cursor: 4 };
        assert_eq!(change("echo hi", 7).keys(p.screen(), PLAIN), Some(bs(3).into_bytes()));
        assert_eq!(change("echo ho", 7).keys(p.screen(), PLAIN), None);
        assert_eq!(change("echo hi", 6).keys(p.screen(), PLAIN), None);
    }

    #[test]
    fn a_block_cursor_only_moves() {
        let block = Modes { cursor_shape: 2, ..PLAIN };
        assert_eq!(keys("ab", 2, "ab", 0, block), Some(l(2).into_bytes()));
        assert_eq!(keys("ab", 2, "a", 1, block), None);
    }

    const PLAIN: Modes = Modes {
        app_keypad: false,
        app_cursor: false,
        bracketed_paste: false,
        mouse: 0,
        mouse_encoding: 0,
        cursor_shape: 0,
    };

    fn k(old: &str, cursor: usize, new: &str, new_cursor: usize) -> Option<String> {
        keys(old, cursor, new, new_cursor, PLAIN).map(|b| String::from_utf8(b).unwrap())
    }

    fn l(n: usize) -> String {
        "\x1b[D".repeat(n)
    }

    fn r(n: usize) -> String {
        "\x1b[C".repeat(n)
    }

    fn bs(n: usize) -> String {
        "\x7f".repeat(n)
    }

    #[test]
    fn motion_only() {
        assert_eq!(k("echo hi", 7, "echo hi", 0), Some(l(7)));
        assert_eq!(k("echo hi", 0, "echo hi", 5), Some(r(5)));
        assert_eq!(k("echo hi", 3, "echo hi", 3), Some(String::new()));
    }

    #[test]
    fn insert_at_cursor() {
        assert_eq!(k("ab", 1, "aXb", 2), Some("X".into()));
        assert_eq!(k("ls", 2, "ls -la", 6), Some(" -la".into()));
    }

    #[test]
    fn delete_word_moves_to_its_end_then_backspaces() {
        assert_eq!(k("echo hello world", 5, "echo world", 5), Some(r(6) + &bs(6)));
    }

    #[test]
    fn delete_to_end_never_passes_the_end() {
        assert_eq!(k("echo hi there", 7, "echo hi", 6), Some(r(6) + &bs(6) + &l(1)));
        assert_eq!(k("echo hi", 7, "echo h", 6), Some(bs(1)));
    }

    #[test]
    fn change_word() {
        assert_eq!(k("git push origin", 4, "git pull origin", 8), Some(r(4) + &bs(2) + "ll"));
        assert_eq!(k("rm a b", 0, "b", 0), Some(r(5) + &bs(5)));
    }

    #[test]
    fn counts_chars_not_bytes() {
        assert_eq!(k("é日x", 3, "é日", 2), Some(bs(1)));
        assert_eq!(k("日本", 0, "日本", 2), Some(r(2)));
    }

    #[test]
    fn application_cursor_keys() {
        let app = Modes { app_cursor: true, ..PLAIN };
        assert_eq!(keys("ab", 2, "ab", 0, app), Some(b"\x1bOD\x1bOD".to_vec()));
    }

    #[test]
    fn newlines_need_bracketed_paste() {
        assert_eq!(k("a", 1, "a\nb", 3), None);
        let paste = Modes { bracketed_paste: true, ..PLAIN };
        assert_eq!(keys("a", 1, "a\nb", 3, paste), Some(b"\x1b[200~\nb\x1b[201~".to_vec()));
        assert_eq!(keys("a", 1, "ab", 2, paste), Some(b"b".to_vec()));
    }

    #[test]
    fn refuses_control_chars_and_bad_cursors() {
        assert_eq!(k("a", 1, "a\x1b[201~", 1), None);
        assert_eq!(k("a", 2, "a", 0), None);
        assert_eq!(k("a", 0, "a", 2), None);
    }
}
