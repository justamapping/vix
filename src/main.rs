mod app;
mod buffer;
mod config;
mod input;
mod keymap;
mod keyspec;
mod listdiff;
mod log;
mod motion;
mod proc;
mod pty;
mod remap;
mod render;
mod session;
mod state;
mod vt;

use std::io::{self, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Instant;

use anyhow::{Result, bail};
use ratatui::DefaultTerminal;
use signal_hook::consts::{SIGHUP, SIGTERM, SIGWINCH};
use signal_hook::iterator::Signals;

use app::{App, Event};
use session::Session;

// events handled before a redraw, so a flooding program can't starve the screen
const BATCH: usize = 256;

const USAGE: &str = "usage: vix [-r | --resume]";

fn main() -> Result<()> {
    let mut resume = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-r" | "--resume" => resume = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            _ => bail!("unknown argument {arg}\n{USAGE}"),
        }
    }
    let session = if resume { session::load()? } else { Session::default() };
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
    let result = App::new(tx, size.width, size.height, config, session).and_then(|mut app| {
        if let Some(e) = config_error {
            app.message(e);
        }
        run(app, &mut terminal, rx)
    });
    let _ = io::stdout().write_all(&vt::mode_bytes(vt::Modes::default()));
    ratatui::restore();
    result
}

fn run(mut app: App, terminal: &mut DefaultTerminal, rx: Receiver<Event>) -> Result<()> {
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
    let snapshot = app.snapshot();
    app.kill_all();
    if snapshot.terms.is_empty() {
        return Ok(());
    }
    session::save(&snapshot)
}
