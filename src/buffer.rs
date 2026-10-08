use crate::input::Key;
use crate::keymap::{self, Action, Cmd, InsertAt, Motion, Operator, Parsed, Scroll, Target};
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
    Visual { line: bool },
    /// `:` ex command or `/` `?` search, per `prompt`
    Command,
}

#[derive(Debug, PartialEq)]
pub enum Effect {
    Open(usize),
    Write,
    Quit { force: bool },
    QuitAll { force: bool },
    Reload,
    Yank(String),
    /// keys a read-only buffer passes up: `i a I A`, `-`, `J`/`K` with count, `<C-^>`
    Insert,
    Parent,
    Switch(isize),
    Alternate,
}

#[derive(Debug, Clone)]
enum Register {
    Lines(Vec<Line>),
    Chars(String),
}

const READONLY: &str = "E21: Cannot make changes, 'modifiable' is off";

/// A small vim over lines that carry hidden ids.
pub struct Buffer {
    pub lines: Vec<Line>,
    pub cursor: Pos,
    pub mode: Mode,
    pub prompt: char,
    pub cmdline: String,
    pub message: Option<String>,
    pub readonly: bool,
    /// first visible row and how many rows are visible
    pub top: usize,
    pub height: usize,
    anchor: Pos,
    saved: Vec<Line>,
    pending: Vec<Key>,
    register: Option<Register>,
    search: Option<(String, bool)>,
    last_find: Option<Motion>,
    undo: Vec<(Vec<Line>, Pos)>,
    redo: Vec<(Vec<Line>, Pos)>,
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn byte_at(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map_or(s.len(), |(i, _)| i)
}

/// First visible row so that `row` stays on screen.
pub fn scroll(top: usize, row: usize, height: usize) -> usize {
    if row < top {
        row
    } else if row >= top + height {
        row + 1 - height
    } else {
        top
    }
}

impl Buffer {
    pub fn new(lines: Vec<Line>) -> Self {
        let mut b = Self {
            lines: Vec::new(),
            cursor: (0, 0),
            mode: Mode::Normal,
            prompt: ':',
            cmdline: String::new(),
            message: None,
            readonly: false,
            top: 0,
            height: 1,
            anchor: (0, 0),
            saved: Vec::new(),
            pending: Vec::new(),
            register: None,
            search: None,
            last_find: None,
            undo: Vec::new(),
            redo: Vec::new(),
        };
        b.load(lines);
        b
    }

    /// Replaces contents as the new saved state, keeping the cursor row where possible.
    pub fn load(&mut self, lines: Vec<Line>) {
        self.undo.clear();
        self.redo.clear();
        self.mode = Mode::Normal;
        self.pending.clear();
        self.refresh(lines);
    }

    /// Moves the cursor and viewport, then clamps both.
    pub fn place(&mut self, cursor: Pos, top: usize) {
        (self.cursor, self.top) = (cursor, top);
        self.clamp();
    }

    /// Replaces contents without touching mode or history (live text under a read-only buffer).
    pub fn refresh(&mut self, lines: Vec<Line>) {
        self.lines = if lines.is_empty() { vec![Line { id: None, text: String::new() }] } else { lines };
        self.saved = self.lines.clone();
        self.anchor.0 = self.anchor.0.min(self.lines.len() - 1);
        self.clamp();
    }

    /// Drops lines of a terminal that no longer exists, edits or not.
    pub fn remove_id(&mut self, id: u64) {
        self.lines.retain(|l| l.id != Some(id));
        self.saved.retain(|l| l.id != Some(id));
        if self.lines.is_empty() {
            self.lines.push(Line { id: None, text: String::new() });
        }
        self.clamp();
    }

    pub fn modified(&self) -> bool {
        self.lines != self.saved
    }

    pub fn texts(&self) -> Vec<String> {
        self.lines.iter().map(|l| l.text.clone()).collect()
    }

    /// Visual selection as ordered (start, end, linewise), end inclusive.
    pub fn selection(&self) -> Option<(Pos, Pos, bool)> {
        let Mode::Visual { line } = self.mode else { return None };
        let (a, b) = if self.anchor <= self.cursor { (self.anchor, self.cursor) } else { (self.cursor, self.anchor) };
        Some((a, b, line))
    }

    pub fn key(&mut self, key: Key) -> Vec<Effect> {
        if matches!(self.mode, Mode::Normal | Mode::Visual { .. }) {
            self.message = None;
        }
        let effects = match self.mode {
            Mode::Normal => self.normal(key),
            Mode::Visual { line } => self.visual(key, line),
            Mode::Insert => {
                self.insert(key);
                vec![]
            }
            Mode::Command => self.command(key),
        };
        self.clamp();
        effects
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

    fn visual(&mut self, key: Key, line: bool) -> Vec<Effect> {
        if self.pending.is_empty() {
            let op = match key {
                Key::Char('y' | 'Y') => Some(Operator::Yank),
                Key::Char('d' | 'x' | 'X' | 'D') => Some(Operator::Delete),
                Key::Char('c' | 's' | 'C' | 'S') => Some(Operator::Change),
                _ => None,
            };
            if let Some(op) = op {
                let upper = matches!(key, Key::Char(c) if c.is_ascii_uppercase());
                return self.operate_selection(op, line || upper);
            }
            match key {
                Key::Esc => self.mode = Mode::Normal,
                Key::Char('v') => self.mode = if line { Mode::Visual { line: false } } else { Mode::Normal },
                Key::Char('V') => self.mode = if line { Mode::Normal } else { Mode::Visual { line: true } },
                Key::Char('o') => std::mem::swap(&mut self.anchor, &mut self.cursor),
                Key::Char(':') => return self.run(Cmd { count: None, action: Action::Command }),
                _ => {}
            }
            if !matches!(key, Key::Esc | Key::Char('v' | 'V' | 'o')) {
                return self.visual_motion(key);
            }
            return vec![];
        }
        self.visual_motion(key)
    }

    fn visual_motion(&mut self, key: Key) -> Vec<Effect> {
        self.pending.push(key);
        match keymap::parse(&self.pending) {
            Parsed::Pending => return vec![],
            Parsed::Done(cmd)
                if matches!(
                    cmd.action,
                    Action::Move(_) | Action::Scroll(_) | Action::SearchNext { .. } | Action::RepeatFind { .. }
                ) =>
            {
                self.pending.clear();
                return self.run(cmd);
            }
            _ => self.pending.clear(),
        }
        vec![]
    }

    fn run(&mut self, Cmd { count, action }: Cmd) -> Vec<Effect> {
        let n = count.unwrap_or(1).max(1);
        match action {
            Action::Move(m) => {
                if let Motion::Find { .. } = m {
                    self.last_find = Some(m);
                }
                self.cursor = motion::apply(&self.texts(), self.cursor, m, count);
            }
            Action::RepeatFind { reverse } => {
                if let Some(Motion::Find { ch, back, till }) = self.last_find {
                    let m = Motion::Find { ch, back: back != reverse, till };
                    self.cursor = motion::apply(&self.texts(), self.cursor, m, count);
                }
            }
            Action::Operate(Operator::Yank, target) => return self.operate(Operator::Yank, target, count),
            Action::Insert(_) if self.readonly => return vec![Effect::Insert],
            Action::Operate(..) | Action::Insert(_) | Action::Paste { .. } if self.readonly => {
                self.message = Some(READONLY.into());
            }
            Action::Operate(op, target) => return self.operate(op, target, count),
            Action::Insert(at) => self.start_insert(at),
            Action::Paste { before } => self.paste(before, n),
            Action::Undo => self.restore(true),
            Action::Redo => self.restore(false),
            Action::Open => return vec![Effect::Open(self.cursor.0)],
            Action::Command | Action::Search { .. } => {
                self.prompt = match action {
                    Action::Search { back: true } => '?',
                    Action::Search { back: false } => '/',
                    _ => ':',
                };
                self.mode = Mode::Command;
                self.cmdline.clear();
            }
            Action::SearchNext { reverse } => {
                if let Some((pat, back)) = self.search.clone() {
                    (0..n).for_each(|_| self.find(&pat, back != reverse));
                }
            }
            Action::Visual { line } => {
                self.mode = Mode::Visual { line };
                self.anchor = self.cursor;
            }
            Action::Scroll(s) => self.scroll_by(s, count),
            Action::Parent => return vec![Effect::Parent],
            Action::Switch { back } => return vec![Effect::Switch(if back { -(n as isize) } else { n as isize })],
            Action::Alternate => return vec![Effect::Alternate],
        }
        vec![]
    }

    fn scroll_by(&mut self, s: Scroll, count: Option<usize>) {
        let h = self.height.max(1);
        let len = self.lines.len();
        let max_top = len.saturating_sub(h);
        let last = len - 1;
        let row = self.cursor.0;
        let half = count.unwrap_or(h / 2).max(1);
        let visible = h.min(len - self.top.min(last));
        let n = count.unwrap_or(1).max(1);
        match s {
            Scroll::LineDown => self.top = (self.top + n).min(max_top),
            Scroll::LineUp => self.top = self.top.saturating_sub(n),
            Scroll::HalfDown => {
                self.top = (self.top + half).min(max_top);
                self.cursor.0 = (row + half).min(last);
            }
            Scroll::HalfUp => {
                self.top = self.top.saturating_sub(half);
                self.cursor.0 = row.saturating_sub(half);
            }
            Scroll::PageDown => {
                self.top = (self.top + h.saturating_sub(2).max(1) * n).min(max_top);
                self.cursor.0 = self.cursor.0.max(self.top);
            }
            Scroll::PageUp => {
                self.top = self.top.saturating_sub(h.saturating_sub(2).max(1) * n);
                self.cursor.0 = self.cursor.0.min(self.top + h - 1);
            }
            Scroll::Top => self.cursor.0 = self.top + (n - 1).min(visible - 1),
            Scroll::Middle => self.cursor.0 = self.top + (visible - 1) / 2,
            Scroll::Bottom => self.cursor.0 = self.top + (visible - 1).saturating_sub(n - 1),
        }
        // keep the cursor inside the moved viewport, as vim does
        self.cursor.0 = self.cursor.0.clamp(self.top, (self.top + h - 1).min(last));
        if matches!(s, Scroll::Top | Scroll::Middle | Scroll::Bottom) {
            self.cursor.1 = motion::first_non_blank(&self.lines[self.cursor.0].text);
        }
    }

    fn find(&mut self, pat: &str, back: bool) {
        match motion::search(&self.texts(), self.cursor, pat, back) {
            Some((pos, wrapped)) => {
                self.cursor = pos;
                self.message = wrapped.then(|| {
                    if back { "search hit TOP, continuing at BOTTOM" } else { "search hit BOTTOM, continuing at TOP" }.into()
                });
            }
            None => self.message = Some(format!("E486: Pattern not found: {pat}")),
        }
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
            }
            None => self.message = Some(if undo { "Already at oldest change" } else { "Already at newest change" }.into()),
        }
    }

    /// Keeps the cursor on the text and on screen.
    fn clamp(&mut self) {
        let row = self.cursor.0.min(self.lines.len() - 1);
        let len = char_len(&self.lines[row].text);
        let max = if self.mode == Mode::Insert { len } else { len.saturating_sub(1) };
        self.cursor = (row, self.cursor.1.min(max));
        self.top = scroll(self.top, row, self.height.max(1));
    }

    fn register_text(&self) -> String {
        match &self.register {
            Some(Register::Lines(lines)) => lines.iter().map(|l| format!("{}\n", l.text)).collect(),
            Some(Register::Chars(s)) => s.clone(),
            None => String::new(),
        }
    }

    fn yanked(&self, op: Operator) -> Vec<Effect> {
        if op == Operator::Yank { vec![Effect::Yank(self.register_text())] } else { vec![] }
    }

    fn operate(&mut self, op: Operator, target: Target, count: Option<usize>) -> Vec<Effect> {
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
            self.operate_lines(op, r0, r1);
            return self.yanked(op);
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
        if let Motion::Find { .. } = m {
            self.last_find = Some(m);
        }
        // a failed f/t does nothing
        if let Motion::Find { ch, back, till } = m {
            let chars: Vec<char> = self.lines[row].text.chars().collect();
            if motion::find(&chars, col, ch, back, till, count.unwrap_or(1).max(1)).is_none() {
                return vec![];
            }
        }
        let to = motion::apply(&self.texts(), self.cursor, m, count);
        // charwise ops stay on the cursor's line
        let to_col = match to.0.cmp(&row) {
            std::cmp::Ordering::Greater => len,
            std::cmp::Ordering::Less => 0,
            std::cmp::Ordering::Equal => to.1 + usize::from(m.inclusive()),
        };
        let (c0, c1) = (col.min(to_col), col.max(to_col).min(len));
        self.operate_span(op, (row, c0), (row, c1));
        self.yanked(op)
    }

    fn operate_selection(&mut self, op: Operator, linewise: bool) -> Vec<Effect> {
        let Some((start, end, _)) = self.selection() else { return vec![] };
        self.mode = Mode::Normal;
        if self.readonly && op != Operator::Yank {
            self.message = Some(READONLY.into());
            return vec![];
        }
        if linewise {
            self.operate_lines(op, start.0, end.0);
        } else {
            let end_len = char_len(&self.lines[end.0].text);
            self.operate_span(op, start, (end.0, (end.1 + 1).min(end_len)));
        }
        self.yanked(op)
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
    }

    /// Charwise op from `start` up to (not including) `end`, possibly across lines.
    fn operate_span(&mut self, op: Operator, start: Pos, end: Pos) {
        let ((r0, c0), (r1, c1)) = (start, end);
        let b0 = byte_at(&self.lines[r0].text, c0);
        let b1 = byte_at(&self.lines[r1].text, c1);
        let text = if r0 == r1 {
            self.lines[r0].text[b0..b1].to_string()
        } else {
            let mut parts = vec![&self.lines[r0].text[b0..]];
            parts.extend(self.lines[r0 + 1..r1].iter().map(|l| l.text.as_str()));
            parts.push(&self.lines[r1].text[..b1]);
            parts.join("\n")
        };
        self.register = Some(Register::Chars(text));
        if op != Operator::Yank {
            self.snapshot();
            let tail = self.lines[r1].text[b1..].to_string();
            self.lines[r0].text.truncate(b0);
            self.lines[r0].text.push_str(&tail);
            self.lines.drain(r0 + 1..=r1);
        }
        if op == Operator::Change {
            self.mode = Mode::Insert;
        }
        self.cursor = start;
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
                let text = &self.lines[row].text;
                let at = if before || text.is_empty() { col } else { col + 1 };
                let split = byte_at(text, at);
                let s = s.repeat(n);
                let tail = self.lines[row].text.split_off(split);
                let mut parts = s.split('\n');
                self.lines[row].text.push_str(parts.next().unwrap_or_default());
                let rest: Vec<&str> = parts.collect();
                match rest.split_last() {
                    None => {
                        self.lines[row].text.push_str(&tail);
                        self.cursor = (row, at + char_len(&s).saturating_sub(1));
                    }
                    Some((last, middle)) => {
                        let mut new: Vec<Line> = middle.iter().map(|t| Line { id: None, text: t.to_string() }).collect();
                        new.push(Line { id: None, text: format!("{last}{tail}") });
                        self.lines.splice(row + 1..row + 1, new);
                        self.cursor = (row, at);
                    }
                }
            }
        }
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
                if self.prompt == ':' {
                    return self.ex(&cmd);
                }
                let back = self.prompt == '?';
                let pat = if cmd.is_empty() { self.search.clone().map(|(p, _)| p).unwrap_or_default() } else { cmd };
                if !pat.is_empty() {
                    self.search = Some((pat.clone(), back));
                    self.find(&pat, back);
                }
            }
            _ => {}
        }
        vec![]
    }

    fn ex(&mut self, cmd: &str) -> Vec<Effect> {
        let cmd = cmd.trim();
        if let Ok(n) = cmd.parse::<usize>() {
            self.cursor = (n.saturating_sub(1).min(self.lines.len() - 1), 0);
            self.cursor.1 = motion::first_non_blank(&self.lines[self.cursor.0].text);
            return vec![];
        }
        match cmd {
            "w" | "w!" if self.readonly => {
                self.message = Some("E45: 'readonly' option is set".into());
                vec![]
            }
            "wq" | "x" if self.readonly => vec![Effect::Quit { force: false }],
            "w" => vec![Effect::Write],
            "wq" | "x" => vec![Effect::Write, Effect::Quit { force: false }],
            "q" if self.modified() => {
                self.message = Some("E37: No write since last change (add ! to override)".into());
                vec![]
            }
            "q" => vec![Effect::Quit { force: false }],
            "q!" => vec![Effect::Quit { force: true }],
            "qa" | "qall" => vec![Effect::QuitAll { force: false }],
            "qa!" | "qall!" => vec![Effect::QuitAll { force: true }],
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
        assert_eq!(feed(&mut b, ":x\r"), vec![Effect::Write, Effect::Quit { force: false }]);
        assert_eq!(feed(&mut b, ":e!\r"), vec![Effect::Reload]);
        feed(&mut b, "dd");
        assert_eq!(feed(&mut b, ":q\r"), vec![]);
        assert!(b.message.as_deref().unwrap().starts_with("E37"));
        assert_eq!(feed(&mut b, ":q!\r"), vec![Effect::Quit { force: true }]);
        assert_eq!(feed(&mut b, ":qa!\r"), vec![Effect::QuitAll { force: true }]);
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

    fn view(lines: &[&str]) -> Buffer {
        let mut b = Buffer::new(lines.iter().map(|t| Line { id: None, text: t.to_string() }).collect());
        b.readonly = true;
        b
    }

    #[test]
    fn visual_yank() {
        let mut b = buf(&["foo bar", "baz qux"]);
        assert_eq!(feed(&mut b, "wvjy"), vec![Effect::Yank("bar\nbaz q".into())]);
        assert_eq!(b.mode, Mode::Normal);
        assert_eq!(b.cursor, (0, 4));
        assert_eq!(feed(&mut b, "Vjy"), vec![Effect::Yank("foo bar\nbaz qux\n".into())]);
        assert_eq!(feed(&mut b, "0veoy"), vec![Effect::Yank("foo".into())]);
    }

    #[test]
    fn visual_delete_across_lines_and_paste_back() {
        let mut b = buf(&["foo bar", "baz qux"]);
        feed(&mut b, "wvjd");
        assert_eq!(ids(&b), vec![(Some(0), "foo ux")]);
        feed(&mut b, "P");
        assert_eq!(ids(&b), vec![(Some(0), "foo bar"), (None, "baz qux")]);
    }

    #[test]
    fn readonly_passes_keys_up() {
        let mut b = view(&["$ ls", "a b"]);
        assert_eq!(feed(&mut b, "dd"), vec![]);
        assert_eq!(b.lines.len(), 2);
        assert!(b.message.as_deref().unwrap().starts_with("E21"));
        assert_eq!(feed(&mut b, "a"), vec![Effect::Insert]);
        assert_eq!(feed(&mut b, "-"), vec![Effect::Parent]);
        assert_eq!(feed(&mut b, "3K"), vec![Effect::Switch(-3)]);
        assert_eq!(feed(&mut b, "\x1e"), vec![Effect::Alternate]);
        assert_eq!(feed(&mut b, "yy"), vec![Effect::Yank("$ ls\n".into())]);
        assert_eq!(feed(&mut b, ":x\r"), vec![Effect::Quit { force: false }]);
    }

    #[test]
    fn search_and_repeat() {
        let mut b = view(&["foo", "bar foo", "foo"]);
        feed(&mut b, "/foo\r");
        assert_eq!(b.cursor, (1, 4));
        feed(&mut b, "n");
        assert_eq!(b.cursor, (2, 0));
        feed(&mut b, "n");
        assert_eq!(b.cursor, (0, 0));
        assert!(b.message.as_deref().unwrap().contains("BOTTOM"));
        feed(&mut b, "N");
        assert_eq!(b.cursor, (2, 0));
        feed(&mut b, "?bar\r");
        assert_eq!(b.cursor, (1, 0));
        feed(&mut b, "/zzz\r");
        assert!(b.message.as_deref().unwrap().starts_with("E486"));
    }

    #[test]
    fn find_and_repeat() {
        let mut b = buf(&["a,b,c,d"]);
        feed(&mut b, "f,");
        assert_eq!(b.cursor, (0, 1));
        feed(&mut b, ";;");
        assert_eq!(b.cursor, (0, 5));
        feed(&mut b, ",");
        assert_eq!(b.cursor, (0, 3));
        feed(&mut b, "0dt,");
        assert_eq!(b.lines[0].text, ",b,c,d");
    }

    #[test]
    fn scrolling_keeps_cursor_on_screen() {
        let lines: Vec<String> = (0..100).map(|i| i.to_string()).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut b = view(&refs);
        b.height = 10;
        feed(&mut b, "G");
        assert_eq!((b.top, b.cursor.0), (90, 99));
        feed(&mut b, "\x15");
        assert_eq!((b.top, b.cursor.0), (85, 94));
        feed(&mut b, "H");
        assert_eq!(b.cursor.0, 85);
        feed(&mut b, "L");
        assert_eq!(b.cursor.0, 94);
        feed(&mut b, "\x19");
        assert_eq!((b.top, b.cursor.0), (84, 93));
        feed(&mut b, "gg\x05");
        assert_eq!((b.top, b.cursor.0), (1, 1));
        feed(&mut b, ":50\r");
        assert_eq!(b.cursor.0, 49);
    }

    #[test]
    fn scroll_keeps_row_visible() {
        assert_eq!(scroll(0, 3, 10), 0);
        assert_eq!(scroll(0, 10, 10), 1);
        assert_eq!(scroll(5, 2, 10), 2);
        assert_eq!(scroll(5, 14, 10), 5);
    }

    #[test]
    fn remove_id_keeps_edits() {
        let mut b = buf(&["foo", "bar"]);
        feed(&mut b, "oweb\x1b");
        b.remove_id(0);
        assert_eq!(ids(&b), vec![(None, "web"), (Some(1), "bar")]);
        assert!(b.modified());
    }
}
