use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Instant;

use anyhow::Result;
use ratatui::DefaultTerminal;

use crate::buffer::{self, Buffer, Effect, Line};
use crate::carry::Spot;
use crate::client::{self, Remote, Term};
use crate::config::Config;
use crate::cwds;
use crate::follow;
use crate::help;
use crate::input::{self, Input, Key, Mouse, MouseKind};
use crate::listdiff;
use crate::motion::Pos;
use crate::picker::{Item, Pick, Picker};
use crate::prompt;
use crate::protocol::{List, Reply, Request, Start};
use crate::render::{self, Bar};
use crate::sock;
use crate::state::{self, View};
use crate::status::{Entry, Info, Kind, Seg};
use crate::vt::{self, Modes};
use crate::workflow;

pub enum Event {
    Input(Vec<u8>),
    Reply(Reply),
    /// the server hung up without saying bye
    Gone,
    Hangup,
    Resize,
}

pub struct App {
    remote: Remote,
    /// the session's name
    name: String,
    list: Buffer,
    /// view n's read-only buffer over the viewed terminal's text
    normal: Buffer,
    view: View,
    cols: u16,
    rows: u16,
    outer: Option<Modes>,
    /// view n was just entered with `<C-\>`, so another one sends it literally
    fresh: bool,
    /// view n was entered with the wheel, so scrolling back to the bottom returns to view i
    wheeled: bool,
    /// where the left button went down in view i, for a drag that turns into a view n selection
    press: Option<(u16, u16)>,
    /// view n's text is behind the terminal
    stale: bool,
    /// when view n last edited the program's input, which it trusts over the screen until the screen shows it
    predicted: Option<Instant>,
    last: Option<u64>,
    alt: Option<u64>,
    /// list row to open once the user answers the write prompt
    confirm: Option<usize>,
    /// the terminal the server opens next came from the list's `<CR>`, so it opens in view n
    opening_normal: bool,
    /// `<C-p>`'s fuzzy finder, drawn over the terminal it has selected
    picker: Option<Picker>,
    /// `:h`'s key sheet, a read-only buffer drawn over everything
    help: Option<Buffer>,
    /// `:w`s sent whose list hasn't come back, which replaces the list buffer's edits rather than rebasing them
    writing: usize,
    /// what the server was last told this client views
    focused: Option<u64>,
    /// this client asked for `:qa`
    killing: bool,
    config: Config,
    /// when a partly typed mapping gives up waiting
    pub deadline: Option<Instant>,
    pub quit: bool,
    /// the outer terminal resized, so its screen can't be trusted to match the last frame
    repaint: bool,
    /// printed once the screen is restored, like `[detached from 0]`
    pub farewell: Option<String>,
}

/// Forwards everything `src` reads as events until EOF.
pub fn pump<E: Send + 'static>(mut src: impl Read + Send + 'static, tx: Sender<E>, wrap: impl Fn(Vec<u8>) -> E + Send + 'static, end: E) {
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
    /// Attaches to session `name` on `stream`, starting it from `start` if it's new.
    pub fn new(tx: Sender<Event>, stream: UnixStream, name: String, cols: u16, rows: u16, config: Config, start: Option<Start>) -> Result<Self> {
        let cursor = start.as_ref().map_or(0, |s| s.cursor);
        let (remote, focus, reader) = Remote::attach(stream, cols, term_rows(&config, rows), start)?;
        client::listen(reader, tx, Event::Reply, Event::Gone);
        let mut normal = Buffer::new(Vec::new());
        normal.readonly = true;
        normal.maps = config.view.clone();
        let mut list = Buffer::new(Vec::new());
        list.maps = config.list.clone();
        list.insert_maps = config.list_insert.clone();
        for b in [&mut normal, &mut list] {
            (b.incsearch, b.hlsearch) = (config.incsearch, config.hlsearch);
        }
        let mut app = Self {
            remote,
            name,
            list,
            normal,
            view: View::List,
            cols,
            rows,
            outer: None,
            fresh: false,
            wheeled: false,
            press: None,
            stale: false,
            predicted: None,
            last: None,
            alt: None,
            confirm: None,
            opening_normal: false,
            picker: None,
            help: None,
            writing: 0,
            focused: None,
            killing: false,
            config,
            deadline: None,
            quit: false,
            repaint: false,
            farewell: None,
        };
        app.size_buffers();
        app.list.load(app.lines());
        // a reattach lands where this session was last looked at
        let cursor = focus.and_then(|id| app.remote.position(id)).unwrap_or(cursor);
        app.list.place((cursor, 0), 0);
        if let Some(id) = focus {
            app.set_view(View::Insert(id));
        }
        app.tell_focus();
        Ok(app)
    }

    /// The list row of the viewed or last viewed terminal, saved with workflows and `--resume`'s snapshot.
    fn cursor(&self) -> usize {
        let at = self.viewing().or(self.last).and_then(|id| self.remote.position(id));
        at.unwrap_or(self.list.cursor.0)
    }

    pub fn message(&mut self, msg: String) {
        self.list.message = Some(msg);
    }

    fn term_rows(&self) -> u16 {
        term_rows(&self.config, self.rows)
    }

    fn size_buffers(&mut self) {
        self.list.height = self.rows.saturating_sub(1).max(1) as usize;
        self.normal.height = self.term_rows().max(1) as usize;
        if let Some(h) = &mut self.help {
            h.height = self.list.height;
        }
    }

    fn lines(&self) -> Vec<Line> {
        self.remote.terms().iter().map(|t| Line { id: Some(t.id), text: t.name.clone() }).collect()
    }

    fn term(&mut self, id: u64) -> Option<&mut Term> {
        self.remote.get_mut(id)
    }

    fn alive(&self, id: u64) -> bool {
        self.remote.alive(id)
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
            t.seen();
        }
    }

    fn go_list(&mut self, id: u64) {
        self.view = View::List;
        // for the directories next to the names
        self.remote.send(&Request::Refresh);
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
        self.normal.input = prompt::field(screen);
        self.predicted = None;
        self.normal.load(lines);
        self.normal.message = None;
        self.normal.place(live.cursor, live.bottom);
        self.stale = false;
        self.set_view(View::Normal(id));
    }

    fn refresh_normal(&mut self) {
        let View::Normal(id) = self.view else { return };
        let expired = self.predicted.is_some_and(|at| at.elapsed() >= prompt::SETTLE);
        if !self.stale && !expired {
            return;
        }
        self.stale = false;
        let Some(t) = self.term(id) else { return };
        let screen = t.vt.screen_mut();
        let lines = vt::text(screen).into_iter().map(|text| Line { id: None, text }).collect();
        let shown = prompt::field(screen);
        let caught_up = match (&shown, &self.normal.input) {
            (Some(a), Some(b)) => a.same(b),
            _ => false,
        };
        if self.predicted.is_none() || caught_up || expired {
            self.normal.input = shown;
            self.predicted = None;
        }
        self.normal.refresh(lines);
    }

    /// A new list from the server. Terminals that are gone leave the list even if they have edits; `open` is one this
    /// client asked to open.
    fn listed(&mut self, list: List) {
        for t in self.remote.sync(&list) {
            if let Some(p) = &mut self.picker {
                p.remove(t.id);
            }
            if self.viewing() == Some(t.id) {
                self.go_list(t.id);
            }
        }
        if self.writing > 0 {
            self.writing -= 1;
            self.list.load(self.lines());
        } else {
            self.list.rebase(self.lines());
        }
        if let Some(p) = &mut self.picker {
            p.cwds(|id| self.remote.get(id).map(|t| workflow::tilde(&t.cwd)));
        }
        if list.open.is_some() {
            let normal = std::mem::take(&mut self.opening_normal);
            self.open(list.open, normal);
        }
    }

    /// Tells the server what this client views, so it isn't flagged.
    fn tell_focus(&mut self) {
        let viewing = self.viewing();
        if self.focused != viewing {
            self.focused = viewing;
            self.remote.send(&Request::Focus { id: viewing });
        }
    }

    /// Applies one event. Returns whether the screen needs a redraw.
    pub fn handle(&mut self, event: Event) -> Result<bool> {
        let redraw = self.apply(event)?;
        self.tell_focus();
        Ok(redraw)
    }

    fn apply(&mut self, event: Event) -> Result<bool> {
        match event {
            Event::Reply(reply) => Ok(self.reply(reply)),
            Event::Gone => {
                if !self.quit {
                    self.quit = true;
                    let log = sock::log(&self.name).map_or_else(|e| e.to_string(), |p| p.display().to_string());
                    self.farewell = Some(format!("[lost session {}, see {log}]", self.name));
                }
                Ok(false)
            }
            // the session keeps running, as with :qd
            Event::Hangup => {
                self.quit = true;
                Ok(false)
            }
            Event::Resize => {
                let (cols, rows) = crossterm::terminal::size()?;
                crate::log::log(format_args!("resize {cols}x{rows}"));
                (self.cols, self.rows) = (cols, rows);
                self.repaint = true;
                self.size_buffers();
                self.remote.send(&Request::Resize { cols, rows: self.term_rows() });
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
                let waiting = match (&self.help, self.view) {
                    (Some(h), _) => h.waiting(),
                    _ if self.picker.is_some() => false,
                    (_, View::List) => self.list.waiting(),
                    (_, View::Normal(_)) => self.normal.waiting(),
                    (_, View::Insert(_)) => false,
                };
                self.deadline = waiting.then(|| Instant::now() + self.config.timeout);
                Ok(true)
            }
        }
    }

    /// Applies what the server sent. Returns whether the screen needs a redraw.
    fn reply(&mut self, reply: Reply) -> bool {
        match reply {
            Reply::Output { id, bytes } => {
                let normal = self.view == View::Normal(id);
                let viewed = self.viewing() == Some(id);
                let Some(t) = self.term(id) else { return false };
                let before = follow::live(t.vt.screen_mut());
                t.feed(&bytes);
                let after = follow::live(t.vt.screen_mut());
                if normal {
                    self.stale = true;
                    // an edit in flight already put the cursor where the program's will be
                    if self.normal.mode == buffer::Mode::Normal && self.predicted.is_none() {
                        (self.normal.cursor, self.normal.top) = follow::follow(self.normal.cursor, self.normal.top, before, after);
                    }
                }
                viewed
            }
            Reply::Flags { id, activity, bell } => {
                let Some(t) = self.term(id) else { return false };
                (t.activity, t.bell) = (activity, bell);
                self.config.status.enabled
            }
            Reply::List(list) => {
                self.listed(list);
                true
            }
            Reply::Exited { id } => {
                let Some(t) = self.remote.get(id) else { return false };
                self.list.message = Some(format!("{} exited", t.name));
                true
            }
            Reply::Resized { cols, rows } => {
                crate::log::log(format_args!("resized {cols}x{rows}"));
                self.remote.resized(cols, rows);
                if let View::Normal(id) = self.view {
                    self.enter_normal(id);
                }
                true
            }
            Reply::Message { text } | Reply::Error { text } => {
                let buf = if matches!(self.view, View::Normal(_)) { &mut self.normal } else { &mut self.list };
                buf.message = Some(text);
                true
            }
            Reply::Bye { reason } => {
                self.quit = true;
                self.farewell = match reason.as_str() {
                    "killed" if self.killing => None,
                    "killed" => Some(format!("[session {} killed]", self.name)),
                    "detached" => Some(format!("[detached from {}]", self.name)),
                    _ => Some(format!("[{reason}]")),
                };
                false
            }
            // resent after this client fell behind and missed output
            Reply::Attached { cols, rows, list, screens, .. } => {
                self.remote.resized(cols, rows);
                // writes it missed the lists of are in this one, so it's loaded once rather than rebased
                self.writing = self.writing.min(1);
                self.listed(list);
                self.remote.screens(screens);
                if let View::Normal(id) = self.view {
                    self.enter_normal(id);
                }
                true
            }
            Reply::Info { .. } | Reply::Text { .. } | Reply::Screen(_) => false,
        }
    }

    /// The mapping timeout passed: run the keys it was holding.
    pub fn flush(&mut self) -> Result<()> {
        self.deadline = None;
        if let Some(h) = &mut self.help {
            let effects = h.flush();
            return self.help_effects(effects);
        }
        match self.view {
            View::List => {
                let effects = self.list.flush();
                self.list_effects(effects)?;
            }
            View::Normal(id) => {
                let effects = self.normal.flush();
                self.normal_effects(id, effects)?;
            }
            View::Insert(_) => {}
        }
        self.tell_focus();
        Ok(())
    }

    fn input(&mut self, input: Input) -> Result<()> {
        if self.picker.is_some() {
            return self.picker_input(input);
        }
        if self.help.is_some() {
            return self.help_input(input);
        }
        let bytes = match (self.view, input) {
            (_, Input::Mouse(m)) => return self.mouse(m),
            (View::Insert(id), input) => {
                let (next, out) = state::step(self.view, input, self.config.escape);
                if !out.is_empty() {
                    self.remote.input(id, out);
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
                    self.remote.input(id, out);
                    self.set_view(next);
                } else {
                    self.normal.escape();
                }
                return Ok(());
            }
            (View::List, Input::Escape) => {
                self.list.escape();
                return Ok(());
            }
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

    /// `:h [topic]`: the key sheet as the config has it, at the first line mentioning topic.
    fn help(&mut self, topic: &str) {
        let lines = help::sheet(&self.config).into_iter().map(|text| Line { id: None, text }).collect();
        let mut b = Buffer::new(lines);
        b.readonly = true;
        b.maps = self.config.view.clone();
        (b.incsearch, b.hlsearch) = (self.config.incsearch, self.config.hlsearch);
        b.height = self.list.height;
        let topic = topic.to_lowercase();
        let found = b.lines.iter().enumerate().find_map(|(r, l)| Some((r, l.text.to_lowercase().find(&topic)?)));
        match found {
            Some((row, at)) if !topic.is_empty() => b.place((row, b.lines[row].text[..at].chars().count()), row),
            Some(_) => {}
            None => b.message = Some(format!("E149: Sorry, no help for {topic}")),
        }
        self.help = Some(b);
    }

    fn help_input(&mut self, input: Input) -> Result<()> {
        let help = self.help.as_mut().expect("checked by the caller");
        let bytes = match input {
            Input::Bytes(b) => b,
            Input::Escape => {
                help.escape();
                return Ok(());
            }
            Input::Mouse(m) => {
                if let kind @ (MouseKind::WheelUp | MouseKind::WheelDown) = m.kind() {
                    help.wheel(kind == MouseKind::WheelDown);
                }
                return Ok(());
            }
        };
        for (key, end) in input::spans(&bytes) {
            let Some(help) = self.help.as_mut() else { break };
            let effects = help.key(key);
            self.help_effects(effects)?;
            if self.help.is_none() {
                if end < bytes.len() {
                    self.input(Input::Bytes(bytes[end..].to_vec()))?;
                }
                break;
            }
        }
        Ok(())
    }

    /// `q` and `:q` close the sheet; moving to a terminal or leaving vix closes it and happens underneath.
    fn help_effects(&mut self, effects: Vec<Effect>) -> Result<()> {
        for effect in effects {
            match effect {
                Effect::Parent | Effect::Quit { .. } => self.help = None,
                Effect::Help(topic) => self.help(&topic),
                Effect::Yank(text) => {
                    let mut out = io::stdout();
                    out.write_all(&vt::osc52(&text))?;
                    out.flush()?;
                }
                e @ (Effect::Goto(_)
                | Effect::Switch(_)
                | Effect::Alternate
                | Effect::Notified
                | Effect::Pick
                | Effect::Edit { .. }
                | Effect::QuitAll { .. }
                | Effect::Detach { .. }) => {
                    self.help = None;
                    match self.viewing() {
                        Some(id) => self.normal_effects(id, vec![e])?,
                        None => self.list_effects(vec![e])?,
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Opens the picker, the alternate terminal first so `<C-p><CR>` acts like `<C-^>`. Directories update once the
    /// server sends fresh ones.
    fn pick(&mut self) {
        let alternate = if self.viewing().is_some() { self.alt } else { self.last };
        let mut items: Vec<Item> = self
            .remote
            .terms()
            .iter()
            .enumerate()
            .map(|(i, t)| Item { id: t.id, index: i + 1, name: t.name.clone(), cwd: workflow::tilde(&t.cwd) })
            .collect();
        if let Some(i) = items.iter().position(|item| Some(item.id) == alternate) {
            let item = items.remove(i);
            items.insert(0, item);
        }
        self.picker = Some(Picker::new(items));
        self.remote.send(&Request::Refresh);
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
                    if let Some((row, spot)) = at {
                        self.carry(id, row, spot)?;
                    }
                    self.set_view(View::Insert(id));
                }
                Effect::Parent | Effect::Quit { force: false } => self.go_list(id),
                Effect::Quit { force: true } => {
                    if let Some(name) = self.kill(id) {
                        self.go_list(id);
                        self.list.message = Some(format!("{name} killed"));
                    }
                }
                Effect::QuitAll { force } => self.quit_all(force, true),
                Effect::Detach { force } => self.detach(force, true),
                Effect::Switch(n) => {
                    let i = self.remote.position(id).unwrap_or(0) as isize;
                    let j = (i + n).clamp(0, self.remote.len() as isize - 1) as usize;
                    let next = self.remote.terms()[j].id;
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
                Effect::Goto(n) => match state::goto(n, self.remote.len()) {
                    Some(i) => self.enter_normal(self.remote.terms()[i].id),
                    None => self.normal.message = Some(format!("no terminal {n}")),
                },
                Effect::Pick => self.pick(),
                Effect::Notified => self.notified(Some(id)),
                Effect::Delete(name) => self.delete(id, name),
                Effect::Rename(name) => self.rename(id, name),
                Effect::Change { change, insert } => {
                    self.remote.send(&Request::Change { id, change });
                    self.predicted = Some(Instant::now());
                    if insert {
                        self.set_view(View::Insert(id));
                    }
                }
                Effect::Help(topic) => self.help(&topic),
                Effect::Open(_) | Effect::Write | Effect::Save(_) => {}
            }
        }
        Ok(())
    }

    /// The list's directory per row, for rows that are running terminals.
    fn notes(&self) -> Vec<Option<String>> {
        let terms: Vec<_> = self.list.lines.iter().filter_map(|l| self.remote.get(l.id?)).collect();
        let cwds: Vec<_> = terms.iter().map(|t| workflow::tilde(&t.cwd)).collect();
        let pairs: Vec<_> = terms.iter().zip(&cwds).map(|(t, c)| (t.name.as_str(), c.as_str())).collect();
        let mut labels = cwds::labels(&pairs).into_iter();
        self.list.lines.iter().map(|l| l.id.and_then(|id| self.remote.get(id)).and_then(|_| labels.next()?)).collect()
    }

    /// `<Tab>`: views the next terminal after `from` that rang or has unseen output.
    fn notified(&mut self, from: Option<u64>) {
        let flags: Vec<_> = self.remote.terms().iter().map(|t| (t.bell, t.activity)).collect();
        let current = from.and_then(|id| self.remote.position(id));
        match state::notifying(&flags, current) {
            Some(i) => self.enter_normal(self.remote.terms()[i].id),
            None if self.viewing().is_some() => self.normal.message = Some("no notifications".into()),
            None => self.list.message = Some("no notifications".into()),
        }
    }

    /// Moves a line editor's cursor to where `i a I A` asked, when that's on its line; anywhere else it stays put.
    fn carry(&mut self, id: u64, row: usize, spot: Spot) -> Result<()> {
        self.remote.send(&Request::Carry { id, row, spot });
        Ok(())
    }

    /// Asks the server to kill `id`. Returns its name; the list drops it once the server sends the next one.
    fn kill(&mut self, id: u64) -> Option<String> {
        let name = self.remote.get(id)?.name.clone();
        self.remote.send(&Request::Kill { id });
        Some(name)
    }

    /// `:d [name]` from view n on `id`. Deleting the viewed terminal moves on to the alternate or a neighbor, like `:bd`.
    fn delete(&mut self, id: u64, name: Option<String>) {
        let target = match &name {
            None => id,
            Some(name) => match self.remote.terms().iter().find(|t| &t.name == name) {
                Some(t) => t.id,
                None => {
                    self.normal.message = Some(format!("E94: No matching terminal for {name}"));
                    return;
                }
            },
        };
        let next = if target == id { self.successor(id) } else { Some(id) };
        let Some(killed) = self.kill(target) else { return };
        let msg = format!("{killed} killed");
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
        let terms = self.remote.terms();
        let i = self.remote.position(id)?;
        terms.get(i + 1).or(i.checked_sub(1).and_then(|j| terms.get(j))).map(|t| t.id)
    }

    /// `:r name` from view n on `id`.
    fn rename(&mut self, id: u64, name: String) {
        self.remote.send(&Request::Rename { id, name });
    }

    /// The program in view i asked for the mouse, so events go to it as they are.
    fn program_mouse(&self, id: u64) -> bool {
        !self.config.mouse || self.remote.get(id).is_some_and(|t| vt::modes(&t.vt).mouse != 0)
    }

    fn mouse(&mut self, m: Mouse) -> Result<()> {
        let kind = m.kind();
        match self.view {
            View::Insert(id) if self.program_mouse(id) => self.remote.input(id, m.encode()),
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
                self.remote.input(id, arrow.repeat(buffer::WHEEL).into_bytes());
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
        if self.unsaved(force, from_normal) {
            return;
        }
        self.kill_session();
    }

    /// `:qa` and `:q` from the list end the session for every client. The server saves `--resume`'s snapshot, then
    /// says bye.
    fn kill_session(&mut self) {
        self.killing = true;
        self.remote.send(&Request::KillSession { cursor: self.cursor() });
    }

    /// `:qd` leaves the session running. Staged list edits live here, so they'd be lost.
    fn detach(&mut self, force: bool, from_normal: bool) {
        if !self.unsaved(force, from_normal) {
            self.remote.send(&Request::Detach);
        }
    }

    /// Refuses with E37 when the list has edits, unless forced.
    fn unsaved(&mut self, force: bool, from_normal: bool) -> bool {
        if force || !self.list.modified() {
            return false;
        }
        let msg = Some("E37: No write since last change (add ! to override)".into());
        if from_normal { self.normal.message = msg } else { self.list.message = msg }
        true
    }

    fn list_key(&mut self, key: Key) -> Result<()> {
        if let Some(row) = self.confirm.take() {
            self.list.message = None;
            if key == Key::Char('y') {
                self.write_open(row);
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
                    if !summary.is_empty() {
                        self.list.message = Some(format!("{summary}. write? [y/n]"));
                        self.confirm = Some(row);
                    } else if self.list.modified() {
                        self.write_open(row);
                    } else {
                        self.open(self.list.lines.get(row).and_then(|l| l.id), true);
                    }
                }
                Effect::Write => {
                    self.write(None);
                    if self.remote.workflow.is_some() {
                        self.remote.send(&Request::Save { workflow: None, cursor: self.cursor() });
                    }
                }
                Effect::Save(name) => {
                    // a bad name would otherwise quit without saving
                    if let Err(e) = workflow::path(&name) {
                        self.list.message = Some(e.to_string());
                        return Ok(());
                    }
                    self.write(None);
                    self.remote.send(&Request::Save { workflow: Some(name), cursor: self.cursor() });
                }
                Effect::Quit { .. } => self.kill_session(),
                Effect::QuitAll { force } => self.quit_all(force, false),
                Effect::Detach { force } => self.detach(force, false),
                Effect::Reload => self.list.load(self.lines()),
                Effect::Edit { name, new } => self.edit(name, new, None),
                Effect::Pick => self.pick(),
                Effect::Notified => self.notified(self.last),
                Effect::Help(topic) => self.help(&topic),
                _ => {}
            }
        }
        Ok(())
    }

    /// `:e name` / `:new name`: views the terminal called `name`, or spawns it at the end of the list in `from`'s
    /// directory, opened once the server sends the list with it.
    fn edit(&mut self, name: String, new: bool, from: Option<u64>) {
        self.opening_normal = false;
        if let Some(id) = self.remote.terms().iter().find(|t| !new && t.name == name).map(|t| t.id) {
            self.set_view(View::Insert(id));
            return;
        }
        self.remote.send(&Request::Spawn { name, from, cwd: None, cmd: None });
    }

    fn open(&mut self, id: Option<u64>, normal: bool) {
        match id.filter(|id| self.alive(*id)) {
            Some(id) if normal => self.enter_normal(id),
            Some(id) => self.set_view(View::Insert(id)),
            None => self.list.message = Some("nothing to open on this line".into()),
        }
    }

    fn pending_summary(&self) -> String {
        if !self.list.modified() {
            return String::new();
        }
        let current = self.current();
        listdiff::summary(&current, &listdiff::diff(&self.list.base(), &current, &self.list.lines))
    }

    fn current(&self) -> Vec<(u64, &str)> {
        self.remote.terms().iter().map(|t| (t.id, t.name.as_str())).collect()
    }

    /// `:w`: the server reconciles running terminals with the list buffer and sends the list back, opening the
    /// terminal on row `open`. Edits count as written once sent.
    fn write(&mut self, open: Option<usize>) {
        self.writing += 1;
        self.remote.send(&Request::Write { base: self.list.base(), lines: self.list.lines.clone(), open });
        self.list.refresh(self.list.lines.clone());
    }

    /// Writes, then opens row `row`'s terminal, which a blank row doesn't have.
    fn write_open(&mut self, row: usize) {
        let blank = self.list.lines.get(row).is_none_or(|l| l.text.trim().is_empty());
        self.opening_normal = !blank;
        self.write((!blank).then_some(row));
        if blank {
            self.open(None, true);
        }
    }

    /// The bottom row's status. Without the bar the list still shows its mode and `[+]`, as before.
    fn bar(&self) -> Option<Bar> {
        let status = &self.config.status;
        if !status.enabled {
            let modified = if self.list.modified() { "[+]" } else { "" };
            let seg = |text: &str, kind| vec![Seg { text: text.into(), kind }];
            return (self.view == View::List).then(|| Bar { left: seg(self.list.mode.label(), Kind::Mode), right: seg(modified, Kind::Text) });
        }
        let viewing = self.viewing();
        let alternate = if viewing.is_some() { self.alt } else { self.last };
        let terms = self
            .remote
            .terms()
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
        let workflow = self.remote.workflow.as_deref().unwrap_or_default();
        let info = Info { mode, modified: self.list.modified(), workflow, terms };
        Some(Bar { left: status.left.expand(&info), right: status.right.expand(&info) })
    }

    /// Button and drag events in SGR encoding, when vix has the mouse.
    fn mouse_modes(&self, modes: Modes) -> Modes {
        if self.config.mouse { Modes { mouse: 1002, mouse_encoding: 1006, ..modes } } else { modes }
    }

    pub fn draw(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        // not `clear`, which asks the terminal where its cursor is, and stdin is read elsewhere
        if std::mem::take(&mut self.repaint) {
            let area = terminal.size()?.into();
            terminal.resize(area)?;
        }
        if self.viewing().is_some_and(|id| !self.alive(id)) {
            self.view = View::List;
        }
        self.refresh_normal();
        let bar = self.bar();
        let notes = self.notes();
        if let Some(h) = &self.help {
            let seg = |text: &str, kind| vec![Seg { text: text.into(), kind }];
            let bar = Bar { left: seg(h.mode.label(), Kind::Mode), right: seg("help", Kind::Text) };
            terminal.draw(|f| render::list(f, h, &[], &bar))?;
            let modes = self.mouse_modes(Modes::default());
            return self.set_outer(modes);
        }
        if let Some(p) = &self.picker {
            // typing keeps what you were looking at; moving the selection previews it
            let shown = if p.browsing { p.selection() } else { self.viewing() };
            let preview = shown.and_then(|id| self.remote.get(id));
            terminal.draw(|f| {
                match (preview, &bar) {
                    (Some(t), bar) => render::term(f, t.vt.screen(), None, bar.as_ref()),
                    (None, Some(bar)) if self.view == View::List => render::list(f, &self.list, &notes, bar),
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
                terminal.draw(|f| render::list(f, &self.list, &notes, bar))?;
                self.mouse_modes(Modes { cursor_shape: if insert { 6 } else { 0 }, ..Default::default() })
            }
            View::Normal(id) => {
                let top = self.normal.top;
                let t = self.remote.get_mut(id).expect("checked above");
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
                let t = self.remote.get(id).expect("checked above");
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

/// The bar takes a row from every terminal, so ptys keep their size across views.
fn term_rows(config: &Config, rows: u16) -> u16 {
    if config.status.enabled { rows.saturating_sub(1).max(1) } else { rows }
}

/// Buffer position under screen cell (x, y), held to the visible lines.
fn cell_pos(buf: &Buffer, (x, y): (u16, u16)) -> Pos {
    let y = (y as usize).min(buf.height.max(1) - 1);
    let row = (buf.top + y).min(buf.lines.len() - 1);
    (row, render::col_at(&buf.lines[row].text, x))
}
