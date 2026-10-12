//! `vix ctl`: drives a running session from scripts and agents, one request at a time, without attaching.

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use regex::{Regex, RegexBuilder};

use crate::keyspec;
use crate::protocol::{self, Entry, Reply, Request, Start};
use crate::sock;
use crate::workflow;

pub const USAGE: &str = "usage: vix ctl [-s session] <verb>
  ls [--json]                          index, %id, name, cwd, cmd, what's running, flags
  new [name] [-c dir] [-- cmd]         prints its %id
  kill <t>
  rename <t> <name>
  send <t> [--raw] <keys>...           vim key notation: 'npm test<CR>', '<C-c>'
  read <t> [--all | --lines n]         the screen as text; --all adds scrollback
  wait <t> --done | --match re | --idle 2s | --exit   [--timeout 30s]
                                       --done: the shell is back at its prompt
  apply <workflow|file.toml> [--prune] spawns what's missing by name
  save [workflow]
  sessions | detach | kill-session
a target <t> is a name, an index from 1, %id, or . for the terminal ctl runs in.
the session is -s, else the one ctl runs in, else the only one running.";

// how often `wait` looks
const POLL: Duration = Duration::from_millis(100);
// how long the shell must be in the foreground with a still screen for `wait --done`, so a command that was just sent has time to start
const SETTLE: Duration = Duration::from_millis(300);

#[derive(Debug, PartialEq)]
pub enum Cmd {
    Ls { json: bool },
    New { name: String, cwd: Option<PathBuf>, cmd: Option<String> },
    Kill(String),
    Rename(String, String),
    Send { target: String, keys: String, raw: bool },
    Read { target: String, lines: Lines },
    Wait { target: String, until: Until, timeout: Option<Duration> },
    Apply { workflow: String, prune: bool },
    Save(Option<String>),
    Sessions,
    Detach,
    KillSession,
}

#[derive(Debug, PartialEq)]
pub enum Lines {
    Screen,
    All,
    Last(usize),
}

#[derive(Debug, PartialEq)]
pub enum Until {
    Done,
    Match(String),
    Idle(Duration),
    Exit,
}

/// Positionals and flags of one verb. Flags in `valued` take the next arg; everything after `--` is the tail.
struct Args {
    pos: Vec<String>,
    flags: Vec<(String, Option<String>)>,
    tail: Option<String>,
}

impl Args {
    fn split(args: Vec<String>, valued: &[&str]) -> Result<Self> {
        let mut parsed = Args { pos: Vec::new(), flags: Vec::new(), tail: None };
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            if arg == "--" {
                parsed.tail = Some(args.collect::<Vec<_>>().join(" "));
                break;
            }
            if arg.starts_with('-') && arg.len() > 1 {
                let value = if valued.contains(&arg.as_str()) { Some(args.next().with_context(|| format!("{arg} needs a value"))?) } else { None };
                parsed.flags.push((arg, value));
            } else {
                parsed.pos.push(arg);
            }
        }
        Ok(parsed)
    }

    fn flag(&mut self, name: &str) -> bool {
        self.value(name).is_some()
    }

    fn value(&mut self, name: &str) -> Option<String> {
        let i = self.flags.iter().position(|(f, _)| f == name)?;
        Some(self.flags.remove(i).1.unwrap_or_default())
    }

    /// The positionals, once every flag was asked for, if there are `min..=max` of them.
    fn done(self, min: usize, max: usize) -> Result<Vec<String>> {
        if let Some((flag, _)) = self.flags.first() {
            bail!("unknown flag {flag}");
        }
        if self.tail.is_some() {
            bail!("unexpected --");
        }
        if !(min..=max).contains(&self.pos.len()) {
            bail!("wrong number of arguments");
        }
        Ok(self.pos)
    }
}

/// The session `-s` names, if any, and the verb.
pub fn parse(args: Vec<String>) -> Result<(Option<String>, Cmd)> {
    let mut args = args.into_iter().peekable();
    let session = if args.next_if(|a| a == "-s").is_some() { Some(args.next().context("-s needs a session")?) } else { None };
    let verb = args.next().context("no verb")?;
    let mut a = Args::split(args.collect(), &["-c", "--lines", "--match", "--idle", "--timeout"])?;
    let cmd = match verb.as_str() {
        "ls" => {
            let json = a.flag("--json");
            a.done(0, 0)?;
            Cmd::Ls { json }
        }
        "new" => {
            let cwd = a.value("-c").map(PathBuf::from);
            let cmd = a.tail.take();
            let name = a.done(0, 1)?.pop().unwrap_or_else(|| "Untitled".into());
            Cmd::New { name, cwd, cmd }
        }
        "kill" => Cmd::Kill(a.done(1, 1)?.remove(0)),
        "rename" => {
            let mut pos = a.done(2, 2)?;
            Cmd::Rename(pos.remove(0), pos.remove(0))
        }
        "send" => {
            let raw = a.flag("--raw");
            let mut pos = a.done(2, usize::MAX)?;
            let target = pos.remove(0);
            Cmd::Send { target, keys: pos.concat(), raw }
        }
        "read" => {
            let lines = match (a.flag("--all"), a.value("--lines")) {
                (false, None) => Lines::Screen,
                (true, None) => Lines::All,
                (false, Some(n)) => Lines::Last(n.parse().context("--lines takes a number")?),
                (true, Some(_)) => bail!("--all or --lines, not both"),
            };
            Cmd::Read { target: a.done(1, 1)?.remove(0), lines }
        }
        "wait" => {
            let timeout = a.value("--timeout").map(|t| duration(&t)).transpose()?;
            let until = match (a.flag("--done"), a.value("--match"), a.value("--idle"), a.flag("--exit")) {
                (true, None, None, false) => Until::Done,
                (false, Some(re), None, false) => Until::Match(re),
                (false, None, Some(d), false) => Until::Idle(duration(&d)?),
                (false, None, None, true) => Until::Exit,
                _ => bail!("wait for one of --done, --match, --idle, --exit"),
            };
            Cmd::Wait { target: a.done(1, 1)?.remove(0), until, timeout }
        }
        "apply" => {
            let prune = a.flag("--prune");
            Cmd::Apply { workflow: a.done(1, 1)?.remove(0), prune }
        }
        "save" => Cmd::Save(a.done(0, 1)?.pop()),
        "sessions" | "detach" | "kill-session" => {
            a.done(0, 0)?;
            match verb.as_str() {
                "sessions" => Cmd::Sessions,
                "detach" => Cmd::Detach,
                _ => Cmd::KillSession,
            }
        }
        _ => bail!("unknown verb {verb}"),
    };
    Ok((session, cmd))
}

/// `2s`, `500ms`, `1m`, or plain seconds.
pub fn duration(s: &str) -> Result<Duration> {
    let split = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let n: f64 = s[..split].parse().with_context(|| format!("bad duration {s}"))?;
    let secs = match &s[split..] {
        "ms" => n / 1000.0,
        "" | "s" => n,
        "m" => n * 60.0,
        _ => bail!("bad duration {s}"),
    };
    Ok(Duration::from_secs_f64(secs))
}

/// The terminal `target` names in `terms`: a name, an index from 1, `%id`, or `.` for `own`.
pub fn resolve(terms: &[Entry], target: &str, own: Option<u64>) -> Result<u64> {
    let id = if target == "." {
        own.context("not inside this session, so . means nothing")?
    } else if let Some(id) = target.strip_prefix('%') {
        id.parse().with_context(|| format!("bad id {target}"))?
    } else if let Ok(index) = target.parse::<usize>() {
        terms.get(index.wrapping_sub(1)).with_context(|| format!("no terminal at {index}"))?.id
    } else {
        let named: Vec<&Entry> = terms.iter().filter(|t| t.name == target).collect();
        match named[..] {
            [t] => t.id,
            [] => bail!("no terminal named {target}"),
            _ => bail!("{} terminals named {target}, use an index or %id", named.len()),
        }
    };
    if !terms.iter().any(|t| t.id == id) {
        bail!("no terminal {target}");
    }
    Ok(id)
}

/// The session to talk to: `-s`, else the one ctl runs in, else the only one.
pub fn pick(flag: Option<String>, inside: Option<String>, live: &[String]) -> Result<String> {
    if let Some(name) = flag.or(inside) {
        return Ok(name);
    }
    match live {
        [name] => Ok(name.clone()),
        [] => bail!("no sessions"),
        _ => bail!("{} sessions ({}), pick one with -s", live.len(), live.join(", ")),
    }
}

/// Lines as `read` prints them: blank rows at the bottom dropped, then the last `n` with `Last`.
pub fn trim(mut lines: Vec<String>, n: Option<usize>) -> Vec<String> {
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let skip = n.map_or(0, |n| lines.len().saturating_sub(n));
    lines.split_off(skip)
}

/// One row per terminal for `ls`.
pub fn table(terms: &[Entry]) -> Vec<String> {
    let width = terms.iter().map(|t| t.name.chars().count()).max().unwrap_or(0);
    terms
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut row = format!("{}  %{:<3} {:<width$}  {}", i + 1, t.id, t.name, workflow::tilde(&t.cwd));
            if let Some(cmd) = &t.cmd {
                row += &format!("  $ {cmd}");
            }
            if let Some(fg) = &t.fg {
                row += &format!("  [running {fg}]");
            }
            if t.activity {
                row += "  [activity]";
            }
            if t.bell {
                row += "  [bell]";
            }
            row
        })
        .collect()
}

struct Conn {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Conn {
    /// Connects without attaching, after checking both sides speak the same protocol.
    fn open(path: &Path, name: &str) -> Result<Self> {
        let stream = sock::connect(path)?.with_context(|| format!("no session {name}"))?;
        let mut conn = Conn { reader: BufReader::new(stream.try_clone()?), stream };
        protocol::send(&mut conn.stream, &Request::Info)?;
        match protocol::recv(&mut conn.reader)? {
            Some(Reply::Info { version, .. }) if version == protocol::VERSION => Ok(conn),
            Some(Reply::Info { version, .. }) => {
                bail!("session {name} runs vix protocol {version}, this vix {}: kill it or use that binary", protocol::VERSION)
            }
            _ => bail!("no answer from session {name}"),
        }
    }

    /// Sends `request` and returns its replies: everything until the answer to an `Info` sent after it, or the
    /// server hanging up. Fails on an `Error`.
    fn ask(&mut self, request: &Request) -> Result<Vec<Reply>> {
        protocol::send(&mut self.stream, request)?;
        protocol::send(&mut self.stream, &Request::Info)?;
        let mut replies = Vec::new();
        while let Some(reply) = protocol::recv(&mut self.reader)? {
            match reply {
                Reply::Info { .. } => break,
                Reply::Error { text } => bail!(text),
                reply => replies.push(reply),
            }
        }
        Ok(replies)
    }

    fn list(&mut self) -> Result<Vec<Entry>> {
        match self.ask(&Request::Refresh)?.into_iter().next() {
            Some(Reply::List(list)) => Ok(list.terms),
            _ => bail!("no list"),
        }
    }

    fn read(&mut self, id: u64, scrollback: bool) -> Result<Vec<String>> {
        match self.ask(&Request::Read { id, scrollback })?.into_iter().next() {
            Some(Reply::Text { lines }) => Ok(lines),
            _ => bail!("no text"),
        }
    }
}

fn print_messages(replies: Vec<Reply>) {
    for reply in replies {
        if let Reply::Message { text } = reply {
            println!("{text}");
        }
    }
}

pub fn run(args: Vec<String>) -> Result<()> {
    if args.first().is_none_or(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return Ok(());
    }
    let (session, cmd) = parse(args).map_err(|e| anyhow::anyhow!("{e:#}\n{USAGE}"))?;
    if cmd == Cmd::Sessions {
        return crate::list_sessions();
    }
    let dir = sock::dir()?;
    let inside = sock::inside(&dir, std::env::var_os("VIX_SOCKET").as_deref().map(Path::new));
    let live: Vec<String> = if session.is_some() || inside.is_some() { Vec::new() } else { sock::sessions(&dir)?.into_iter().map(|(n, _)| n).collect() };
    let name = pick(session, inside.clone(), &live)?;
    let own = if inside.as_deref() == Some(name.as_str()) { std::env::var("VIX_ID").ok().and_then(|id| id.parse().ok()) } else { None };
    let mut conn = Conn::open(&sock::path(&dir, &name)?, &name)?;
    let target = |conn: &mut Conn, t: &str| conn.list().and_then(|terms| resolve(&terms, t, own));
    match cmd {
        Cmd::Ls { json } => {
            let terms = conn.list()?;
            if json {
                println!("{}", serde_json::to_string(&terms)?);
            } else {
                table(&terms).iter().for_each(|row| println!("{row}"));
            }
        }
        Cmd::New { name, cwd, cmd } => {
            let cwd = cwd.unwrap_or(std::env::current_dir()?);
            if !cwd.is_dir() {
                bail!("no directory {}", cwd.display());
            }
            let cwd = std::path::absolute(cwd)?;
            let replies = conn.ask(&Request::Spawn { name, from: None, cwd: Some(cwd), cmd })?;
            match replies.into_iter().find_map(|r| if let Reply::List(l) = r { l.open } else { None }) {
                Some(id) => println!("%{id}"),
                None => bail!("spawn failed"),
            }
        }
        Cmd::Kill(t) => {
            let id = target(&mut conn, &t)?;
            conn.ask(&Request::Kill { id })?;
        }
        Cmd::Rename(t, name) => {
            let id = target(&mut conn, &t)?;
            conn.ask(&Request::Rename { id, name })?;
        }
        Cmd::Send { target: t, keys, raw } => {
            let id = target(&mut conn, &t)?;
            let bytes = if raw { keys.into_bytes() } else { keyspec::bytes(&keyspec::parse(&keys)?) };
            conn.ask(&Request::Input { id, bytes })?;
        }
        Cmd::Read { target: t, lines } => {
            let id = target(&mut conn, &t)?;
            let text = conn.read(id, lines != Lines::Screen)?;
            let n = if let Lines::Last(n) = lines { Some(n) } else { None };
            trim(text, n).iter().for_each(|l| println!("{l}"));
        }
        Cmd::Wait { target: t, until, timeout } => {
            // ids aren't reused, so one that's gone already exited
            let id = match (&until, t.strip_prefix('%').and_then(|id| id.parse().ok())) {
                (Until::Exit, Some(id)) => id,
                _ => target(&mut conn, &t)?,
            };
            wait(&mut conn, id, until, timeout)?;
        }
        Cmd::Apply { workflow: w, prune } => {
            let session = if w.contains('/') || w.ends_with(".toml") { workflow::load_from(Path::new(&w))? } else { workflow::load(&w)? };
            let terms = Start::new(session.with_context(|| format!("no workflow {w}"))?, None).terms;
            print_messages(conn.ask(&Request::Apply { terms, prune })?);
        }
        Cmd::Save(workflow) => print_messages(conn.ask(&Request::Save { workflow, cursor: 0 })?),
        Cmd::Detach => _ = conn.ask(&Request::DetachAll)?,
        Cmd::KillSession => _ = conn.ask(&Request::KillSession { cursor: 0 })?,
        Cmd::Sessions => unreachable!("handled before connecting"),
    }
    Ok(())
}

/// Polls until `until` holds for terminal `id`, or `timeout` passes.
fn wait(conn: &mut Conn, id: u64, until: Until, timeout: Option<Duration>) -> Result<()> {
    let start = Instant::now();
    let re = match &until {
        Until::Match(re) => Some(RegexBuilder::new(re).multi_line(true).build()?),
        _ => None,
    };
    let mut last: (Vec<String>, Instant) = (Vec::new(), start);
    // how long the screen has looked the same
    let mut still = |conn: &mut Conn| -> Result<Duration> {
        let text = conn.read(id, false)?;
        if text != last.0 {
            last = (text, Instant::now());
        }
        Ok(last.1.elapsed())
    };
    loop {
        let done = match &until {
            Until::Done => {
                let quiet = still(conn)?;
                let term = conn.list()?.into_iter().find(|t| t.id == id).context("the terminal exited")?;
                term.fg.is_none() && quiet >= SETTLE
            }
            Until::Exit => !conn.list()?.iter().any(|t| t.id == id),
            Until::Match(_) => matches(re.as_ref().expect("built above"), &conn.read(id, false)?),
            Until::Idle(idle) => still(conn)? >= *idle,
        };
        if done {
            return Ok(());
        }
        if timeout.is_some_and(|t| start.elapsed() >= t) {
            bail!("timed out");
        }
        thread::sleep(POLL);
    }
}

/// Whether `re` matches the screen, rows joined by newlines so `^` and `$` work per row.
pub fn matches(re: &Regex, lines: &[String]) -> bool {
    re.is_match(&lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    fn entry(id: u64, name: &str) -> Entry {
        Entry { id, name: name.into(), cwd: "/".into(), cmd: None, fg: None, activity: false, bell: false }
    }

    #[test]
    fn parses_verbs() {
        assert_eq!(parse(args("ls")).unwrap(), (None, Cmd::Ls { json: false }));
        assert_eq!(parse(args("-s web ls --json")).unwrap(), (Some("web".into()), Cmd::Ls { json: true }));
        assert_eq!(
            parse(args("new dev -c ~/srv -- npm run dev")).unwrap().1,
            Cmd::New { name: "dev".into(), cwd: Some("~/srv".into()), cmd: Some("npm run dev".into()) }
        );
        assert_eq!(parse(args("new")).unwrap().1, Cmd::New { name: "Untitled".into(), cwd: None, cmd: None });
        assert_eq!(parse(args("rename 2 api")).unwrap().1, Cmd::Rename("2".into(), "api".into()));
        assert_eq!(parse(args("send web ls <CR>")).unwrap().1, Cmd::Send { target: "web".into(), keys: "ls<CR>".into(), raw: false });
        assert_eq!(parse(args("read . --lines 5")).unwrap().1, Cmd::Read { target: ".".into(), lines: Lines::Last(5) });
        assert_eq!(
            parse(args("wait %3 --idle 500ms --timeout 1m")).unwrap().1,
            Cmd::Wait { target: "%3".into(), until: Until::Idle(Duration::from_millis(500)), timeout: Some(Duration::from_secs(60)) }
        );
        assert_eq!(parse(args("wait . --done")).unwrap().1, Cmd::Wait { target: ".".into(), until: Until::Done, timeout: None });
        assert_eq!(parse(args("apply web --prune")).unwrap().1, Cmd::Apply { workflow: "web".into(), prune: true });
        assert_eq!(parse(args("save")).unwrap().1, Cmd::Save(None));
    }

    #[test]
    fn rejects_bad_args() {
        for bad in ["", "frob", "kill", "kill a b", "ls --bogus", "ls extra", "read a --all --lines 2", "wait a", "wait a --exit --idle 1s", "wait a --done --exit", "send a", "-s"] {
            assert!(parse(args(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn durations() {
        assert_eq!(duration("2s").unwrap(), Duration::from_secs(2));
        assert_eq!(duration("1.5").unwrap(), Duration::from_millis(1500));
        assert_eq!(duration("250ms").unwrap(), Duration::from_millis(250));
        assert!(duration("2h").is_err() && duration("s").is_err());
    }

    #[test]
    fn targets() {
        let terms = [entry(4, "web"), entry(7, "api"), entry(9, "api")];
        assert_eq!(resolve(&terms, "web", None).unwrap(), 4);
        assert_eq!(resolve(&terms, "2", None).unwrap(), 7);
        assert_eq!(resolve(&terms, "%9", None).unwrap(), 9);
        assert_eq!(resolve(&terms, ".", Some(7)).unwrap(), 7);
        for bad in ["api", "0", "4", "%5", ".", "nope"] {
            assert!(resolve(&terms, bad, None).is_err(), "{bad}");
        }
    }

    #[test]
    fn picks_a_session() {
        assert_eq!(pick(Some("a".into()), Some("b".into()), &[]).unwrap(), "a");
        assert_eq!(pick(None, Some("b".into()), &[]).unwrap(), "b");
        assert_eq!(pick(None, None, &["0".into()]).unwrap(), "0");
        assert!(pick(None, None, &[]).is_err());
        assert!(pick(None, None, &["0".into(), "web".into()]).unwrap_err().to_string().contains("-s"));
    }

    #[test]
    fn trims_and_tails() {
        let lines = || ["$ ls", "a b", "$ ", "", ""].map(String::from).to_vec();
        assert_eq!(trim(lines(), None), ["$ ls", "a b", "$ "]);
        assert_eq!(trim(lines(), Some(2)), ["a b", "$ "]);
        assert_eq!(trim(lines(), Some(9)).len(), 3);
    }

    #[test]
    fn matches_per_row() {
        let screen = ["$ npm test", "ok 3 passed", "$ "].map(String::from);
        assert!(matches(&RegexBuilder::new(r"^\$ $").multi_line(true).build().unwrap(), &screen));
        assert!(matches(&Regex::new(r"\d+ passed").unwrap(), &screen));
        assert!(!matches(&Regex::new("failed").unwrap(), &screen));
    }
}
