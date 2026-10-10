use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use anyhow::{Error, Result};

use crate::app::{Event, pump};
use crate::dump;
use crate::listdiff::Op;
use crate::proc;
use crate::pty::Pty;
use crate::session::Saved;
use crate::vt;

// output this soon after a spawn or resize is the program starting or redrawing, not activity
const SPAWN_QUIET: Duration = Duration::from_millis(1000);
const RESIZE_QUIET: Duration = Duration::from_millis(250);

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
}

impl Term {
    /// Where the shell is now, falling back to where it started.
    pub fn cwd(&self) -> PathBuf {
        self.pty.pid().and_then(proc::cwd).unwrap_or_else(|| self.cwd.clone())
    }

    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
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

    /// Clears activity and bell, now that it's been seen.
    pub fn seen(&mut self) {
        (self.activity, self.bell) = (false, false);
    }
}

/// The running list: terminals in order, each with a pty and its parsed screen. Output and exits arrive as events.
pub struct Mux {
    terms: Vec<Term>,
    next_id: u64,
    cols: u16,
    rows: u16,
    /// where new terminals start when nothing else says
    pub cwd: PathBuf,
    tx: Sender<Event>,
}

impl Mux {
    pub fn new(tx: Sender<Event>, cols: u16, rows: u16, cwd: PathBuf) -> Self {
        Self { terms: Vec::new(), next_id: 0, cols, rows, cwd, tx }
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
        let pty = Pty::spawn(self.cols, self.rows, &cwd)?;
        pump(pty.reader()?, self.tx.clone(), move |b| Event::Output(id, b), Event::Exited(id));
        let vt = vt::parser(self.cols, self.rows);
        Ok(Term { id, name, cwd, cmd: None, pty, vt, activity: false, bell: false, quiet: Instant::now() + SPAWN_QUIET })
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
            t.vt.screen_mut().set_size(rows, cols);
            t.quiet = t.quiet.max(Instant::now() + RESIZE_QUIET);
        }
        Ok(())
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

    /// Names, live directories, and commands, without output.
    pub fn saved(&self) -> Vec<Saved> {
        self.terms.iter().map(|t| Saved { cmd: t.cmd.clone(), ..Saved::new(t.name.clone(), t.cwd()) }).collect()
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

    fn names(mux: &Mux) -> Vec<&str> {
        mux.terms().iter().map(|t| t.name.as_str()).collect()
    }

    #[test]
    fn reconcile_keeps_kills_and_spawns_in_list_order() {
        let (tx, _rx) = mpsc::channel();
        let mut mux = Mux::new(tx, 80, 24, std::env::temp_dir());
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
}
