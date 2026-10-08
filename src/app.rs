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
    view: View,
    next_id: u64,
    cols: u16,
    rows: u16,
    top: usize,
    outer: Option<Modes>,
    cwd: PathBuf,
    tx: Sender<Event>,
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
        let mut app = Self {
            terms: Vec::new(),
            list: Buffer::new(Vec::new()),
            view: View::List,
            next_id: 0,
            cols,
            rows,
            top: 0,
            outer: None,
            cwd: cwd.clone(),
            tx,
            quit: false,
        };
        let name = cwd.file_name().map_or("shell".into(), |n| n.to_string_lossy().into_owned());
        let first = app.spawn(name, cwd)?;
        app.terms.push(first);
        app.list.load(app.lines());
        Ok(app)
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

    fn viewing(&self) -> Option<u64> {
        match self.view {
            View::Insert(id) | View::Normal(id) => Some(id),
            View::List => None,
        }
    }

    pub fn kill_all(&mut self) {
        for t in &mut self.terms {
            t.pty.kill();
        }
    }

    /// Applies one event. Returns whether the screen needs a redraw.
    pub fn handle(&mut self, event: Event) -> Result<bool> {
        match event {
            Event::Output(id, bytes) => {
                let Some(t) = self.term(id) else { return Ok(false) };
                t.vt.process(&bytes);
                let replies = std::mem::take(&mut t.vt.callbacks_mut().replies);
                if !replies.is_empty() {
                    t.pty.write(&replies)?;
                }
                Ok(self.viewing() == Some(id))
            }
            Event::Exited(id) => {
                let Some(i) = self.terms.iter().position(|t| t.id == id) else { return Ok(false) };
                let t = self.terms.remove(i);
                if self.viewing() == Some(id) {
                    self.view = View::List;
                }
                if !self.list.modified() {
                    self.list.load(self.lines());
                }
                self.list.message = Some(format!("{} exited", t.name));
                Ok(true)
            }
            Event::Hangup => {
                self.quit = true;
                Ok(false)
            }
            Event::Resize => {
                let (cols, rows) = crossterm::terminal::size()?;
                (self.cols, self.rows) = (cols, rows);
                for t in &mut self.terms {
                    t.pty.resize(cols, rows)?;
                    t.vt.screen_mut().set_size(rows, cols);
                }
                Ok(true)
            }
            Event::Input(bytes) => {
                for input in input::split(&bytes) {
                    self.input(input)?;
                }
                Ok(true)
            }
        }
    }

    fn input(&mut self, input: Input) -> Result<()> {
        let (View::Insert(id) | View::Normal(id)) = self.view else {
            if let Input::Bytes(b) = input {
                for key in input::keys(&b) {
                    self.list_key(key)?;
                }
            }
            return Ok(());
        };
        let (next, out) = state::step(self.view, input);
        if let Some(t) = self.term(id)
            && !out.is_empty()
        {
            t.pty.write(&out)?;
        }
        if next == View::List
            && let Some(row) = self.list.lines.iter().position(|l| l.id == Some(id))
        {
            self.list.cursor = (row, 0);
        }
        self.view = next;
        Ok(())
    }

    fn list_key(&mut self, key: Key) -> Result<()> {
        for effect in self.list.key(key) {
            match effect {
                Effect::Open(row) => match self.list.lines[row].id.filter(|id| self.terms.iter().any(|t| t.id == *id)) {
                    Some(id) => self.view = View::Insert(id),
                    None => self.list.message = Some("not written (:w to spawn it)".into()),
                },
                Effect::Write => self.write(),
                Effect::Quit => self.quit = true,
                Effect::Reload => self.list.load(self.lines()),
            }
        }
        Ok(())
    }

    /// `:w`: reconcile running terminals with the list buffer.
    fn write(&mut self) {
        let old: Vec<u64> = self.terms.iter().map(|t| t.id).collect();
        let ops = listdiff::diff(&old, &self.list.lines);
        let mut pool: HashMap<u64, Term> = self.terms.drain(..).map(|t| (t.id, t)).collect();
        let mut next: Vec<Term> = Vec::new();
        let mut errors = Vec::new();
        for op in ops {
            let spawned = match op {
                Op::Keep { id, name } => {
                    let mut t = pool.remove(&id).expect("diff only keeps known ids");
                    t.name = name;
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
                Ok(t) => next.push(t),
                Err(e) => errors.push(e),
            }
        }
        self.terms = next;
        self.list.load(self.lines());
        if let Some(e) = errors.first() {
            self.list.message = Some(format!("spawn failed: {e}"));
        }
    }

    pub fn draw(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        if self.viewing().is_some_and(|id| !self.terms.iter().any(|t| t.id == id)) {
            self.view = View::List;
        }
        self.top = render::scroll(self.top, self.list.cursor.0, self.rows.saturating_sub(1).max(1) as usize);
        let modes = match self.view {
            View::List => {
                let insert = self.list.mode == buffer::Mode::Insert;
                terminal.draw(|f| render::list(f, &self.list, self.top))?;
                Modes { cursor_shape: if insert { 6 } else { 0 }, ..Default::default() }
            }
            View::Insert(id) | View::Normal(id) => {
                let insert = matches!(self.view, View::Insert(_));
                let t = self.terms.iter().find(|t| t.id == id).expect("checked above");
                let tag = (!insert).then_some(" NORMAL ");
                terminal.draw(|f| render::term(f, t.vt.screen(), tag))?;
                if insert { vt::modes(&t.vt) } else { Modes::default() }
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
