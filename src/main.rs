mod app;
mod buffer;
mod carry;
mod client;
mod config;
mod ctl;
mod cwds;
mod dump;
mod follow;
mod input;
mod keymap;
mod keyspec;
mod listdiff;
mod log;
mod motion;
mod mux;
mod outbox;
mod picker;
mod proc;
mod prompt;
mod protocol;
mod pty;
mod remap;
mod render;
mod server;
mod session;
mod sock;
mod state;
mod status;
mod textobj;
mod vt;
mod workflow;

use std::io::{self, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use ratatui::DefaultTerminal;
use signal_hook::consts::{SIGHUP, SIGTERM, SIGWINCH};
use signal_hook::iterator::Signals;

use app::{App, Event};
use protocol::{Reply, Request, Start};
use session::Session;

// events handled before a redraw, so a flooding program can't starve the screen
const BATCH: usize = 256;

const USAGE: &str = "usage: vix [-r | --resume] [workflow]
       vix -d | --detached [-x cols] [-y rows] [-r | workflow]   starts it without attaching, prints its name
       vix -a | --attach [session]
       vix -l | --list
       vix ctl <verb>, see vix ctl --help";

fn main() -> Result<()> {
    let (mut resume, mut attach, mut list, mut detached) = (false, false, false, false);
    let mut name: Option<String> = None;
    let (mut cols, mut rows): (Option<u16>, Option<u16>) = (None, None);
    let mut args = std::env::args().skip(1).peekable();
    if args.next_if(|a| a == "ctl").is_some() {
        return ctl::run(args.collect());
    }
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-r" | "--resume" => resume = true,
            "-a" | "--attach" => attach = true,
            "-l" | "--list" => list = true,
            "-d" | "--detached" => detached = true,
            "-x" => cols = Some(args.next().and_then(|n| n.parse().ok()).context("-x takes a number of columns")?),
            "-y" => rows = Some(args.next().and_then(|n| n.parse().ok()).context("-y takes a number of rows")?),
            // started by a client, not by hand
            "--server" => return server::run(&args.next().context("--server needs a session name")?),
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            _ if arg.starts_with('-') || name.is_some() => bail!("unknown argument {arg}\n{USAGE}"),
            _ => name = Some(arg),
        }
    }
    if list {
        if resume || attach || detached || name.is_some() {
            bail!("--list takes nothing else\n{USAGE}");
        }
        return list_sessions();
    }
    if resume && (attach || name.is_some()) {
        bail!("--resume starts a new session, so it takes no name\n{USAGE}");
    }
    if detached {
        if attach {
            bail!("--detached starts a session, --attach joins one\n{USAGE}");
        }
        return start_detached(name, resume, (cols.unwrap_or(80), rows.unwrap_or(24)));
    }
    if cols.is_some() || rows.is_some() {
        bail!("-x and -y size a --detached session; attached ones take the terminal's size\n{USAGE}");
    }
    let (session, stream, start, new_workflow) = connect(name, resume, attach)?;
    let (config, config_error) = config::load();

    let (tx, rx) = mpsc::channel();
    app::pump(io::stdin(), tx.clone(), Event::Input, Event::Hangup);
    let mut signals = Signals::new([SIGWINCH, SIGHUP, SIGTERM])?;
    let sig = tx.clone();
    thread::spawn(move || {
        for signal in signals.forever() {
            let _ = sig.send(if signal == SIGWINCH { Event::Resize } else { Event::Hangup });
        }
    });

    let mut terminal = ratatui::try_init()?;
    let size = terminal.size()?;
    let result = App::new(tx, stream, session, size.width, size.height, config, start).and_then(|mut app| {
        if let Some(msg) = config_error.or(new_workflow) {
            app.message(msg);
        }
        run(&mut app, &mut terminal, rx)?;
        Ok(app.farewell)
    });
    let _ = io::stdout().write_all(&vt::mode_bytes(vt::Modes::default()));
    ratatui::restore();
    if let Some(line) = result? {
        println!("{line}");
    }
    Ok(())
}

/// `vix -l`: each running session, numbers first.
fn list_sessions() -> Result<()> {
    let sessions = sock::sessions(&sock::dir()?)?;
    if sessions.is_empty() {
        bail!("no sessions");
    }
    for (name, stream) in sessions {
        match sock::info(stream) {
            Ok(live) => println!("{live}"),
            Err(e) => println!("{name}: {e:#}"),
        }
    }
    Ok(())
}

/// Attaches to the session a workflow names if it's running, else starts one: from the workflow, `--resume`'s
/// snapshot, or empty, under the lowest free number. With `attach`, only attaches, by default to the session
/// detached last. Returns its name, the socket, what to start it from, and a message if the workflow is new.
fn connect(workflow: Option<String>, resume: bool, attach: bool) -> Result<(String, UnixStream, Option<Start>, Option<String>)> {
    let dir = sock::dir()?;
    let inside = sock::inside(&dir, std::env::var_os("VIX_SOCKET").as_deref().map(Path::new));
    let name = match &workflow {
        Some(w) => w.clone(),
        None if attach => {
            let live: Vec<sock::Live> = sock::sessions(&dir)?.into_iter().filter_map(|(_, s)| sock::info(s).ok()).collect();
            sock::latest(&live, inside.as_deref()).context("no session to attach to")?.to_string()
        }
        None => sock::next_name(sock::sessions(&dir)?.iter().map(|(n, _)| n.as_str())),
    };
    // it would draw itself inside itself forever
    if inside.as_deref() == Some(name.as_str()) {
        bail!("already inside session {name}");
    }
    let path = sock::path(&dir, &name)?;
    if let Some(stream) = sock::connect(&path)? {
        return Ok((name, stream, None, None));
    }
    if attach {
        bail!("no session {name}");
    }
    let mut new = None;
    let session = match &workflow {
        Some(w) => workflow::load(w)?.unwrap_or_else(|| {
            new = Some(format!("\"{w}\" [New]"));
            Session::default()
        }),
        None if resume => session::load()?,
        None => Session::default(),
    };
    let stream = sock::start(&path, &name)?;
    Ok((name, stream, Some(Start::new(session, workflow)), new))
}

/// `vix -d`: starts the session `connect` would attach to, sized `cols` x `rows`, and leaves without attaching.
/// One that's running already is left as it is.
fn start_detached(workflow: Option<String>, resume: bool, (cols, rows): (u16, u16)) -> Result<()> {
    let (name, mut stream, start, _) = connect(workflow, resume, false)?;
    if start.is_some() {
        protocol::send(&mut stream, &Request::Hello { version: protocol::VERSION, cols, rows, start })?;
        protocol::send(&mut stream, &Request::Detach)?;
        let mut reader = BufReader::new(stream);
        while let Some(reply) = protocol::recv(&mut reader)? {
            match reply {
                Reply::Bye { reason } if reason == "detached" => break,
                Reply::Bye { reason } => bail!(reason),
                _ => {}
            }
        }
    }
    println!("{name}");
    Ok(())
}

fn run(app: &mut App, terminal: &mut DefaultTerminal, rx: Receiver<Event>) -> Result<()> {
    app.draw(terminal)?;
    loop {
        let event = match app.deadline {
            Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Err(RecvTimeoutError::Timeout) => {
                    app.flush()?;
                    app.draw(terminal)?;
                    continue;
                }
                event => event.ok(),
            },
            None => rx.recv().ok(),
        };
        let Some(event) = event else { break };
        let mut redraw = app.handle(event)?;
        for event in rx.try_iter().take(BATCH) {
            redraw |= app.handle(event)?;
        }
        if app.quit {
            break;
        }
        if redraw {
            app.draw(terminal)?;
        }
    }
    Ok(())
}
