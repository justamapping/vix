use std::collections::HashMap;
use std::io::{self, ErrorKind, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;

use anyhow::Result;
use ratatui::DefaultTerminal;

use crate::buffer::{self, Buffer, Effect, Line};
use crate::input::{self, Input, Key};
use crate::listdiff::{self, Op};
use crate::pty::Pty;
use crate::render;
use crate::state::{self, View};
use crate::vt::{self, Modes};

pub enum Event {
    Input(Vec<u8>),
    Output(u64, Vec<u8>),
    Exited(u64),
    Hangup,
    Resize,
}

pub struct Term {
    id: u64,
    name: String,
    cwd: PathBuf,
    pty: Pty,
    vt: vt::Parser,
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
    /// view n's text is behind the terminal
    stale: bool,
    last: Option<u64>,
    alt: Option<u64>,
    /// list row to open once the user answers the write prompt
    confirm: Option<usize>,
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
    pub fn new(tx: Sender<Event>, cols: u16, rows: u16) -> Result<Self> {
        let cwd = std::env::current_dir()?;
        let mut normal = Buffer::new(Vec::new());
        normal.readonly = true;
        let mut app = Self {
            terms: Vec::new(),
            list: Buffer::new(Vec::new()),
            normal,
            view: View::List,
            next_id: 0,
            cols,
            rows,
            outer: None,
            cwd: cwd.clone(),
            tx,
            fresh: false,
            stale: false,
            last: None,
            alt: None,
            confirm: None,
            quit: false,
        };
        app.size_buffers();
        let first = app.spawn("Untitled".into(), cwd)?;
        app.terms.push(first);
        app.list.load(app.lines());
        Ok(app)
    }

    fn size_buffers(&mut self) {
        self.list.height = self.rows.saturating_sub(1).max(1) as usize;
        self.normal.height = self.rows.max(1) as usize;
    }

    fn spawn(&mut self, name: String, cwd: PathBuf) -> Result<Term> {
        let id = self.next_id;
        self.next_id += 1;
        let pty = Pty::spawn(self.cols, self.rows, &cwd)?;
        pump(pty.reader()?, self.tx.clone(), move |b| Event::Output(id, b), Event::Exited(id));
        Ok(Term { id, name, cwd, pty, vt: vt::parser(self.cols, self.rows) })
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
        if let Some(id) = self.viewing()
            && self.last != Some(id)
        {
            self.alt = self.last.replace(id);
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
        let total = vt::history(screen);
        let (row, col) = screen.cursor_position();
        let lines = vt::text(screen).into_iter().map(|text| Line { id: None, text }).collect();
        self.normal.load(lines);
        self.normal.message = None;
        self.normal.place((total + row as usize, col as usize), total);
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
                let Some(t) = self.term(id) else { return Ok(false) };
                let screen = t.vt.screen_mut();
                let before = vt::history(screen);
                let rows = screen.size().0 as usize;
                t.vt.process(&bytes);
                let grew = vt::history(t.vt.screen_mut()).saturating_sub(before);
                let replies = std::mem::take(&mut t.vt.callbacks_mut().replies);
                if !replies.is_empty() {
                    t.pty.write(&replies)?;
                }
                // view n: a cursor on the last line follows output, anywhere else its text stays put
                if normal {
                    self.stale = true;
                    if self.normal.cursor.0 + 1 == before + rows {
                        self.normal.cursor.0 += grew;
                        self.normal.top += grew;
                    }
                }
                Ok(self.viewing() == Some(id))
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
                for t in &mut self.terms {
                    t.pty.resize(cols, rows)?;
                    t.vt.screen_mut().set_size(rows, cols);
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
                for input in input::split(&bytes) {
                    self.input(input)?;
                }
                Ok(true)
            }
        }
    }

    fn input(&mut self, input: Input) -> Result<()> {
        let bytes = match (self.view, input) {
            (View::Insert(id), input) => {
                let (next, out) = state::step(self.view, input);
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
                    let (next, out) = state::step(self.view, Input::Escape);
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

    fn normal_key(&mut self, id: u64, key: Key) -> Result<()> {
        self.fresh = false;
        self.refresh_normal();
        let effects = self.normal.key(key);
        crate::log::log(format_args!("  normal {key:?} -> {:?} {effects:?}", self.normal.mode));
        for effect in effects {
            match effect {
                Effect::Insert => self.set_view(View::Insert(id)),
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
                Effect::Open(_) | Effect::Write => {}
            }
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
        for effect in self.list.key(key) {
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
                _ => {}
            }
        }
        Ok(())
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
                    let cwd = next.iter().find(|t| t.id == from).map_or(self.cwd.clone(), |t| t.cwd.clone());
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

    pub fn draw(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        if self.viewing().is_some_and(|id| !self.alive(id)) {
            self.view = View::List;
        }
        self.refresh_normal();
        let modes = match self.view {
            View::List => {
                let insert = self.list.mode == buffer::Mode::Insert;
                terminal.draw(|f| render::list(f, &self.list))?;
                Modes { cursor_shape: if insert { 6 } else { 0 }, ..Default::default() }
            }
            View::Normal(id) => {
                let top = self.normal.top;
                let t = self.terms.iter_mut().find(|t| t.id == id).expect("checked above");
                let screen = t.vt.screen_mut();
                let total = vt::history(screen);
                screen.set_scrollback(total.saturating_sub(top));
                let normal = &self.normal;
                let drawn = terminal.draw(|f| render::term(f, screen, Some(normal)));
                screen.set_scrollback(0);
                drawn?;
                Modes::default()
            }
            View::Insert(id) => {
                let t = self.terms.iter().find(|t| t.id == id).expect("checked above");
                terminal.draw(|f| render::term(f, t.vt.screen(), None))?;
                vt::modes(&t.vt)
            }
        };
        if self.outer != Some(modes) {
            let mut out = io::stdout();
            out.write_all(&vt::mode_bytes(modes))?;
            out.flush()?;
            self.outer = Some(modes);
        }
        Ok(())
    }
}
