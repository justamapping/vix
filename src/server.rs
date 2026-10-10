//! `vix --server <name>`: owns one session's terminals and serves its clients over the socket.

use std::collections::HashMap;
use std::fs;
use std::io::{BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result};

use crate::mux::{Mux, PtyEvent, Term};
use crate::protocol::{self, Entry, List, Reply, Request, Screen, Start};
use crate::session::{self, Saved, Session};
use crate::sock;
use crate::workflow;

// how long full-screen programs stay a row short on attach, so they see two sizes and redraw
const NUDGE: Duration = Duration::from_millis(50);
// a client that won't take its last messages within this is dropped anyway
const BYE_TIMEOUT: Duration = Duration::from_secs(1);

pub enum Event {
    Pty(PtyEvent),
    Connect(UnixStream),
    Request(usize, Request),
    Gone(usize),
    /// ptys shrunk on attach, to give their size back
    Fit(Vec<u64>),
}

impl From<PtyEvent> for Event {
    fn from(pty: PtyEvent) -> Self {
        Event::Pty(pty)
    }
}

struct Client {
    out: Sender<Arc<Vec<u8>>>,
    stream: UnixStream,
    writer: JoinHandle<()>,
    /// said Hello, so it gets output
    attached: bool,
    focus: Option<u64>,
    size: (u16, u16),
}

pub struct Server {
    name: String,
    mux: Mux<Event>,
    clients: HashMap<usize, Client>,
    next_client: usize,
    workflow: Option<String>,
    /// the terminal last viewed by any client, where a reattach lands
    last: Option<u64>,
    size: (u16, u16),
    /// the first Hello spawned the session
    started: bool,
    killed: bool,
    /// where `KillSession` leaves the `--resume` snapshot
    snapshot: PathBuf,
    tx: Sender<Event>,
}

/// Serves session `name` on its socket until it's killed or empty.
pub fn run(name: &str) -> Result<()> {
    let path = sock::path(&sock::dir()?, name)?;
    let listener = sock::bind(&path)?;
    eprintln!("vix {} serving {name}", env!("CARGO_PKG_VERSION"));
    serve(listener, &path, name, session::path().context("no $HOME")?)
}

/// The server loop on `listener`, removing the socket at `path` once done.
pub fn serve(listener: UnixListener, path: &Path, name: &str, snapshot: PathBuf) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let accept = tx.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    if accept.send(Event::Connect(stream)).is_err() {
                        return;
                    }
                }
                Err(e) => eprintln!("accept: {e}"),
            }
        }
    });
    let mut server = Server {
        name: name.into(),
        mux: Mux::new(tx.clone(), 80, 24, std::env::current_dir()?),
        clients: HashMap::new(),
        next_client: 0,
        workflow: None,
        last: None,
        size: (80, 24),
        started: false,
        killed: false,
        snapshot,
        tx,
    };
    let result = server.serve(rx);
    let _ = fs::remove_file(path);
    server.mux.kill_all();
    server.close_all();
    result
}

impl Server {
    fn serve(&mut self, rx: mpsc::Receiver<Event>) -> Result<()> {
        for event in rx {
            self.handle(event)?;
            if !self.alive() {
                break;
            }
        }
        Ok(())
    }

    /// Lives while it has terminals or clients, once started.
    fn alive(&self) -> bool {
        !self.killed && (!self.started || self.mux.len() > 0 || self.clients.values().any(|c| c.attached))
    }

    fn handle(&mut self, event: Event) -> Result<()> {
        match event {
            Event::Connect(stream) => self.connect(stream)?,
            Event::Gone(cid) => {
                self.clients.remove(&cid);
            }
            Event::Pty(PtyEvent::Output(id, bytes)) => self.output(id, bytes),
            Event::Pty(PtyEvent::Exited(id)) => {
                if self.mux.remove(id).is_some() {
                    self.broadcast(&Reply::Exited { id });
                    self.lists(None, None);
                }
            }
            Event::Fit(ids) => self.mux.fit(&ids)?,
            Event::Request(cid, request) => {
                if let Err(e) = self.request(cid, request) {
                    self.send(cid, &Reply::Message { text: format!("{e:#}") });
                }
            }
        }
        Ok(())
    }

    /// Reads requests on a thread of its own, and writes replies from a queue on another.
    fn connect(&mut self, stream: UnixStream) -> Result<()> {
        let cid = self.next_client;
        self.next_client += 1;
        let tx = self.tx.clone();
        let reader = BufReader::new(stream.try_clone()?);
        thread::spawn(move || {
            let mut reader = reader;
            loop {
                match protocol::recv(&mut reader) {
                    Ok(Some(request)) => {
                        if tx.send(Event::Request(cid, request)).is_err() {
                            return;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        eprintln!("client {cid}: {e:#}");
                        break;
                    }
                }
            }
            let _ = tx.send(Event::Gone(cid));
        });
        let (out, queue) = mpsc::channel::<Arc<Vec<u8>>>();
        let mut socket = stream.try_clone()?;
        // ends once the client is dropped and its queue drained
        let writer = thread::spawn(move || {
            for msg in queue {
                if socket.write_all(&msg).is_err() {
                    break;
                }
            }
            let _ = socket.shutdown(Shutdown::Both);
        });
        self.clients.insert(cid, Client { out, stream, writer, attached: false, focus: None, size: self.size });
        Ok(())
    }

    fn output(&mut self, id: u64, bytes: Vec<u8>) {
        let viewed = self.clients.values().any(|c| c.attached && c.focus == Some(id));
        let Some(t) = self.mux.get_mut(id) else { return };
        let flagged = match t.feed(&bytes, viewed) {
            Ok(flagged) => flagged,
            Err(e) => {
                eprintln!("terminal {id}: {e:#}");
                false
            }
        };
        let flags = Reply::Flags { id, activity: t.activity, bell: t.bell };
        self.broadcast(&Reply::Output { id, bytes });
        if flagged {
            self.broadcast(&flags);
        }
    }

    fn request(&mut self, cid: usize, request: Request) -> Result<()> {
        match request {
            Request::Hello { version, cols, rows, start } => self.hello(cid, version, (cols, rows), start)?,
            Request::Input { id, bytes } => {
                self.latest(cid)?;
                if let Some(t) = self.mux.get_mut(id) {
                    t.write(&bytes)?;
                }
            }
            Request::Carry { id, at } => {
                if let Some(t) = self.mux.get_mut(id) {
                    t.carry(at)?;
                }
            }
            Request::Resize { cols, rows } => {
                if let Some(c) = self.clients.get_mut(&cid) {
                    c.size = (cols, rows);
                }
                self.resize((cols, rows))?;
            }
            Request::Focus { id } => self.focus(cid, id),
            Request::Write { base, lines, open } => {
                let (ids, errors) = self.mux.write(&base, &lines);
                self.lists(Some(cid), open.and_then(|row| ids.get(row).copied().flatten()));
                if let Some(e) = errors.first() {
                    self.send(cid, &Reply::Message { text: format!("spawn failed: {e}") });
                }
            }
            Request::Spawn { name, from } => {
                let cwd = from.and_then(|id| self.mux.get(id)).map_or(self.mux.cwd.clone(), Term::cwd);
                let id = self.mux.spawn(Saved::new(name, cwd)).context("spawn failed")?;
                self.lists(Some(cid), Some(id));
            }
            Request::Kill { id } => {
                if let Some(mut t) = self.mux.remove(id) {
                    t.kill();
                    self.lists(None, None);
                }
            }
            Request::Rename { id, name } => {
                if self.mux.rename(id, name).is_some() {
                    self.lists(None, None);
                }
            }
            Request::Save { workflow, cursor } => {
                let name = workflow.or(self.workflow.take()).context("E32: No workflow name")?;
                let saved = workflow::save(&name, Session { cursor, terms: self.mux.saved() });
                self.workflow = Some(name.clone());
                self.lists(None, None);
                saved?;
                self.send(cid, &Reply::Message { text: format!("\"{name}\" {} terminals written", self.mux.len()) });
            }
            Request::Refresh => {
                let list = self.list(None);
                self.send(cid, &Reply::List(list));
            }
            Request::Info => {
                let clients = self.clients.values().filter(|c| c.attached).count();
                self.send(cid, &Reply::Info { name: self.name.clone(), terms: self.mux.len(), clients });
            }
            Request::Detach => self.bye(cid, "detached"),
            Request::KillSession { cursor } => self.kill_session(cursor),
        }
        Ok(())
    }

    /// Attaches a client: the first spawns the session from `start`, later ones get every screen as it is.
    fn hello(&mut self, cid: usize, version: u32, size: (u16, u16), start: Option<Start>) -> Result<()> {
        if version != protocol::VERSION {
            let reason = format!("session {} runs vix protocol {}, this vix {version}: kill it or use that binary", self.name, protocol::VERSION);
            self.bye(cid, &reason);
            return Ok(());
        }
        let Some(client) = self.clients.get_mut(&cid) else { return Ok(()) };
        client.size = size;
        self.resize(size)?;
        let focus = if self.started {
            let shrunk = self.mux.shrink()?;
            let tx = self.tx.clone();
            thread::spawn(move || {
                thread::sleep(NUDGE);
                let _ = tx.send(Event::Fit(shrunk));
            });
            self.last.filter(|id| self.mux.alive(*id))
        } else {
            self.started = true;
            let start = start.unwrap_or_default();
            self.workflow = start.workflow.clone();
            self.mux.open(start.session().terms)?;
            None
        };
        let screens = self.mux.exact().into_iter().map(|(id, bytes)| Screen { id, bytes }).collect();
        let (cols, rows) = self.size;
        self.send(cid, &Reply::Attached { cols, rows, list: self.list(None), screens, focus });
        if let Some(c) = self.clients.get_mut(&cid) {
            c.attached = true;
        }
        Ok(())
    }

    /// The terminal size follows the client that typed last.
    fn latest(&mut self, cid: usize) -> Result<()> {
        match self.clients.get(&cid) {
            Some(c) if c.attached => self.resize(c.size),
            _ => Ok(()),
        }
    }

    /// Resizes every terminal, telling clients at that point in the stream so replicas resize at the same byte.
    fn resize(&mut self, size: (u16, u16)) -> Result<()> {
        if size == self.size {
            return Ok(());
        }
        self.size = size;
        self.mux.resize(size.0, size.1)?;
        self.broadcast(&Reply::Resized { cols: size.0, rows: size.1 });
        Ok(())
    }

    /// What a client views isn't flagged for anyone, and viewing clears its flags.
    fn focus(&mut self, cid: usize, id: Option<u64>) {
        if let Some(c) = self.clients.get_mut(&cid) {
            c.focus = id;
        }
        let Some(id) = id else { return };
        self.last = Some(id);
        let Some(t) = self.mux.get_mut(id) else { return };
        if t.activity || t.bell {
            t.seen();
            self.broadcast(&Reply::Flags { id, activity: false, bell: false });
        }
    }

    /// Saves the `--resume` snapshot, kills every terminal, and says bye to everyone.
    fn kill_session(&mut self, cursor: usize) {
        let snapshot = Session { cursor, terms: self.mux.dumped() };
        self.mux.kill_all();
        if !snapshot.terms.is_empty()
            && let Err(e) = session::save_to(&self.snapshot, &snapshot)
        {
            eprintln!("saving the session: {e:#}");
        }
        self.broadcast(&Reply::Bye { reason: "killed".into() });
        self.killed = true;
    }

    fn list(&self, open: Option<u64>) -> List {
        let terms = self
            .mux
            .terms()
            .iter()
            .map(|t| Entry { id: t.id, name: t.name.clone(), cwd: t.cwd(), cmd: t.cmd.clone(), activity: t.activity, bell: t.bell })
            .collect();
        List { terms, workflow: self.workflow.clone(), open }
    }

    /// The list to every client, with `open` only for the one that asked.
    fn lists(&mut self, asker: Option<usize>, open: Option<u64>) {
        let plain = Arc::new(protocol::encode(&Reply::List(self.list(None))));
        let opened = Arc::new(protocol::encode(&Reply::List(self.list(open))));
        for (cid, c) in &self.clients {
            if c.attached || asker == Some(*cid) {
                let _ = c.out.send(if asker == Some(*cid) { opened.clone() } else { plain.clone() });
            }
        }
    }

    fn send(&self, cid: usize, reply: &Reply) {
        if let Some(c) = self.clients.get(&cid) {
            let _ = c.out.send(Arc::new(protocol::encode(reply)));
        }
    }

    /// To every attached client, encoded once.
    fn broadcast(&self, reply: &Reply) {
        let msg = Arc::new(protocol::encode(reply));
        for c in self.clients.values().filter(|c| c.attached) {
            let _ = c.out.send(msg.clone());
        }
    }

    /// Says why, then drops the client once that's sent.
    fn bye(&mut self, cid: usize, reason: &str) {
        self.send(cid, &Reply::Bye { reason: reason.into() });
        self.clients.remove(&cid);
    }

    /// Lets every client's writer finish, but not wait forever on one that stopped reading.
    fn close_all(&mut self) {
        for (_, c) in self.clients.drain() {
            let _ = c.stream.set_write_timeout(Some(BYE_TIMEOUT));
            drop(c.out);
            let _ = c.writer.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Line;
    use crate::vt;

    const TIMEOUT: Duration = Duration::from_secs(5);

    struct TestClient {
        stream: UnixStream,
        reader: BufReader<UnixStream>,
    }

    impl TestClient {
        fn connect(path: &Path) -> Self {
            let stream = UnixStream::connect(path).unwrap();
            stream.set_read_timeout(Some(TIMEOUT)).unwrap();
            let reader = BufReader::new(stream.try_clone().unwrap());
            Self { stream, reader }
        }

        fn hello(path: &Path, start: Option<Start>) -> (Self, Reply) {
            let mut c = Self::connect(path);
            c.send(Request::Hello { version: protocol::VERSION, cols: 40, rows: 10, start });
            let attached = c.recv().unwrap();
            (c, attached)
        }

        fn send(&mut self, request: Request) {
            protocol::send(&mut self.stream, &request).unwrap();
        }

        /// The next reply, or None once the server hung up.
        fn recv(&mut self) -> Option<Reply> {
            protocol::recv(&mut self.reader).unwrap()
        }

        /// Skips replies until one `f` picks.
        fn until<T>(&mut self, mut f: impl FnMut(Reply) -> Option<T>) -> T {
            loop {
                if let Some(t) = f(self.recv().expect("server hung up")) {
                    return t;
                }
            }
        }

        fn list(&mut self) -> List {
            self.until(|r| if let Reply::List(l) = r { Some(l) } else { None })
        }
    }

    fn names(list: &List) -> Vec<&str> {
        list.terms.iter().map(|t| t.name.as_str()).collect()
    }

    fn shows(screen: &mut vt::Parser, line: &str) -> bool {
        vt::text(screen.screen_mut()).iter().any(|l| l.trim() == line)
    }

    fn start(dir: &Path) -> (PathBuf, PathBuf, JoinHandle<Result<()>>) {
        let path = dir.join("t.sock");
        let last = dir.join("last.toml");
        let listener = sock::bind(&path).unwrap();
        let (p, l) = (path.clone(), last.clone());
        (path, last, thread::spawn(move || serve(listener, &p, "t", l)))
    }

    fn one(name: &str) -> Option<Start> {
        Some(Start::new(Session { cursor: 0, terms: vec![Saved::new(name, "/")] }, None))
    }

    #[test]
    fn serves_clients_until_killed() {
        let tmp = tempfile::Builder::new().prefix("vx").tempdir_in("/tmp").unwrap();
        let (path, last, server) = start(tmp.path());
        // probing whether it's alive doesn't count as a client
        drop(sock::connect(&path).unwrap());

        let (mut a, attached) = TestClient::hello(&path, one("sh"));
        let Reply::Attached { list, screens, focus, cols: 40, rows: 10 } = attached else { panic!("{attached:?}") };
        assert_eq!((names(&list), screens.len(), focus), (vec!["sh"], 1, None));
        let sh = list.terms[0].id;
        let mut replica = vt::parser(40, 10);
        replica.process(&screens[0].bytes);
        a.send(Request::Input { id: sh, bytes: b"echo hi\r".to_vec() });
        a.until(|r| {
            if let Reply::Output { bytes, .. } = r {
                replica.process(&bytes);
            }
            shows(&mut replica, "hi").then_some(())
        });

        // a second client gets the same screen
        let (mut b, attached) = TestClient::hello(&path, one("ignored"));
        let Reply::Attached { list, screens, .. } = attached else { panic!("{attached:?}") };
        assert_eq!(names(&list), ["sh"]);
        let mut copy = vt::parser(40, 10);
        copy.process(&screens[0].bytes);
        assert!(shows(&mut copy, "hi"));

        // only the client that spawned is told to open it
        b.send(Request::Spawn { name: "logs".into(), from: Some(sh) });
        let list = b.list();
        assert_eq!(names(&list), ["sh", "logs"]);
        let logs = list.open.expect("opens what it spawned");
        assert_eq!(a.list().open, None);

        // :w renames sh, kills logs, spawns api, and opens it
        let lines = vec![Line { id: Some(sh), text: "web".into() }, Line { id: None, text: "api".into() }];
        a.send(Request::Write { base: vec![sh, logs], lines, open: Some(1) });
        let list = a.list();
        assert_eq!(names(&list), ["web", "api"]);
        let api = list.open.expect("opens the written row");
        assert_eq!(names(&b.list()), ["web", "api"]);

        a.send(Request::Kill { id: api });
        assert_eq!(names(&a.list()), ["web"]);
        a.send(Request::Rename { id: sh, name: "www".into() });
        assert_eq!(names(&a.list()), ["www"]);

        a.send(Request::Info);
        assert_eq!(a.until(|r| if let Reply::Info { terms, clients, .. } = r { Some((terms, clients)) } else { None }), (1, 2));
        b.send(Request::Detach);
        assert_eq!(b.until(|r| if let Reply::Bye { reason } = r { Some(reason) } else { None }), "detached");
        assert!(b.recv().is_none());

        a.send(Request::Focus { id: Some(sh) });
        a.send(Request::KillSession { cursor: 0 });
        assert_eq!(a.until(|r| if let Reply::Bye { reason } = r { Some(reason) } else { None }), "killed");
        server.join().unwrap().unwrap();
        assert!(!path.exists());
        assert!(fs::read_to_string(last).unwrap().contains("name = \"www\""));
    }

    #[test]
    fn ends_once_detached_and_empty() {
        let tmp = tempfile::Builder::new().prefix("vx").tempdir_in("/tmp").unwrap();
        let (path, _, server) = start(tmp.path());
        let (mut a, attached) = TestClient::hello(&path, one("sh"));
        let Reply::Attached { list, .. } = attached else { panic!("{attached:?}") };

        let mut old = TestClient::connect(&path);
        old.send(Request::Hello { version: protocol::VERSION + 1, cols: 40, rows: 10, start: None });
        assert!(matches!(old.recv(), Some(Reply::Bye { reason }) if reason.contains("kill it or use that binary")));

        // hanging up is detaching; the session outlives it until its last terminal exits
        a.send(Request::Input { id: list.terms[0].id, bytes: b"sleep 0.5; exit\r".to_vec() });
        drop(a);
        thread::sleep(Duration::from_millis(100));
        assert!(path.exists());
        server.join().unwrap().unwrap();
        assert!(!path.exists());
    }
}
