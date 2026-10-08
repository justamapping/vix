use crate::input::Key;
use crate::keymap::{self, Action, Cmd, InsertAt, Motion, Operator, Parsed, Target};
use crate::motion::{self, Pos};

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub id: Option<u64>,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Normal,
    Insert,
    Command,
}

#[derive(Debug, PartialEq)]
pub enum Effect {
    Open(usize),
    Write,
    Quit,
    Reload,
}

#[derive(Debug, Clone)]
enum Register {
    Lines(Vec<Line>),
    Chars(String),
}

/// A small vim over lines that carry hidden ids.
pub struct Buffer {
    pub lines: Vec<Line>,
    pub cursor: Pos,
    pub mode: Mode,
    pub cmdline: String,
    pub message: Option<String>,
    saved: Vec<Line>,
    pending: Vec<Key>,
    register: Option<Register>,
    undo: Vec<(Vec<Line>, Pos)>,
    redo: Vec<(Vec<Line>, Pos)>,
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn byte_at(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map_or(s.len(), |(i, _)| i)
}

impl Buffer {
    pub fn new(lines: Vec<Line>) -> Self {
        let mut b = Self {
            lines: Vec::new(),
            cursor: (0, 0),
            mode: Mode::Normal,
            cmdline: String::new(),
            message: None,
            saved: Vec::new(),
            pending: Vec::new(),
            register: None,
            undo: Vec::new(),
            redo: Vec::new(),
        };
        b.load(lines);
        b
    }

    /// Replaces contents as the new saved state, keeping the cursor row where possible.
    pub fn load(&mut self, lines: Vec<Line>) {
        self.lines = if lines.is_empty() { vec![Line { id: None, text: String::new() }] } else { lines };
        self.saved = self.lines.clone();
        self.undo.clear();
        self.redo.clear();
        self.mode = Mode::Normal;
        self.pending.clear();
        self.clamp();
    }

    pub fn modified(&self) -> bool {
        self.lines != self.saved
    }

    pub fn texts(&self) -> Vec<String> {
        self.lines.iter().map(|l| l.text.clone()).collect()
    }

    pub fn key(&mut self, key: Key) -> Vec<Effect> {
        if self.mode == Mode::Normal {
            self.message = None;
        }
        match self.mode {
            Mode::Normal => self.normal(key),
            Mode::Insert => {
                self.insert(key);
                vec![]
            }
            Mode::Command => self.command(key),
        }
    }

    fn normal(&mut self, key: Key) -> Vec<Effect> {
        if key == Key::Esc {
            self.pending.clear();
            return vec![];
        }
        self.pending.push(key);
        match keymap::parse(&self.pending) {
            Parsed::Pending => vec![],
            Parsed::Invalid => {
                self.pending.clear();
                vec![]
            }
            Parsed::Done(cmd) => {
                self.pending.clear();
                self.run(cmd)
            }
        }
    }

    fn run(&mut self, Cmd { count, action }: Cmd) -> Vec<Effect> {
        let n = count.unwrap_or(1).max(1);
        match action {
            Action::Move(m) => {
                self.cursor = motion::apply(&self.texts(), self.cursor, m, count);
                self.clamp();
            }
            Action::Operate(op, target) => self.operate(op, target, count),
            Action::Insert(at) => self.start_insert(at),
            Action::Paste { before } => self.paste(before, n),
            Action::Undo => self.restore(true),
            Action::Redo => self.restore(false),
            Action::Open => return vec![Effect::Open(self.cursor.0)],
            Action::Command => {
                self.mode = Mode::Command;
                self.cmdline.clear();
            }
        }
        vec![]
    }

    fn snapshot(&mut self) {
        self.undo.push((self.lines.clone(), self.cursor));
        self.redo.clear();
    }

    fn restore(&mut self, undo: bool) {
        let (from, to) = if undo { (&mut self.undo, &mut self.redo) } else { (&mut self.redo, &mut self.undo) };
        match from.pop() {
            Some((lines, cursor)) => {
                to.push((std::mem::replace(&mut self.lines, lines), self.cursor));
                self.cursor = cursor;
                self.clamp();
            }
            None => self.message = Some(if undo { "Already at oldest change" } else { "Already at newest change" }.into()),
        }
    }

    fn clamp(&mut self) {
        let row = self.cursor.0.min(self.lines.len() - 1);
        let len = char_len(&self.lines[row].text);
        let max = if self.mode == Mode::Insert { len } else { len.saturating_sub(1) };
        self.cursor = (row, self.cursor.1.min(max));
    }

    fn operate(&mut self, op: Operator, target: Target, count: Option<usize>) {
        let (row, col) = self.cursor;
        let n = count.unwrap_or(1).max(1);
        let linewise = match target {
            Target::Line => Some((row, (row + n - 1).min(self.lines.len() - 1))),
            Target::Motion(m) if m.linewise() => {
                let to = motion::apply(&self.texts(), self.cursor, m, count).0;
                Some((row.min(to), row.max(to)))
            }
            Target::Motion(_) => None,
        };
        if let Some((r0, r1)) = linewise {
            return self.operate_lines(op, r0, r1);
        }
        let Target::Motion(mut m) = target else { unreachable!() };
        let len = char_len(&self.lines[row].text);
        // cw on a word acts like ce
        let on_word = self.lines[row].text.chars().nth(col).is_some_and(|c| !c.is_whitespace());
        if op == Operator::Change && on_word {
            m = match m {
                Motion::WordFwd => Motion::WordEnd,
                Motion::BigWordFwd => Motion::BigWordEnd,
                m => m,
            };
        }
        let to = motion::apply(&self.texts(), self.cursor, m, count);
        // charwise ops stay on the cursor's line
        let to_col = match to.0.cmp(&row) {
            std::cmp::Ordering::Greater => len,
            std::cmp::Ordering::Less => 0,
            std::cmp::Ordering::Equal => to.1 + usize::from(m.inclusive()),
        };
        let (c0, c1) = (col.min(to_col), col.max(to_col).min(len));
        self.operate_chars(op, row, c0, c1);
    }

    fn operate_lines(&mut self, op: Operator, r0: usize, r1: usize) {
        self.register = Some(Register::Lines(self.lines[r0..=r1].to_vec()));
        match op {
            Operator::Yank => self.cursor.0 = r0,
            Operator::Delete => {
                self.snapshot();
                self.lines.drain(r0..=r1);
                if self.lines.is_empty() {
                    self.lines.push(Line { id: None, text: String::new() });
                }
                let row = r0.min(self.lines.len() - 1);
                self.cursor = (row, motion::first_non_blank(&self.lines[row].text));
            }
            Operator::Change => {
                self.snapshot();
                let id = self.lines[r0].id;
                self.lines.splice(r0..=r1, [Line { id, text: String::new() }]);
                self.cursor = (r0, 0);
                self.mode = Mode::Insert;
            }
        }
        self.clamp();
    }

    fn operate_chars(&mut self, op: Operator, row: usize, c0: usize, c1: usize) {
        let text = &self.lines[row].text;
        let (b0, b1) = (byte_at(text, c0), byte_at(text, c1));
        self.register = Some(Register::Chars(text[b0..b1].to_string()));
        if op != Operator::Yank {
            self.snapshot();
            self.lines[row].text.replace_range(b0..b1, "");
        }
        if op == Operator::Change {
            self.mode = Mode::Insert;
        }
        self.cursor = (row, c0);
        self.clamp();
    }

    fn start_insert(&mut self, at: InsertAt) {
        self.snapshot();
        let (row, col) = self.cursor;
        let len = char_len(&self.lines[row].text);
        self.mode = Mode::Insert;
        self.cursor = match at {
            InsertAt::Before => (row, col),
            InsertAt::After => (row, (col + 1).min(len)),
            InsertAt::LineStart => (row, motion::first_non_blank(&self.lines[row].text)),
            InsertAt::LineEnd => (row, len),
            InsertAt::Below | InsertAt::Above => {
                let r = if at == InsertAt::Below { row + 1 } else { row };
                self.lines.insert(r, Line { id: None, text: String::new() });
                (r, 0)
            }
        };
    }

    fn insert(&mut self, key: Key) {
        let (row, col) = self.cursor;
        let text = &mut self.lines[row].text;
        match key {
            Key::Esc => {
                self.mode = Mode::Normal;
                self.cursor.1 = col.saturating_sub(1);
                if self.undo.last().is_some_and(|(lines, _)| *lines == self.lines) {
                    self.undo.pop();
                }
                self.clamp();
            }
            Key::Char(c) => {
                text.insert(byte_at(text, col), c);
                self.cursor.1 += 1;
            }
            Key::Enter => {
                let rest = text.split_off(byte_at(text, col));
                self.lines.insert(row + 1, Line { id: None, text: rest });
                self.cursor = (row + 1, 0);
            }
            Key::Backspace if col > 0 => {
                text.remove(byte_at(text, col - 1));
                self.cursor.1 -= 1;
            }
            Key::Backspace if row > 0 => {
                let line = self.lines.remove(row);
                let prev = &mut self.lines[row - 1].text;
                self.cursor = (row - 1, char_len(prev));
                prev.push_str(&line.text);
            }
            Key::Left => self.cursor.1 = col.saturating_sub(1),
            Key::Right => self.cursor.1 = (col + 1).min(char_len(text)),
            _ => {}
        }
    }

    fn paste(&mut self, before: bool, n: usize) {
        let Some(reg) = self.register.clone() else { return };
        self.snapshot();
        let (row, col) = self.cursor;
        match reg {
            Register::Lines(lines) => {
                let at = if before { row } else { row + 1 };
                let many: Vec<Line> = (0..n).flat_map(|_| lines.iter().cloned()).collect();
                self.lines.splice(at..at, many);
                self.cursor = (at, motion::first_non_blank(&self.lines[at].text));
            }
            Register::Chars(s) => {
                let text = &mut self.lines[row].text;
                let at = if before || text.is_empty() { col } else { col + 1 };
                let s = s.repeat(n);
                text.insert_str(byte_at(text, at), &s);
                self.cursor = (row, at + char_len(&s) - 1);
            }
        }
        self.clamp();
    }

    fn command(&mut self, key: Key) -> Vec<Effect> {
        match key {
            Key::Esc => self.mode = Mode::Normal,
            Key::Backspace if self.cmdline.is_empty() => self.mode = Mode::Normal,
            Key::Backspace => {
                self.cmdline.pop();
            }
            Key::Char(c) => self.cmdline.push(c),
            Key::Enter => {
                self.mode = Mode::Normal;
                let cmd = std::mem::take(&mut self.cmdline);
                return self.ex(&cmd);
            }
            _ => {}
        }
        vec![]
    }

    fn ex(&mut self, cmd: &str) -> Vec<Effect> {
        match cmd.trim() {
            "w" => vec![Effect::Write],
            "wq" | "x" => vec![Effect::Write, Effect::Quit],
            "q" if self.modified() => {
                self.message = Some("E37: No write since last change (add ! to override)".into());
                vec![]
            }
            "q" | "q!" => vec![Effect::Quit],
            "e!" => vec![Effect::Reload],
            "" => vec![],
            other => {
                self.message = Some(format!("E492: Not an editor command: {other}"));
                vec![]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(names: &[&str]) -> Buffer {
        Buffer::new(names.iter().enumerate().map(|(i, n)| Line { id: Some(i as u64), text: n.to_string() }).collect())
    }

    fn feed(b: &mut Buffer, s: &str) -> Vec<Effect> {
        crate::input::keys(s.as_bytes()).into_iter().flat_map(|k| b.key(k)).collect()
    }

    fn ids(b: &Buffer) -> Vec<(Option<u64>, &str)> {
        b.lines.iter().map(|l| (l.id, l.text.as_str())).collect()
    }

    #[test]
    fn jj_enter_opens_third() {
        let mut b = buf(&["foo", "bar", "buzz"]);
        assert_eq!(feed(&mut b, "jj\r"), vec![Effect::Open(2)]);
    }

    #[test]
    fn dd_and_p_move_ids() {
        let mut b = buf(&["foo", "bar", "buzz"]);
        feed(&mut b, "ddp");
        assert_eq!(ids(&b), vec![(Some(1), "bar"), (Some(0), "foo"), (Some(2), "buzz")]);
        assert!(b.modified());
    }

    #[test]
    fn yyp_duplicates_id() {
        let mut b = buf(&["foo"]);
        feed(&mut b, "yyp");
        assert_eq!(ids(&b), vec![(Some(0), "foo"), (Some(0), "foo")]);
    }

    #[test]
    fn o_creates_line_without_id() {
        let mut b = buf(&["foo"]);
        feed(&mut b, "oweb\x1b");
        assert_eq!(ids(&b), vec![(Some(0), "foo"), (None, "web")]);
        assert_eq!(b.cursor, (1, 2));
        assert_eq!(b.mode, Mode::Normal);
    }

    #[test]
    fn cw_renames_keeping_id() {
        let mut b = buf(&["foo bar"]);
        feed(&mut b, "cwbaz\x1b");
        assert_eq!(ids(&b), vec![(Some(0), "baz bar")]);
    }

    #[test]
    fn undo_redo() {
        let mut b = buf(&["foo", "bar"]);
        feed(&mut b, "ddu");
        assert_eq!(ids(&b), vec![(Some(0), "foo"), (Some(1), "bar")]);
        assert!(!b.modified());
        feed(&mut b, "\x12");
        assert_eq!(ids(&b), vec![(Some(1), "bar")]);
    }

    #[test]
    fn insert_session_is_one_undo() {
        let mut b = buf(&["foo"]);
        feed(&mut b, "Abar\x1bu");
        assert_eq!(ids(&b), vec![(Some(0), "foo")]);
    }

    #[test]
    fn counts_and_motions() {
        let mut b = buf(&["a", "b", "c", "d"]);
        feed(&mut b, "G");
        assert_eq!(b.cursor.0, 3);
        feed(&mut b, "gg2dd");
        assert_eq!(ids(&b), vec![(Some(2), "c"), (Some(3), "d")]);
        feed(&mut b, "dG");
        assert_eq!(ids(&b), vec![(None, "")]);
    }

    #[test]
    fn x_and_dw() {
        let mut b = buf(&["foo bar"]);
        feed(&mut b, "x");
        assert_eq!(b.lines[0].text, "oo bar");
        feed(&mut b, "dw");
        assert_eq!(b.lines[0].text, "bar");
        feed(&mut b, "$x");
        assert_eq!(b.lines[0].text, "ba");
    }

    #[test]
    fn ex_commands() {
        let mut b = buf(&["foo"]);
        assert_eq!(feed(&mut b, ":w\r"), vec![Effect::Write]);
        assert_eq!(feed(&mut b, ":x\r"), vec![Effect::Write, Effect::Quit]);
        assert_eq!(feed(&mut b, ":e!\r"), vec![Effect::Reload]);
        feed(&mut b, "dd");
        assert_eq!(feed(&mut b, ":q\r"), vec![]);
        assert!(b.message.as_deref().unwrap().starts_with("E37"));
        assert_eq!(feed(&mut b, ":q!\r"), vec![Effect::Quit]);
        feed(&mut b, ":nope\r");
        assert!(b.message.as_deref().unwrap().starts_with("E492"));
    }

    #[test]
    fn insert_enter_and_backspace() {
        let mut b = buf(&["foobar"]);
        feed(&mut b, "3li\r");
        assert_eq!(ids(&b), vec![(Some(0), "foo"), (None, "bar")]);
        feed(&mut b, "\x7f\x1b");
        assert_eq!(ids(&b), vec![(Some(0), "foobar")]);
    }
}
