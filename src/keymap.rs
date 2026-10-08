use crate::input::Key;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordFwd,
    WordBack,
    WordEnd,
    BigWordFwd,
    BigWordBack,
    BigWordEnd,
    LineStart,
    FirstNonBlank,
    LineEnd,
    FirstLine,
    LastLine,
}

impl Motion {
    pub fn linewise(self) -> bool {
        matches!(self, Motion::Up | Motion::Down | Motion::FirstLine | Motion::LastLine)
    }

    pub fn inclusive(self) -> bool {
        matches!(self, Motion::LineEnd | Motion::WordEnd | Motion::BigWordEnd)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Operator {
    Delete,
    Yank,
    Change,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target {
    Motion(Motion),
    Line,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InsertAt {
    Before,
    After,
    LineStart,
    LineEnd,
    Below,
    Above,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    Move(Motion),
    Operate(Operator, Target),
    Insert(InsertAt),
    Paste { before: bool },
    Undo,
    Redo,
    Open,
    Command,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cmd {
    pub count: Option<usize>,
    pub action: Action,
}

#[derive(Debug, PartialEq)]
pub enum Parsed {
    Done(Cmd),
    Pending,
    Invalid,
}

/// Parses a pending normal-mode key sequence: `[count] cmd`, `[count] op [count] motion`.
pub fn parse(keys: &[Key]) -> Parsed {
    let (count, rest) = count(keys);
    let Some(&first) = rest.first() else {
        return Parsed::Pending;
    };
    let done = |action| Parsed::Done(Cmd { count, action });
    let op = match first {
        Key::Char('d') => Some(Operator::Delete),
        Key::Char('y') => Some(Operator::Yank),
        Key::Char('c') => Some(Operator::Change),
        _ => None,
    };
    if let Some(op) = op {
        let (count2, rest) = self::count(&rest[1..]);
        let count = match (count, count2) {
            (Some(a), Some(b)) => Some(a * b),
            (a, b) => a.or(b),
        };
        return match rest.first() {
            None => Parsed::Pending,
            Some(&k) if k == first => Parsed::Done(Cmd { count, action: Action::Operate(op, Target::Line) }),
            Some(_) => match motion(rest) {
                Parsed::Done(Cmd { action: Action::Move(m), .. }) => {
                    Parsed::Done(Cmd { count, action: Action::Operate(op, Target::Motion(m)) })
                }
                other => other,
            },
        };
    }
    let operate = |op, m| done(Action::Operate(op, Target::Motion(m)));
    match first {
        Key::Char('x') => operate(Operator::Delete, Motion::Right),
        Key::Char('X') => operate(Operator::Delete, Motion::Left),
        Key::Char('D') => operate(Operator::Delete, Motion::LineEnd),
        Key::Char('C') => operate(Operator::Change, Motion::LineEnd),
        Key::Char('s') => operate(Operator::Change, Motion::Right),
        Key::Char('S') => done(Action::Operate(Operator::Change, Target::Line)),
        Key::Char('Y') => done(Action::Operate(Operator::Yank, Target::Line)),
        Key::Char('i') => done(Action::Insert(InsertAt::Before)),
        Key::Char('a') => done(Action::Insert(InsertAt::After)),
        Key::Char('I') => done(Action::Insert(InsertAt::LineStart)),
        Key::Char('A') => done(Action::Insert(InsertAt::LineEnd)),
        Key::Char('o') => done(Action::Insert(InsertAt::Below)),
        Key::Char('O') => done(Action::Insert(InsertAt::Above)),
        Key::Char('p') => done(Action::Paste { before: false }),
        Key::Char('P') => done(Action::Paste { before: true }),
        Key::Char('u') => done(Action::Undo),
        Key::Ctrl('r') => done(Action::Redo),
        Key::Enter => done(Action::Open),
        Key::Char(':') => done(Action::Command),
        _ => match motion(rest) {
            Parsed::Done(cmd) => Parsed::Done(Cmd { count, ..cmd }),
            other => other,
        },
    }
}

fn count(keys: &[Key]) -> (Option<usize>, &[Key]) {
    let mut n: Option<usize> = None;
    let mut i = 0;
    while let Some(Key::Char(c @ '0'..='9')) = keys.get(i) {
        if *c == '0' && n.is_none() {
            break;
        }
        n = Some(n.unwrap_or(0).saturating_mul(10).saturating_add(*c as usize - '0' as usize));
        i += 1;
    }
    (n, &keys[i..])
}

fn motion(keys: &[Key]) -> Parsed {
    use Motion::*;
    let m = match keys[0] {
        Key::Char('h') | Key::Left | Key::Backspace => Left,
        Key::Char('l') | Key::Right | Key::Char(' ') => Right,
        Key::Char('j') | Key::Down => Down,
        Key::Char('k') | Key::Up => Up,
        Key::Char('w') => WordFwd,
        Key::Char('b') => WordBack,
        Key::Char('e') => WordEnd,
        Key::Char('W') => BigWordFwd,
        Key::Char('B') => BigWordBack,
        Key::Char('E') => BigWordEnd,
        Key::Char('0') => LineStart,
        Key::Char('^') => FirstNonBlank,
        Key::Char('$') => LineEnd,
        Key::Char('G') => LastLine,
        Key::Char('g') => match keys.get(1) {
            None => return Parsed::Pending,
            Some(Key::Char('g')) => FirstLine,
            Some(_) => return Parsed::Invalid,
        },
        _ => return Parsed::Invalid,
    };
    Parsed::Done(Cmd { count: None, action: Action::Move(m) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Parsed {
        let keys: Vec<Key> = s.chars().map(Key::Char).collect();
        parse(&keys)
    }

    fn done(count: Option<usize>, action: Action) -> Parsed {
        Parsed::Done(Cmd { count, action })
    }

    #[test]
    fn motions_with_counts() {
        assert_eq!(p("j"), done(None, Action::Move(Motion::Down)));
        assert_eq!(p("3j"), done(Some(3), Action::Move(Motion::Down)));
        assert_eq!(p("12G"), done(Some(12), Action::Move(Motion::LastLine)));
        assert_eq!(p("0"), done(None, Action::Move(Motion::LineStart)));
        assert_eq!(p("10"), Parsed::Pending);
    }

    #[test]
    fn gg_waits() {
        assert_eq!(p("g"), Parsed::Pending);
        assert_eq!(p("gg"), done(None, Action::Move(Motion::FirstLine)));
        assert_eq!(p("gx"), Parsed::Invalid);
    }

    #[test]
    fn operators() {
        let op = |o, t| Action::Operate(o, t);
        assert_eq!(p("d"), Parsed::Pending);
        assert_eq!(p("dd"), done(None, op(Operator::Delete, Target::Line)));
        assert_eq!(p("3yy"), done(Some(3), op(Operator::Yank, Target::Line)));
        assert_eq!(p("cw"), done(None, op(Operator::Change, Target::Motion(Motion::WordFwd))));
        assert_eq!(p("2d3w"), done(Some(6), op(Operator::Delete, Target::Motion(Motion::WordFwd))));
        assert_eq!(p("dgg"), done(None, op(Operator::Delete, Target::Motion(Motion::FirstLine))));
        assert_eq!(p("dy"), Parsed::Invalid);
        assert_eq!(p("x"), done(None, op(Operator::Delete, Target::Motion(Motion::Right))));
    }

    #[test]
    fn simple_commands() {
        assert_eq!(p("o"), done(None, Action::Insert(InsertAt::Below)));
        assert_eq!(p("2p"), done(Some(2), Action::Paste { before: false }));
        assert_eq!(p("u"), done(None, Action::Undo));
        assert_eq!(p(":"), done(None, Action::Command));
        assert_eq!(parse(&[Key::Enter]), done(None, Action::Open));
        assert_eq!(p("Z"), Parsed::Invalid);
    }
}
