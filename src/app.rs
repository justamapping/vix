use std::collections::HashMap;
use std::io::{self, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::DefaultTerminal;

use crate::buffer::{self, Buffer, Effect, Line};
use crate::carry;
use crate::config::Config;
use crate::dump;
use crate::follow;
use crate::input::{self, Input, Key, Mouse, MouseKind};
use crate::listdiff::{self, Op};
use crate::motion::Pos;
use crate::picker::{Item, Pick, Picker};
use crate::proc;
use crate::pty::Pty;
use crate::render::{self, Bar};
use crate::session::{Saved, Session};
use crate::state::{self, View};
use crate::status::{Entry, Info};
use crate::vt::{self, Modes};

pub enum Event {
    Input(Vec<u8>),
    Output(u64, Vec<u8>),
    Exited(u64),
    Hangup,
    Resize,
}

// output this soon after a spawn or resize is the program starting or redrawing, not activity
const SPAWN_QUIET: Duration = Duration::from_millis(1000);
const RESIZE_QUIET: Duration = Duration::from_millis(250);

pub struct Term {
    id: u64,
    name: String,
    cwd: PathBuf,
    pty: Pty,
    vt: vt::Parser,
    /// output or a bell since it was last viewed
    activity: bool,
    bell: bool,
    quiet: Instant,
}

impl Term {
    /// Where the shell is now, falling back to where it started.
    fn cwd(&self) -> PathBuf {
        self.pty.pid().and_then(proc::cwd).unwrap_or_else(|| self.cwd.clone())
    }
}

pub struct App {
    terms: Vec<Term>,
    list: Buffer,
    /// view n's read-only buffer over the viewed terminal's text
    normal: Buffer,
    view: View,
    next_id: u64,
    cols: u16,
    rows: u16,
    outer: Option<Modes>,
    cwd: PathBuf,
    tx: Sender<Event>,
    /// view n was just entered with `<C-\>`, so another one sends it literally
    fresh: bool,
    /// view n was entered with the wheel, so scrolling back to the bottom returns to view i
    wheeled: bool,
    /// where the left button went down in view i, for a drag that turns into a view n selection
    press: Option<(u16, u16)>,
    /// view n's text is behind the terminal
    stale: bool,
    last: Option<u64>,
    alt: Option<u64>,
    /// list row to open once the user answers the write prompt
    confirm: Option<usize>,
    /// `<C-p>`'s fuzzy finder, drawn over the terminal it has selected
    picker: Option<Picker>,
    config: Config,
    /// when a partly typed mapping gives up waiting
    pub deadline: Option<Instant>,
    pub quit: bool,
}

/// Forwards everything `src` reads as events until EOF.
pub fn pump(mut src: impl Read + Send + 'static, tx: Sender<Event>, wrap: impl Fn(Vec<u8>) -> Event + Send + 'static, end: Event) {
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match src.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(wrap(buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let _ = tx.send(end);
    });
}

impl App {
    pub fn new(tx: Sender<Event>, cols: u16, rows: u16, config: Config, session: Session) -> Result<Self> {
        let cwd = std::env::current_dir()?;
        let mut normal = Buffer::new(Vec::new());
        normal.readonly = true;
        normal.maps = config.view.clone();
        let mut list = Buffer::new(Vec::new());
        list.maps = config.list.clone();
        list.insert_maps = config.list_insert.clone();
        let mut app = Self {
            terms: Vec::new(),
            list,
            normal,
            view: View::List,
            next_id: 0,
            cols,
            rows,
            outer: None,
            cwd: cwd.clone(),
            tx,
            fresh: false,
            wheeled: false,
            press: None,
            stale: false,
            last: None,
            alt: None,
            confirm: None,
            picker: None,
            config,
            deadline: None,
            quit: false,
        };
        app.size_buffers();
        let mut saved = session.terms;
        if saved.is_empty() {
            saved.push(Saved { name: "Untitled".into(), cwd: cwd.clone(), output: Vec::new() });
        }
        for Saved { name, cwd: dir, output } in saved {
            let dir = if dir.is_dir() { dir } else { cwd.clone() };
            let mut t = app.spawn(name, dir)?;
            dump::restore(&mut t.vt, &output);
            app.terms.push(t);
        }
        app.list.load(app.lines());
        app.list.place((session.cursor, 0), 0);
        Ok(app)
    }

    /// The list as `--resume` will bring it back.
    pub fn snapshot(&mut self) -> Session {
        let terms = self.terms.iter_mut().map(|t| Saved { name: t.name.clone(), cwd: t.cwd(), output: dump::dump(&mut t.vt) }).collect();
        let at = self.viewing().or(self.last).and_then(|id| self.terms.iter().position(|t| t.id == id));
        Session { cursor: at.unwrap_or(self.list.cursor.0), terms }
    }

    pub fn message(&mut self, msg: String) {
        self.list.message = Some(msg);
    }

    /// The bar takes a row from every terminal, so ptys keep their size across views.
    fn term_rows(&self) -> u16 {
        if self.config.status.enabled { self.rows.saturating_sub(1).max(1) } else { self.rows }
    }

    fn size_buffers(&mut self) {
        self.list.height = self.rows.saturating_sub(1).max(1) as usize;
        self.normal.height = self.term_rows().max(1) as usize;
    }

    fn spawn(&mut self, name: String, cwd: PathBuf) -> Result<Term> {
        let id = self.next_id;
        self.next_id += 1;
        let rows = self.term_rows();
        let pty = Pty::spawn(self.cols, rows, &cwd)?;
        pump(pty.reader()?, self.tx.clone(), move |b| Event::Output(id, b), Event::Exited(id));
        let vt = vt::parser(self.cols, rows);
        Ok(Term { id, name, cwd, pty, vt, activity: false, bell: false, quiet: Instant::now() + SPAWN_QUIET })
    }

    fn lines(&self) -> Vec<Line> {
        self.terms.iter().map(|t| Line { id: Some(t.id), text: t.name.clone() }).collect()
    }

    fn term(&mut self, id: u64) -> Option<&mut Term> {
        self.terms.iter_mut().find(|t| t.id == id)
    }

    fn alive(&self, id: u64) -> bool {
        self.terms.iter().any(|t| t.id == id)
    }

    fn viewing(&self) -> Option<u64> {
        match self.view {
            View::Insert(id) | View::Normal(id) => Some(id),
            View::List => None,
        }
    }

    /// Switches view, remembering the previous terminal for `<C-^>`.
    fn set_view(&mut self, view: View) {
        self.view = view;
        self.wheeled = false;
        let Some(id) = self.viewing() else { return };
        if self.last != Some(id) {
            self.alt = self.last.replace(id);
        }
        if let Some(t) = self.term(id) {
            (t.activity, t.bell) = (false, false);
        }
    }

    fn go_list(&mut self, id: u64) {
        self.view = View::List;
        if let Some(row) = self.list.lines.iter().position(|l| l.id == Some(id)) {
            self.list.place((row, 0), self.list.top);
        }
    }

    /// Enters view n on `id` with the cursor where the program's cursor is.
    fn enter_normal(&mut self, id: u64) {
        let Some(t) = self.term(id) else { return };
        let screen = t.vt.screen_mut();
        let live = follow::live(screen);
        let lines = vt::text(screen).into_iter().map(|text| Line { id: None, text }).collect();
        self.normal.load(lines);
        self.normal.message = None;
        self.normal.place(live.cursor, live.bottom);
        self.stale = false;
        self.set_view(View::Normal(id));
    }

    fn refresh_normal(&mut self) {
        let View::Normal(id) = self.view else { return };
        if !self.stale {
            return;
        }
        self.stale = false;
        let Some(t) = self.term(id) else { return };
        let lines = vt::text(t.vt.screen_mut()).into_iter().map(|text| Line { id: None, text }).collect();
        self.normal.refresh(lines);
    }

    pub fn kill_all(&mut self) {
        for t in &mut self.terms {
            t.pty.kill();
        }
    }

    /// Forgets a terminal that exited or was killed; its lines leave the list even if it has edits.
    fn remove(&mut self, id: u64) -> Option<Term> {
        let i = self.terms.iter().position(|t| t.id == id)?;
        let t = self.terms.remove(i);
        if let Some(p) = &mut self.picker {
            p.remove(id);
        }
        if self.viewing() == Some(id) {
            self.go_list(id);
        }
        if self.list.modified() {
            self.list.remove_id(id);
        } else {
            self.list.load(self.lines());
        }
        Some(t)
    }

    /// Applies one event. Returns whether the screen needs a redraw.
    pub fn handle(&mut self, event: Event) -> Result<bool> {
        match event {
            Event::Output(id, bytes) => {
                let normal = self.view == View::Normal(id);
                let viewed = self.viewing() == Some(id);
                let Some(t) = self.term(id) else { return Ok(false) };
                let before = follow::live(t.vt.screen_mut());
                t.vt.process(&bytes);
                let after = follow::live(t.vt.screen_mut());
                let replies = std::mem::take(&mut t.vt.callbacks_mut().replies);
                if !replies.is_empty() {
                    t.pty.write(&replies)?;
                }
                let flags = (t.activity, t.bell);
                let bell = std::mem::take(&mut t.vt.callbacks_mut().bell);
                if !viewed {
                    t.bell |= bell;
                    t.activity |= Instant::now() >= t.quiet;
                }
                let flagged = (t.activity, t.bell) != flags;
                if normal {
                    self.stale = true;
                    if self.normal.mode == buffer::Mode::Normal {
                        (self.normal.cursor, self.normal.top) = follow::follow(self.normal.cursor, self.normal.top, before, after);
                    }
                }
                Ok(viewed || (flagged && self.config.status.enabled))
            }
            Event::Exited(id) => {
                let Some(t) = self.remove(id) else { return Ok(false) };
                self.list.message = Some(format!("{} exited", t.name));
                Ok(true)
            }
            Event::Hangup => {
                self.quit = true;
                Ok(false)
            }
            Event::Resize => {
                crate::log::log("resize");
                let (cols, rows) = crossterm::terminal::size()?;
                (self.cols, self.rows) = (cols, rows);
                self.size_buffers();
                let rows = self.term_rows();
                for t in &mut self.terms {
                    t.pty.resize(cols, rows)?;
                    t.vt.screen_mut().set_size(rows, cols);
                    t.quiet = t.quiet.max(Instant::now() + RESIZE_QUIET);
                }
                if let View::Normal(id) = self.view {
                    self.enter_normal(id);
                }
                Ok(true)
            }
            Event::Input(bytes) => {
                crate::log::log(format_args!(
                    "input {:?} view={:?} normal={:?} list={:?}",
                    String::from_utf8_lossy(&bytes),
                    self.view,
                    self.normal.mode,
                    self.list.mode
                ));
                for input in input::split(&bytes, self.config.escape) {
                    self.input(input)?;
                }
                let waiting = match self.view {
                    _ if self.picker.is_some() => false,
                    View::List => self.list.waiting(),
                    View::Normal(_) => self.normal.waiting(),
                    View::Insert(_) => false,
                };
                self.deadline = waiting.then(|| Instant::now() + self.config.timeout);
                Ok(true)
            }
        }
    }

    /// The mapping timeout passed: run the keys it was holding.
    pub fn flush(&mut self) -> Result<()> {
        self.deadline = None;
        match self.view {
            View::List => {
                let effects = self.list.flush();
                self.list_effects(effects)
            }
            View::Normal(id) => {
                let effects = self.normal.flush();
                self.normal_effects(id, effects)
            }
            View::Insert(_) => Ok(()),
        }
    }

    fn input(&mut self, input: Input) -> Result<()> {
        if self.picker.is_some() {
            return self.picker_input(input);
        }
        let bytes = match (self.view, input) {
            (_, Input::Mouse(m)) => return self.mouse(m),
            (View::Insert(id), input) => {
                let (next, out) = state::step(self.view, input, self.config.escape);
                if let Some(t) = self.term(id)
                    && !out.is_empty()
                {
                    t.pty.write(&out)?;
                }
                if next == View::Normal(id) {
                    self.enter_normal(id);
                    self.fresh = true;
                }
                return Ok(());
            }
            (View::Normal(id), Input::Escape) => {
                if std::mem::take(&mut self.fresh) {
                    let (next, out) = state::step(self.view, Input::Escape, self.config.escape);
                    if let Some(t) = self.term(id) {
                        t.pty.write(&out)?;
                    }
                    self.set_view(next);
                }
                return Ok(());
            }
            (View::List, Input::Escape) => return Ok(()),
            (_, Input::Bytes(b)) => b,
        };
        // keys go to the list or view n until one of them changes view; the rest go to the new one
        let before = self.view;
        for (key, end) in input::spans(&bytes) {
            match self.view {
                View::List => self.list_key(key)?,
                View::Normal(id) => self.normal_key(id, key)?,
                View::Insert(_) => unreachable!("insert view is handled above"),
            }
            if self.view != before {
                if end < bytes.len() {
                    self.input(Input::Bytes(bytes[end..].to_vec()))?;
                }
                break;
            }
        }
        Ok(())
    }

    /// Opens the picker, the alternate terminal first so `<C-p><CR>` acts like `<C-^>`.
    fn pick(&mut self) {
        let alternate = if self.viewing().is_some() { self.alt } else { self.last };
        let mut items: Vec<Item> = self
            .terms
            .iter()
            .enumerate()
            .map(|(i, t)| Item { id: t.id, index: i + 1, name: t.name.clone(), cwd: tilde(&t.cwd()) })
            .collect();
        if let Some(i) = items.iter().position(|item| Some(item.id) == alternate) {
            let item = items.remove(i);
            items.insert(0, item);
        }
        self.picker = Some(Picker::new(items));
    }

    fn picker_input(&mut self, input: Input) -> Result<()> {
        let bytes = match input {
            Input::Bytes(b) => b,
            Input::Escape => {
                self.picker = None;
                return Ok(());
            }
            Input::Mouse(_) => return Ok(()),
        };
        for (key, end) in input::spans(&bytes) {
            let Some(pick) = self.picker.as_mut().and_then(|p| p.key(key)) else { continue };
            self.picker = None;
            if let Pick::Open(id) = pick {
                self.enter_normal(id);
            }
            if end < bytes.len() {
                self.input(Input::Bytes(bytes[end..].to_vec()))?;
            }
            break;
        }
        Ok(())
    }

    fn normal_key(&mut self, id: u64, key: Key) -> Result<()> {
        self.fresh = false;
        self.wheeled = false;
        self.refresh_normal();
        let effects = self.normal.key(key);
        crate::log::log(format_args!("  normal {key:?} -> {:?} {effects:?}", self.normal.mode));
        self.normal_effects(id, effects)
    }

    fn normal_effects(&mut self, id: u64, effects: Vec<Effect>) -> Result<()> {
        for effect in effects {
            match effect {
                Effect::Insert(at) => {
                    if let Some(at) = at {
                        self.carry(id, at)?;
                    }
                    self.set_view(View::Insert(id));
                }
                Effect::Parent | Effect::Quit { force: false } => self.go_list(id),
                Effect::Quit { force: true } => {
                    if let Some(mut t) = self.remove(id) {
                        t.pty.kill();
                        self.list.message = Some(format!("{} killed", t.name));
                    }
                }
                Effect::QuitAll { force } => self.quit_all(force, true),
                Effect::Switch(n) => {
                    let i = self.terms.iter().position(|t| t.id == id).unwrap_or(0) as isize;
                    let j = (i + n).clamp(0, self.terms.len() as isize - 1) as usize;
                    let next = self.terms[j].id;
                    if next != id {
                        self.enter_normal(next);
                    }
                }
                Effect::Alternate => match self.alt.filter(|a| self.alive(*a)) {
                    Some(alt) => self.enter_normal(alt),
                    None => self.normal.message = Some("E23: No alternate file".into()),
                },
                Effect::Yank(text) => {
                    let mut out = io::stdout();
                    out.write_all(&vt::osc52(&text))?;
                    out.flush()?;
                }
                Effect::Reload => {
                    self.stale = true;
                    self.refresh_normal();
                }
                Effect::Edit { name, new } => self.edit(name, new, Some(id)),
                Effect::Goto(n) => match state::goto(n, self.terms.len()) {
                    Some(i) => self.enter_normal(self.terms[i].id),
                    None => self.normal.message = Some(format!("no terminal {n}")),
                },
                Effect::Pick => self.pick(),
                Effect::Delete(name) => self.delete(id, name),
                Effect::Rename(name) => self.rename(id, name),
                Effect::Open(_) | Effect::Write => {}
            }
        }
        Ok(())
    }

    /// Moves a line editor's cursor to where `i a I A` asked, when that's on its line; anywhere else it stays put.
    fn carry(&mut self, id: u64, (row, index): Pos) -> Result<()> {
        let Some(t) = self.term(id) else { return Ok(()) };
        if !t.pty.raw() {
            return Ok(());
        }
        let Some(row) = row.checked_sub(vt::history(t.vt.screen_mut())) else { return Ok(()) };
        if let Some(keys) = carry::arrows(t.vt.screen(), row as u16, index) {
            t.pty.write(&keys)?;
        }
        Ok(())
    }

    /// `:d [name]` from view n on `id`. Deleting the viewed terminal moves on to the alternate or a neighbor, like `:bd`.
    fn delete(&mut self, id: u64, name: Option<String>) {
        let target = match &name {
            None => id,
            Some(name) => match self.terms.iter().find(|t| &t.name == name) {
                Some(t) => t.id,
                None => {
                    self.normal.message = Some(format!("E94: No matching terminal for {name}"));
                    return;
                }
            },
        };
        let next = if target == id { self.successor(id) } else { Some(id) };
        let Some(mut t) = self.remove(target) else { return };
        t.pty.kill();
        let msg = format!("{} killed", t.name);
        match next {
            Some(next) if next != id => {
                self.enter_normal(next);
                self.normal.message = Some(msg);
            }
            Some(_) => self.normal.message = Some(msg),
            None => self.list.message = Some(msg),
        }
    }

    /// Where view n goes when `id` is deleted: the alternate, else the next terminal, else the previous.
    fn successor(&self, id: u64) -> Option<u64> {
        if let Some(alt) = self.alt.filter(|a| *a != id && self.alive(*a)) {
            return Some(alt);
        }
        let i = self.terms.iter().position(|t| t.id == id)?;
        self.terms.get(i + 1).or(i.checked_sub(1).and_then(|j| self.terms.get(j))).map(|t| t.id)
    }

    /// `:r name` from view n on `id`.
    fn rename(&mut self, id: u64, name: String) {
        let Some(t) = self.term(id) else { return };
        let old = std::mem::replace(&mut t.name, name.clone());
        self.list.rename_id(id, &old, &name);
    }

    /// The program in view i asked for the mouse, so events go to it as they are.
    fn program_mouse(&self, id: u64) -> bool {
        !self.config.mouse || self.terms.iter().any(|t| t.id == id && vt::modes(&t.vt).mouse != 0)
    }

    fn mouse(&mut self, m: Mouse) -> Result<()> {
        let kind = m.kind();
        match self.view {
            View::Insert(id) if self.program_mouse(id) => {
                if let Some(t) = self.term(id) {
                    t.pty.write(&m.encode())?;
                }
            }
            View::Insert(id) => self.insert_mouse(id, m, kind)?,
            View::Normal(id) => match kind {
                MouseKind::WheelUp | MouseKind::WheelDown => {
                    self.refresh_normal();
                    let down = kind == MouseKind::WheelDown;
                    self.normal.wheel(down);
                    if down && self.wheeled && self.normal.at_bottom() {
                        self.set_view(View::Insert(id));
                    }
                }
                MouseKind::Press | MouseKind::Drag => {
                    self.refresh_normal();
                    let at = cell_pos(&self.normal, m.cell());
                    self.normal.click(at, kind == MouseKind::Drag);
                }
                MouseKind::Other => {}
            },
            View::List if self.confirm.is_some() => {}
            View::List => match kind {
                MouseKind::WheelUp | MouseKind::WheelDown => self.list.wheel(kind == MouseKind::WheelDown),
                MouseKind::Press | MouseKind::Drag => {
                    let at = cell_pos(&self.list, m.cell());
                    self.list.click(at, kind == MouseKind::Drag);
                }
                MouseKind::Other => {}
            },
        }
        Ok(())
    }

    /// View i with a program that left the mouse alone: the wheel scrolls like a terminal would, a drag selects.
    fn insert_mouse(&mut self, id: u64, m: Mouse, kind: MouseKind) -> Result<()> {
        let Some(t) = self.term(id) else { return Ok(()) };
        let up = kind == MouseKind::WheelUp;
        match kind {
            // full-screen programs get arrow keys, as the outer terminal's alternate scroll would send
            MouseKind::WheelUp | MouseKind::WheelDown if t.vt.screen().alternate_screen() => {
                let arrow = match (up, t.vt.screen().application_cursor()) {
                    (true, false) => "\x1b[A",
                    (false, false) => "\x1b[B",
                    (true, true) => "\x1bOA",
                    (false, true) => "\x1bOB",
                };
                t.pty.write(arrow.repeat(buffer::WHEEL).as_bytes())?;
            }
            MouseKind::WheelUp if vt::history(t.vt.screen_mut()) > 0 => {
                self.enter_normal(id);
                self.normal.wheel(false);
                self.wheeled = true;
            }
            MouseKind::Press => self.press = Some(m.cell()),
            MouseKind::Drag => {
                let from = self.press.take().unwrap_or(m.cell());
                self.enter_normal(id);
                let from = cell_pos(&self.normal, from);
                self.normal.click(from, false);
                self.normal.click(cell_pos(&self.normal, m.cell()), true);
            }
            _ => {}
        }
        Ok(())
    }

    fn quit_all(&mut self, force: bool, from_normal: bool) {
        if force || !self.list.modified() {
            self.quit = true;
            return;
        }
        let msg = Some("E37: No write since last change (add ! to override)".into());
        if from_normal { self.normal.message = msg } else { self.list.message = msg }
    }

    fn list_key(&mut self, key: Key) -> Result<()> {
        if let Some(row) = self.confirm.take() {
            self.list.message = None;
            if key == Key::Char('y') {
                let ids = self.write();
                self.open(ids.get(row).copied().flatten());
            }
            return Ok(());
        }
        let effects = self.list.key(key);
        self.list_effects(effects)
    }

    fn list_effects(&mut self, effects: Vec<Effect>) -> Result<()> {
        for effect in effects {
            match effect {
                Effect::Open(row) => {
                    let summary = self.pending_summary();
                    if summary.is_empty() {
                        let ids = if self.list.modified() { self.write() } else { self.list.lines.iter().map(|l| l.id).collect() };
                        self.open(ids.get(row).copied().flatten());
                    } else {
                        self.list.message = Some(format!("{summary}. write? [y/n]"));
                        self.confirm = Some(row);
                    }
                }
                Effect::Write => {
                    self.write();
                }
                Effect::Quit { .. } => self.quit = true,
                Effect::QuitAll { force } => self.quit_all(force, false),
                Effect::Reload => self.list.load(self.lines()),
                Effect::Edit { name, new } => self.edit(name, new, None),
                Effect::Pick => self.pick(),
                _ => {}
            }
        }
        Ok(())
    }

    /// `:e name` / `:new name`: views the terminal called `name`, or spawns it at the end of the list in `from`'s directory.
    fn edit(&mut self, name: String, new: bool, from: Option<u64>) {
        if let Some(id) = self.terms.iter().find(|t| !new && t.name == name).map(|t| t.id) {
            self.set_view(View::Insert(id));
            return;
        }
        let cwd = self.terms.iter().find(|t| Some(t.id) == from).map_or(self.cwd.clone(), Term::cwd);
        match self.spawn(name, cwd) {
            Ok(t) => {
                let line = Line { id: Some(t.id), text: t.name.clone() };
                let id = t.id;
                self.terms.push(t);
                if self.list.modified() {
                    self.list.push_saved(line);
                } else {
                    self.list.load(self.lines());
                }
                self.set_view(View::Insert(id));
            }
            Err(e) => {
                let buf = if from.is_some() { &mut self.normal } else { &mut self.list };
                buf.message = Some(format!("spawn failed: {e}"));
            }
        }
    }

    fn open(&mut self, id: Option<u64>) {
        match id.filter(|id| self.alive(*id)) {
            Some(id) => self.set_view(View::Insert(id)),
            None => self.list.message = Some("nothing to open on this line".into()),
        }
    }

    fn pending_summary(&self) -> String {
        if !self.list.modified() {
            return String::new();
        }
        let old: Vec<(u64, &str)> = self.terms.iter().map(|t| (t.id, t.name.as_str())).collect();
        let ids: Vec<u64> = old.iter().map(|(id, _)| *id).collect();
        listdiff::summary(&old, &listdiff::diff(&ids, &self.list.lines))
    }

    /// `:w`: reconcile running terminals with the list buffer. Returns the id each old list line ended up with.
    fn write(&mut self) -> Vec<Option<u64>> {
        let old: Vec<u64> = self.terms.iter().map(|t| t.id).collect();
        let ops = listdiff::diff(&old, &self.list.lines);
        let mut pool: HashMap<u64, Term> = self.terms.drain(..).map(|t| (t.id, t)).collect();
        let mut next: Vec<Term> = Vec::new();
        let mut placed: Vec<Option<u64>> = Vec::new();
        let mut errors = Vec::new();
        for op in ops {
            let spawned = match op {
                Op::Keep { id, name } => {
                    let mut t = pool.remove(&id).expect("diff only keeps known ids");
                    t.name = name;
                    placed.push(Some(id));
                    next.push(t);
                    continue;
                }
                Op::Kill { id } => {
                    if let Some(mut t) = pool.remove(&id) {
                        t.pty.kill();
                    }
                    continue;
                }
                Op::Clone { from, name } => {
                    let cwd = next.iter().find(|t| t.id == from).map_or(self.cwd.clone(), Term::cwd);
                    self.spawn(name, cwd)
                }
                Op::Spawn { name } => self.spawn(name, self.cwd.clone()),
            };
            match spawned {
                Ok(t) => {
                    placed.push(Some(t.id));
                    next.push(t);
                }
                Err(e) => {
                    placed.push(None);
                    errors.push(e);
                }
            }
        }
        // ops came in non-blank line order
        let mut placed = placed.into_iter();
        let ids = self.list.lines.iter().map(|l| if l.text.trim().is_empty() { None } else { placed.next().flatten() }).collect();
        self.terms = next;
        self.list.load(self.lines());
        if let Some(e) = errors.first() {
            self.list.message = Some(format!("spawn failed: {e}"));
        }
        ids
    }

    /// The bottom row's status. Without the bar the list still shows its mode and `[+]`, as before.
    fn bar(&self) -> Option<Bar> {
        let status = &self.config.status;
        if !status.enabled {
            let modified = if self.list.modified() { "[+]" } else { "" };
            return (self.view == View::List).then(|| Bar { left: self.list.mode.label().into(), right: modified.into() });
        }
        let viewing = self.viewing();
        let alternate = if viewing.is_some() { self.alt } else { self.last };
        let terms = self
            .terms
            .iter()
            .map(|t| Entry {
                name: &t.name,
                current: viewing == Some(t.id),
                alternate: alternate == Some(t.id),
                activity: t.activity,
                bell: t.bell,
            })
            .collect();
        let mode = match self.view {
            View::List => self.list.mode.label(),
            View::Normal(_) => self.normal.mode.label(),
            View::Insert(_) => "-- TERMINAL --",
        };
        let info = Info { mode, modified: self.list.modified(), terms };
        Some(Bar { left: status.left.expand(&info), right: status.right.expand(&info) })
    }

    /// Button and drag events in SGR encoding, when vix has the mouse.
    fn mouse_modes(&self, modes: Modes) -> Modes {
        if self.config.mouse { Modes { mouse: 1002, mouse_encoding: 1006, ..modes } } else { modes }
    }

    pub fn draw(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        if self.viewing().is_some_and(|id| !self.alive(id)) {
            self.view = View::List;
        }
        self.refresh_normal();
        let bar = self.bar();
        if let Some(p) = &self.picker {
            // typing keeps what you were looking at; moving the selection previews it
            let shown = if p.browsing { p.selection() } else { self.viewing() };
            let preview = shown.and_then(|id| self.terms.iter().find(|t| t.id == id));
            terminal.draw(|f| {
                match (preview, &bar) {
                    (Some(t), bar) => render::term(f, t.vt.screen(), None, bar.as_ref()),
                    (None, Some(bar)) if self.view == View::List => render::list(f, &self.list, bar),
                    _ => {}
                }
                render::picker(f, p);
            })?;
            let modes = self.mouse_modes(Modes { cursor_shape: 6, ..Default::default() });
            return self.set_outer(modes);
        }
        let modes = match self.view {
            View::List => {
                let insert = self.list.mode == buffer::Mode::Insert;
                let bar = bar.as_ref().expect("the list always has a bar");
                terminal.draw(|f| render::list(f, &self.list, bar))?;
                self.mouse_modes(Modes { cursor_shape: if insert { 6 } else { 0 }, ..Default::default() })
            }
            View::Normal(id) => {
                let top = self.normal.top;
                let t = self.terms.iter_mut().find(|t| t.id == id).expect("checked above");
                let screen = t.vt.screen_mut();
                let total = vt::history(screen);
                screen.set_scrollback(total.saturating_sub(top));
                let normal = &self.normal;
                let drawn = terminal.draw(|f| render::term(f, screen, Some(normal), bar.as_ref()));
                screen.set_scrollback(0);
                drawn?;
                self.mouse_modes(Modes::default())
            }
            View::Insert(id) => {
                let t = self.terms.iter().find(|t| t.id == id).expect("checked above");
                terminal.draw(|f| render::term(f, t.vt.screen(), None, bar.as_ref()))?;
                let modes = vt::modes(&t.vt);
                if modes.mouse == 0 { self.mouse_modes(modes) } else { modes }
            }
        };
        self.set_outer(modes)
    }

    fn set_outer(&mut self, modes: Modes) -> Result<()> {
        if self.outer != Some(modes) {
            let mut out = io::stdout();
            out.write_all(&vt::mode_bytes(modes))?;
            out.flush()?;
            self.outer = Some(modes);
        }
        Ok(())
    }
}

/// `path` with the home directory as `~`.
fn tilde(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home.as_deref().and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// Buffer position under screen cell (x, y), held to the visible lines.
fn cell_pos(buf: &Buffer, (x, y): (u16, u16)) -> Pos {
    let y = (y as usize).min(buf.height.max(1) - 1);
    let row = (buf.top + y).min(buf.lines.len() - 1);
    (row, render::col_at(&buf.lines[row].text, x))
}
