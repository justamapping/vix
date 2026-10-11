//! What a client and its session's server say over the socket: one JSON object per line, bytes in base64.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::Result;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::buffer::Line;
use crate::carry::Spot;
use crate::prompt::Change;
use crate::session::{Saved, Session};

/// Bump on any change to these types; client and server refuse to talk across versions.
pub const VERSION: u32 = 6;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// First message. `start` is what a new session spawns; an existing one ignores it.
    Hello { version: u32, cols: u16, rows: u16, start: Option<Start> },
    Input { id: u64, #[serde(with = "b64")] bytes: Vec<u8> },
    /// moves a line editor's cursor to `spot` on `row`, for `i a I A`
    Carry { id: u64, row: usize, spot: Spot },
    /// retypes a line editor's input, for operators in view n
    Change { id: u64, change: Change },
    Resize { cols: u16, rows: u16 },
    Focus { id: Option<u64> },
    /// `:w` of the list edited from `base`; `open` is a row to open once it's applied
    Write { base: Vec<u64>, lines: Vec<Line>, open: Option<usize> },
    /// `:e`, `:new`; the reply's `List` opens it
    Spawn { name: String, from: Option<u64> },
    Kill { id: u64 },
    Rename { id: u64, name: String },
    /// `cursor` is the asker's list row, saved with the workflow
    Save { workflow: Option<String>, cursor: usize },
    /// fresh cwds, for the picker
    Refresh,
    /// `vix -l`, without attaching
    Info,
    Detach,
    /// `:qa`; `cursor` is the asker's list row, for `--resume`
    KillSession { cursor: usize },
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    /// also sent again, without `focus`, to a client that fell behind and missed output
    Attached { cols: u16, rows: u16, list: List, screens: Vec<Screen>, focus: Option<u64> },
    Output { id: u64, #[serde(with = "b64")] bytes: Vec<u8> },
    /// after any change; `open` only to the client that asked
    List(List),
    Flags { id: u64, activity: bool, bell: bool },
    Resized { cols: u16, rows: u16 },
    Exited { id: u64 },
    Message { text: String },
    /// `since` is when its last client left, or it started, in unix ms; `vix -a` picks the latest
    // defaulted, so `vix -l` still reads servers from before it
    Info { name: String, terms: usize, clients: usize, #[serde(default)] since: u64 },
    /// the server is done with this client: killed, version mismatch
    Bye { reason: String },
}

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct List {
    pub terms: Vec<Entry>,
    pub workflow: Option<String>,
    pub open: Option<u64>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: u64,
    pub name: String,
    pub cwd: PathBuf,
    pub cmd: Option<String>,
    pub activity: bool,
    pub bell: bool,
}

/// A terminal's `dump::exact`, for its replica.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Screen {
    pub id: u64,
    #[serde(with = "b64")]
    pub bytes: Vec<u8>,
}

/// What a new session spawns: a `Session`, with the output toml leaves out.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Start {
    pub workflow: Option<String>,
    pub cursor: usize,
    pub terms: Vec<Seed>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Seed {
    pub name: String,
    pub cwd: PathBuf,
    pub cmd: Option<String>,
    #[serde(with = "b64")]
    pub output: Vec<u8>,
}

impl Start {
    pub fn new(session: Session, workflow: Option<String>) -> Self {
        let terms = session.terms.into_iter().map(|Saved { name, cwd, cmd, output }| Seed { name, cwd, cmd, output }).collect();
        Self { workflow, cursor: session.cursor, terms }
    }

    pub fn session(self) -> Session {
        let terms = self.terms.into_iter().map(|Seed { name, cwd, cmd, output }| Saved { name, cwd, cmd, output }).collect();
        Session { cursor: self.cursor, terms }
    }
}

/// One message as a line, ready to queue for any number of clients.
pub fn encode<T: Serialize>(msg: &T) -> Vec<u8> {
    let mut line = serde_json::to_vec(msg).expect("protocol types always serialize");
    line.push(b'\n');
    line
}

pub fn send<T: Serialize>(w: &mut impl Write, msg: &T) -> Result<()> {
    w.write_all(&encode(msg))?;
    w.flush()?;
    Ok(())
}

/// The next message, or None once the other side hung up.
pub fn recv<T: DeserializeOwned>(r: &mut impl BufRead) -> Result<Option<T>> {
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&line)?))
}

mod b64 {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        STANDARD.decode(String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    fn list() -> List {
        let entry = Entry { id: 3, name: "web".into(), cwd: "/srv".into(), cmd: Some("npm run dev".into()), activity: true, bell: false };
        List { terms: vec![entry], workflow: Some("demo".into()), open: Some(3) }
    }

    fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(msgs: Vec<T>) {
        let mut wire = Vec::new();
        for m in &msgs {
            send(&mut wire, m).unwrap();
        }
        assert_eq!(wire.iter().filter(|&&b| b == b'\n').count(), msgs.len(), "one line each");
        let mut r = BufReader::new(&wire[..]);
        for m in msgs {
            assert_eq!(recv::<T>(&mut r).unwrap(), Some(m));
        }
        assert_eq!(recv::<T>(&mut r).unwrap(), None);
    }

    #[test]
    fn every_request_round_trips() {
        let start = Start::new(Session { cursor: 1, terms: vec![Saved::new("a", "/"), Saved { output: b"old\r\n".to_vec(), ..Saved::new("b", "/tmp") }] }, Some("demo".into()));
        round_trip(vec![
            Request::Hello { version: VERSION, cols: 80, rows: 24, start: Some(start) },
            Request::Hello { version: VERSION, cols: 80, rows: 24, start: None },
            Request::Input { id: 1, bytes: b"echo hi\r\x1b[A\xff".to_vec() },
            Request::Carry { id: 1, row: 2, spot: Spot::At(5) },
            Request::Resize { cols: 100, rows: 30 },
            Request::Focus { id: None },
            Request::Write { base: vec![0, 1], lines: vec![Line { id: Some(1), text: "web".into() }, Line { id: None, text: "api".into() }], open: Some(1) },
            Request::Spawn { name: "logs".into(), from: Some(1) },
            Request::Kill { id: 1 },
            Request::Rename { id: 1, name: "tail".into() },
            Request::Save { workflow: Some("demo".into()), cursor: 1 },
            Request::Refresh,
            Request::Info,
            Request::Detach,
            Request::KillSession { cursor: 0 },
        ]);
    }

    #[test]
    fn every_reply_round_trips() {
        round_trip(vec![
            Reply::Attached { cols: 80, rows: 24, list: list(), screens: vec![Screen { id: 3, bytes: b"\x1b[?1049h\x00".to_vec() }], focus: Some(3) },
            Reply::Output { id: 3, bytes: "日本\n".as_bytes().to_vec() },
            Reply::List(list()),
            Reply::List(List::default()),
            Reply::Flags { id: 3, activity: true, bell: true },
            Reply::Resized { cols: 80, rows: 24 },
            Reply::Exited { id: 3 },
            Reply::Message { text: "\"web\" 3 terminals written".into() },
            Reply::Info { name: "0".into(), terms: 2, clients: 1, since: 1_700_000_000_000 },
            Reply::Bye { reason: "killed".into() },
        ]);
    }

    #[test]
    fn bytes_go_as_base64() {
        let line = encode(&Request::Input { id: 1, bytes: b"\x1b[A\xff".to_vec() });
        assert_eq!(line, b"{\"type\":\"input\",\"id\":1,\"bytes\":\"G1tB/w==\"}\n");
    }

    #[test]
    fn start_keeps_output_toml_drops() {
        let session = Session { cursor: 1, terms: vec![Saved { cmd: Some("ls".into()), output: b"residue".to_vec(), ..Saved::new("a", "/") }] };
        let expected = Session { cursor: 1, terms: session.terms.clone() };
        let start: Start = serde_json::from_slice(&encode(&Start::new(session, None))).unwrap();
        assert_eq!(start.session(), expected);
    }

    #[test]
    fn garbage_is_an_error_not_a_hangup() {
        let mut r = BufReader::new(&b"{\"type\":\"nope\"}\n"[..]);
        assert!(recv::<Request>(&mut r).is_err());
    }
}
