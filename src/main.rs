mod app;
mod buffer;
mod carry;
mod client;
mod config;
mod dump;
mod follow;
mod input;
mod keymap;
mod keyspec;
mod listdiff;
mod log;
mod motion;
mod mux;
mod picker;
mod proc;
mod protocol;
mod pty;
mod remap;
mod render;
mod server;
mod session;
#[allow(dead_code)] // `-l` uses the rest, step 3f
mod sock;
mod state;
mod status;
mod vt;
mod workflow;

use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use ratatui::DefaultTerminal;
use signal_hook::consts::{SIGHUP, SIGTERM, SIGWINCH};
use signal_hook::iterator::Signals;

use app::{App, Event};
use protocol::Start;
use session::Session;

// events handled before a redraw, so a flooding program can't starve the screen
const BATCH: usize = 256;

const USAGE: &str = "usage: vix [-r | --resume] [workflow]";

fn main() -> Result<()> {
    let mut resume = false;
    let mut name: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-r" | "--resume" => resume = true,
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
    if resume && name.is_some() {
        bail!("--resume and a workflow can't be used together\n{USAGE}");
    }
    let (session, stream, start, new_workflow) = connect(name, resume)?;
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

/// Attaches to the session a workflow names if it's running, else starts one: from the workflow, `--resume`'s
/// snapshot, or empty, under the lowest free number. Returns its name, the socket, what to start it from, and a
/// message if the workflow is new.
fn connect(workflow: Option<String>, resume: bool) -> Result<(String, UnixStream, Option<Start>, Option<String>)> {
    let dir = sock::dir()?;
    let name = match &workflow {
        Some(w) => w.clone(),
        None => sock::next_name(sock::sessions(&dir)?.iter().map(|(n, _)| n.as_str())),
    };
    let path = sock::path(&dir, &name)?;
    if let Some(stream) = sock::connect(&path)? {
        return Ok((name, stream, None, None));
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
