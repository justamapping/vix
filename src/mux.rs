use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use anyhow::{Error, Result};

use crate::app::pump;
use crate::buffer::Line;
use crate::carry::{self, Spot};
use crate::dump;
use crate::listdiff::{self, Op};
use crate::proc;
use crate::prompt::{Change, Typed};
use crate::pty::Pty;
use crate::session::Saved;
use crate::vt;

// output this soon after a spawn or resize is the program starting or redrawing, not activity
const SPAWN_QUIET: Duration = Duration::from_millis(1000);
const RESIZE_QUIET: Duration = Duration::from_millis(250);

/// What a terminal's pty tells whoever owns the `Mux`.
pub enum PtyEvent {
    Output(u64, Vec<u8>),
    Exited(u64),
}

pub struct Term {
    pub id: u64,
    pub name: String,
    /// where it started
    cwd: PathBuf,
    /// the workflow command it was started with
    pub cmd: Option<String>,
    pty: Pty,
    pub vt: vt::Parser,
    /// output or a bell since it was last viewed
    pub activity: bool,
    pub bell: bool,
    quiet: Instant,
    /// the input the last change typed will leave, so the next one can build on it before the program redraws
    typed: Option<Typed>,
}

impl Term {
    /// Where the shell is now, falling back to where it started.
    pub fn cwd(&self) -> PathBuf {
        self.pty.pid().and_then(proc::cwd).unwrap_or_else(|| self.cwd.clone())
    }

    /// The program running in the foreground, or None when the shell is.
    pub fn fg(&self) -> Option<String> {
        let leader = self.pty.foreground()?;
        if Some(leader) == self.pty.pid() {
            return None;
        }
        proc::name(leader)
    }

    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.typed = None;
        self.pty.write(bytes)
    }

    /// Whether the program reads keys as they come, like a line editor.
    pub fn raw(&self) -> bool {
        self.pty.raw()
    }

    pub fn kill(&mut self) {
        self.pty.kill();
    }

    /// Parses output, answers the program's queries, and flags it unless it's being viewed. Returns whether flags changed.
    pub fn feed(&mut self, bytes: &[u8], viewed: bool) -> Result<bool> {
        self.vt.process(bytes);
        let replies = std::mem::take(&mut self.vt.callbacks_mut().replies);
        if !replies.is_empty() {
            self.pty.write(&replies)?;
        }
        let flags = (self.activity, self.bell);
        let bell = std::mem::take(&mut self.vt.callbacks_mut().bell);
        if !viewed {
            self.bell |= bell;
            self.activity |= Instant::now() >= self.quiet;
        }
        Ok((self.activity, self.bell) != flags)
    }

    /// Moves a line editor's cursor to `spot` on `row` (counting scrollback) with arrow keys, for `i a I A`.
    pub fn carry(&mut self, row: usize, spot: Spot) -> Result<()> {
        if !self.raw() {
            return Ok(());
        }
        let Some(row) = row.checked_sub(vt::history(self.vt.screen_mut())) else { return Ok(()) };
        if let Some(keys) = carry::arrows(self.vt.screen(), row as u16, spot) {
            self.write(&keys)?;
        }
        Ok(())
    }

    /// Types `change` into a line editor, if its screen still shows the input the change was made from, or the last
    /// change will leave it there. Returns whether it did.
    pub fn change(&mut self, change: &Change) -> Result<bool> {
        let typed = self.typed.take();
        let keys = self.raw().then(|| change.keys(self.vt.screen(), vt::modes(&self.vt), typed.as_ref())).flatten();
        let Some(keys) = keys else { return Ok(false) };
        self.write(&keys)?;
        self.typed = Some(change.typed());
        Ok(true)
    }

    /// Clears activity and bell, now that it's been seen.
    pub fn seen(&mut self) {
        (self.activity, self.bell) = (false, false);
    }
}

/// The running list: terminals in order, each with a pty and its parsed screen. Output and exits arrive as events.
pub struct Mux<E> {
    terms: Vec<Term>,
    next_id: u64,
    cols: u16,
    rows: u16,
    /// where new terminals start when nothing else says
    pub cwd: PathBuf,
    /// the session's socket, which shells get as `VIX_SOCKET` along with their `VIX_ID`
    socket: Option<PathBuf>,
    tx: Sender<E>,
}

impl<E: From<PtyEvent> + Send + 'static> Mux<E> {
    pub fn new(tx: Sender<E>, cols: u16, rows: u16, cwd: PathBuf, socket: Option<PathBuf>) -> Self {
        Self { terms: Vec::new(), next_id: 0, cols, rows, cwd, socket, tx }
    }

    pub fn terms(&self) -> &[Term] {
        &self.terms
    }

    pub fn len(&self) -> usize {
        self.terms.len()
    }

    pub fn get(&self, id: u64) -> Option<&Term> {
        self.terms.iter().find(|t| t.id == id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Term> {
        self.terms.iter_mut().find(|t| t.id == id)
    }

    pub fn position(&self, id: u64) -> Option<usize> {
        self.terms.iter().position(|t| t.id == id)
    }

    pub fn alive(&self, id: u64) -> bool {
        self.get(id).is_some()
    }

    fn start(&mut self, name: String, cwd: PathBuf) -> Result<Term> {
        let id = self.next_id;
        self.next_id += 1;
        let id_var = id.to_string();
        let mut env = vec![("VIX_ID", OsStr::new(&id_var))];
        if let Some(socket) = &self.socket {
            env.push(("VIX_SOCKET", socket.as_os_str()));
        }
        let pty = Pty::spawn(self.cols, self.rows, &cwd, &env)?;
        pump(pty.reader()?, self.tx.clone(), move |b| PtyEvent::Output(id, b).into(), PtyEvent::Exited(id).into());
        let vt = vt::parser(self.cols, self.rows);
        Ok(Term { id, name, cwd, cmd: None, pty, vt, activity: false, bell: false, quiet: Instant::now() + SPAWN_QUIET, typed: None })
    }

    /// Starts a terminal at the end of the list, replaying saved output and typing `cmd` into it.
    pub fn spawn(&mut self, saved: Saved) -> Result<u64> {
        let Saved { name, cwd, cmd, output } = saved;
        let mut t = self.start(name, cwd)?;
        dump::restore(&mut t.vt, &output);
        if let Some(cmd) = &cmd {
            t.write(format!("{cmd}\r").as_bytes())?;
        }
        t.cmd = cmd;
        let id = t.id;
        self.terms.push(t);
        Ok(id)
    }

    /// Spawns a session's terminals, or one Untitled. A directory that's gone falls back to the mux's.
    pub fn open(&mut self, mut saved: Vec<Saved>) -> Result<()> {
        if saved.is_empty() {
            saved.push(Saved::new("Untitled", self.cwd.clone()));
        }
        for mut s in saved {
            if !s.cwd.is_dir() {
                s.cwd = self.cwd.clone();
            }
            self.spawn(s)?;
        }
        Ok(())
    }

    /// Takes a terminal out of the list without killing it, e.g. once it exited.
    pub fn remove(&mut self, id: u64) -> Option<Term> {
        let i = self.position(id)?;
        Some(self.terms.remove(i))
    }

    pub fn kill_all(&mut self) {
        for t in &mut self.terms {
            t.kill();
        }
    }

    /// Renames `id`, returning its old name.
    pub fn rename(&mut self, id: u64, name: String) -> Option<String> {
        Some(std::mem::replace(&mut self.get_mut(id)?.name, name))
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        (self.cols, self.rows) = (cols, rows);
        for t in &mut self.terms {
            t.pty.resize(cols, rows)?;
            vt::resize(&mut t.vt, cols, rows);
            t.quiet = t.quiet.max(Instant::now() + RESIZE_QUIET);
        }
        Ok(())
    }

    /// Makes full-screen programs redraw, for what `dump::exact` can't copy, like scroll regions: shrinks their ptys
    /// a row, until `fit`. Returns which. tmux does this on attach.
    pub fn shrink(&mut self) -> Result<Vec<u64>> {
        let mut shrunk = Vec::new();
        for t in self.terms.iter_mut().filter(|t| t.vt.screen().alternate_screen()) {
            t.pty.resize(self.cols, self.rows.saturating_sub(1).max(1))?;
            t.quiet = t.quiet.max(Instant::now() + RESIZE_QUIET);
            shrunk.push(t.id);
        }
        Ok(shrunk)
    }

    /// Gives shrunk ptys their size back.
    pub fn fit(&mut self, ids: &[u64]) -> Result<()> {
        for t in self.terms.iter_mut().filter(|t| ids.contains(&t.id)) {
            t.pty.resize(self.cols, self.rows)?;
        }
        Ok(())
    }

    /// `:w` of `lines`, edited from the list `base`. Returns the id each line ended up with, and failed spawns.
    pub fn write(&mut self, base: &[u64], lines: &[Line]) -> (Vec<Option<u64>>, Vec<Error>) {
        let current: Vec<(u64, &str)> = self.terms.iter().map(|t| (t.id, t.name.as_str())).collect();
        let ops = listdiff::diff(base, &current, lines);
        let (placed, errors) = self.reconcile(ops);
        // ops come in non-blank line order
        let mut placed = placed.into_iter();
        let ids = lines.iter().map(|l| if l.text.trim().is_empty() { None } else { placed.next().flatten() }).collect();
        (ids, errors)
    }

    /// Applies `listdiff` ops. Returns the id each Keep/Clone/Spawn ended up with, in op order, and failed spawns.
    pub fn reconcile(&mut self, ops: Vec<Op>) -> (Vec<Option<u64>>, Vec<Error>) {
        let mut pool: HashMap<u64, Term> = self.terms.drain(..).map(|t| (t.id, t)).collect();
        let mut next: Vec<Term> = Vec::new();
        let mut placed = Vec::new();
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
                        t.kill();
                    }
                    continue;
                }
                Op::Clone { from, name } => {
                    let cwd = next.iter().find(|t| t.id == from).map_or(self.cwd.clone(), Term::cwd);
                    self.start(name, cwd)
                }
                Op::Spawn { name } => self.start(name, self.cwd.clone()),
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
        self.terms = next;
        (placed, errors)
    }

    /// Spawns each of `saved` that no terminal has the name of yet, and with `prune` kills terminals not in `saved`.
    /// Running it twice does nothing the second time. Returns how many it spawned and killed.
    pub fn apply(&mut self, saved: Vec<Saved>, prune: bool) -> Result<(usize, usize)> {
        let current: Vec<(u64, &str)> = self.terms.iter().map(|t| (t.id, t.name.as_str())).collect();
        let wanted: Vec<&str> = saved.iter().map(|s| s.name.as_str()).collect();
        let (matched, extra) = listdiff::by_name(&current, &wanted);
        let missing: Vec<Saved> = saved.into_iter().zip(matched).filter(|(_, m)| m.is_none()).map(|(s, _)| s).collect();
        let spawned = missing.len();
        if spawned > 0 {
            self.open(missing)?;
        }
        let killed = if prune { extra.len() } else { 0 };
        for id in extra.into_iter().take(killed) {
            if let Some(mut t) = self.remove(id) {
                t.kill();
            }
        }
        Ok((spawned, killed))
    }

    /// Names, live directories, and commands, without output.
    pub fn saved(&self) -> Vec<Saved> {
        self.terms.iter().map(|t| Saved { cmd: t.cmd.clone(), ..Saved::new(t.name.clone(), t.cwd()) }).collect()
    }

    /// Each terminal's exact screen, for a client attaching.
    pub fn exact(&mut self) -> Vec<(u64, Vec<u8>)> {
        self.terms.iter_mut().map(|t| (t.id, dump::exact(&mut t.vt))).collect()
    }

    /// `saved` with each screen and scrollback, for `--resume`.
    pub fn dumped(&mut self) -> Vec<Saved> {
        let mut saved = self.saved();
        for (s, t) in saved.iter_mut().zip(&mut self.terms) {
            s.output = dump::dump(&mut t.vt);
        }
        saved
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn names(mux: &Mux<PtyEvent>) -> Vec<&str> {
        mux.terms().iter().map(|t| t.name.as_str()).collect()
    }

    #[test]
    fn reconcile_keeps_kills_and_spawns_in_list_order() {
        let (tx, _rx) = mpsc::channel();
        let mut mux: Mux<PtyEvent> = Mux::new(tx, 80, 24, std::env::temp_dir(), None);
        let a = mux.spawn(Saved::new("a", "/")).unwrap();
        let b = mux.spawn(Saved::new("b", "/")).unwrap();
        let ops = vec![
            Op::Spawn { name: "c".into() },
            Op::Keep { id: b, name: "bee".into() },
            Op::Clone { from: b, name: "b2".into() },
            Op::Kill { id: a },
        ];
        let (placed, errors) = mux.reconcile(ops);
        assert!(errors.is_empty());
        assert_eq!(names(&mux), ["c", "bee", "b2"]);
        assert_eq!(placed[1], Some(b));
        assert!(!mux.alive(a) && placed.iter().all(|id| id.is_some_and(|id| mux.alive(id))));
        assert_eq!(mux.rename(b, "b".into()).as_deref(), Some("bee"));
        assert_eq!(mux.saved()[2].cwd, PathBuf::from("/"));
        mux.kill_all();
    }

    #[test]
    fn apply_spawns_whats_missing_once() {
        let (tx, _rx) = mpsc::channel();
        let mut mux: Mux<PtyEvent> = Mux::new(tx, 80, 24, std::env::temp_dir(), None);
        mux.spawn(Saved::new("web", "/")).unwrap();
        mux.spawn(Saved::new("scratch", "/")).unwrap();
        let wanted = || vec![Saved::new("web", "/"), Saved::new("api", "/gone")];
        assert_eq!(mux.apply(wanted(), false).unwrap(), (1, 0));
        assert_eq!(names(&mux), ["web", "scratch", "api"]);
        assert_eq!(mux.terms()[2].cwd, std::env::temp_dir(), "a directory that's gone falls back");
        assert_eq!(mux.apply(wanted(), false).unwrap(), (0, 0));
        assert_eq!(mux.apply(wanted(), true).unwrap(), (0, 1));
        assert_eq!(names(&mux), ["web", "api"]);
        mux.kill_all();
    }
}
