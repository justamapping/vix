//! The client's side of a session: a copy of each terminal's screen, kept up by what the server sends.

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use anyhow::{Result, bail};

use crate::protocol::{self, List, Reply, Request, Screen, Start};
use crate::vt;

// the server answers Hello at once; longer means it's stuck
const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);

/// A terminal as the server last described it, with a replica of its screen.
pub struct Term {
    pub id: u64,
    pub name: String,
    /// where its shell was as of the last list
    pub cwd: PathBuf,
    pub vt: vt::Parser,
    pub activity: bool,
    pub bell: bool,
}

impl Term {
    /// Parses output. The server already answered the program's queries and flagged its bells.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.vt.process(bytes);
        let callbacks = self.vt.callbacks_mut();
        callbacks.replies.clear();
        callbacks.bell = false;
    }

    /// Clears activity and bell, now that it's been seen.
    pub fn seen(&mut self) {
        (self.activity, self.bell) = (false, false);
    }
}

/// The session as this client sees it: terminals in list order, and the socket to ask the server for changes.
pub struct Remote {
    terms: Vec<Term>,
    stream: UnixStream,
    pub workflow: Option<String>,
    cols: u16,
    rows: u16,
}

impl Remote {
    /// Says Hello on `stream`, starting the session from `start` if it's new. Returns the session, the terminal to
    /// land on, and the reader for the rest of what the server sends.
    pub fn attach(stream: UnixStream, cols: u16, rows: u16, start: Option<Start>) -> Result<(Self, Option<u64>, BufReader<UnixStream>)> {
        protocol::send(&mut &stream, &Request::Hello { version: protocol::VERSION, cols, rows, start })?;
        let mut reader = BufReader::new(stream.try_clone()?);
        // macOS refuses these once the server hung up, which the read then reports
        let _ = stream.set_read_timeout(Some(ATTACH_TIMEOUT));
        let reply = protocol::recv(&mut reader)?;
        let _ = stream.set_read_timeout(None);
        match reply {
            Some(Reply::Attached { cols, rows, list, screens, focus }) => {
                let mut remote = Self { terms: Vec::new(), stream, workflow: None, cols, rows };
                remote.sync(&list);
                remote.screens(screens);
                Ok((remote, focus, reader))
            }
            Some(Reply::Bye { reason }) => bail!(reason),
            Some(other) => bail!("expected to attach, the server said {other:?}"),
            None => bail!("the server hung up"),
        }
    }

    /// Sends a request without waiting; a server that's gone shows up as the reader's end instead.
    pub fn send(&self, request: &Request) {
        let _ = protocol::send(&mut &self.stream, request);
    }

    pub fn input(&self, id: u64, bytes: Vec<u8>) {
        self.send(&Request::Input { id, bytes });
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

    /// Matches the replicas to `list`. New terminals start blank, since their output follows the list. Returns the
    /// ones that are gone.
    pub fn sync(&mut self, list: &List) -> Vec<Term> {
        let mut old = std::mem::take(&mut self.terms);
        for e in &list.terms {
            let t = match old.iter().position(|t| t.id == e.id) {
                Some(i) => old.remove(i),
                None => Term { id: e.id, name: String::new(), cwd: PathBuf::new(), vt: vt::parser(self.cols, self.rows), activity: false, bell: false },
            };
            self.terms.push(Term { name: e.name.clone(), cwd: e.cwd.clone(), activity: e.activity, bell: e.bell, ..t });
        }
        self.workflow = list.workflow.clone();
        old
    }

    /// Replaces replicas with exact copies, on attach or after falling behind.
    pub fn screens(&mut self, screens: Vec<Screen>) {
        let (cols, rows) = (self.cols, self.rows);
        for s in screens {
            if let Some(t) = self.get_mut(s.id) {
                t.vt = vt::parser(cols, rows);
                t.feed(&s.bytes);
            }
        }
    }

    /// The server resized every terminal at this point in their output.
    pub fn resized(&mut self, cols: u16, rows: u16) {
        (self.cols, self.rows) = (cols, rows);
        for t in &mut self.terms {
            t.vt.screen_mut().set_size(rows, cols);
        }
    }
}

/// Forwards the server's replies as events until it hangs up, then sends `end`.
pub fn listen<E: Send + 'static>(mut reader: BufReader<UnixStream>, tx: Sender<E>, wrap: impl Fn(Reply) -> E + Send + 'static, end: E) {
    thread::spawn(move || {
        loop {
            match protocol::recv(&mut reader) {
                Ok(Some(reply)) => {
                    if tx.send(wrap(reply)).is_err() {
                        return;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    crate::log::log(format_args!("server: {e:#}"));
                    break;
                }
            }
        }
        let _ = tx.send(end);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Entry;
    use std::sync::mpsc;

    fn entry(id: u64, name: &str) -> Entry {
        Entry { id, name: name.into(), cwd: "/tmp".into(), cmd: None, fg: None, activity: false, bell: false }
    }

    fn list(entries: Vec<Entry>) -> List {
        List { terms: entries, workflow: None, open: None }
    }

    /// A fake server that answers Hello with `reply`, then sends `rest`.
    fn server(reply: Reply, rest: Vec<Reply>) -> UnixStream {
        let (client, server) = UnixStream::pair().unwrap();
        thread::spawn(move || {
            let hello: Option<Request> = protocol::recv(&mut BufReader::new(&server)).unwrap();
            assert!(matches!(hello, Some(Request::Hello { cols: 20, rows: 5, start: None, .. })), "{hello:?}");
            for r in std::iter::once(reply).chain(rest) {
                protocol::send(&mut &server, &r).unwrap();
            }
        });
        client
    }

    fn names(remote: &Remote) -> Vec<&str> {
        remote.terms().iter().map(|t| t.name.as_str()).collect()
    }

    #[test]
    fn attach_copies_screens_then_listens() {
        let screens = vec![Screen { id: 1, bytes: b"hello\x1b[6n\x07".to_vec() }];
        let attached = Reply::Attached { cols: 20, rows: 5, list: list(vec![entry(1, "web"), entry(2, "api")]), screens, focus: Some(2) };
        let stream = server(attached, vec![Reply::Output { id: 1, bytes: b" there".to_vec() }]);
        let (mut remote, focus, reader) = Remote::attach(stream, 20, 5, None).unwrap();
        assert_eq!((names(&remote), focus), (vec!["web", "api"], Some(2)));
        let web = remote.get_mut(1).unwrap();
        assert_eq!(web.vt.screen().contents(), "hello");
        // the server answers queries and flags bells, not the replica
        assert!(web.vt.callbacks_mut().replies.is_empty() && !web.vt.callbacks_mut().bell);

        let (tx, rx) = mpsc::channel();
        listen(reader, tx, Some, None);
        let Some(Reply::Output { id, bytes }) = rx.recv().unwrap() else { panic!() };
        remote.get_mut(id).unwrap().feed(&bytes);
        assert_eq!(remote.get(1).unwrap().vt.screen().contents(), "hello there");
        assert!(rx.recv().unwrap().is_none(), "ends when the server hangs up");
    }

    #[test]
    fn attach_says_why_not() {
        let stream = server(Reply::Bye { reason: "kill it or use that binary".into() }, vec![]);
        let err = Remote::attach(stream, 20, 5, None).err().unwrap();
        assert_eq!(err.to_string(), "kill it or use that binary");
    }

    #[test]
    fn sync_keeps_screens_adds_blanks_and_returns_the_gone() {
        let attached = Reply::Attached { cols: 20, rows: 5, list: list(vec![entry(1, "web"), entry(2, "api")]), screens: vec![], focus: None };
        let (mut remote, _, _reader) = Remote::attach(server(attached, vec![]), 20, 5, None).unwrap();
        remote.get_mut(1).unwrap().feed(b"kept");
        let mut list = list(vec![entry(3, "logs"), Entry { activity: true, ..entry(1, "www") }]);
        list.workflow = Some("demo".into());
        let gone = remote.sync(&list);
        assert_eq!(gone.iter().map(|t| t.id).collect::<Vec<_>>(), [2]);
        assert_eq!(names(&remote), ["logs", "www"]);
        let www = remote.get(1).unwrap();
        assert!(www.activity && www.vt.screen().contents() == "kept");
        assert_eq!(remote.get(3).unwrap().vt.screen().size(), (5, 20));
        assert_eq!(remote.workflow.as_deref(), Some("demo"));

        remote.resized(30, 8);
        assert!(remote.terms().iter().all(|t| t.vt.screen().size() == (8, 30)));

        // resent after falling behind: replaced, not appended
        remote.screens(vec![Screen { id: 1, bytes: b"anew".to_vec() }]);
        assert_eq!(remote.get(1).unwrap().vt.screen().contents(), "anew");
    }
}
