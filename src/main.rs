mod app;
mod buffer;
mod config;
mod input;
mod keymap;
mod listdiff;
mod log;
mod motion;
mod pty;
mod render;
mod state;
mod vt;

use std::io::{self, Write};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use anyhow::Result;
use ratatui::DefaultTerminal;
use signal_hook::{consts::SIGWINCH, iterator::Signals};

use app::{App, Event};

// events handled before a redraw, so a flooding program can't starve the screen
const BATCH: usize = 256;

fn main() -> Result<()> {
    let (tx, rx) = mpsc::channel();
    app::pump(io::stdin(), tx.clone(), Event::Input, Event::Hangup);
    let mut signals = Signals::new([SIGWINCH])?;
    let winch = tx.clone();
    thread::spawn(move || {
        for _ in signals.forever() {
            let _ = winch.send(Event::Resize);
        }
    });

    let mut terminal = ratatui::try_init()?;
    let size = terminal.size()?;
    let result = App::new(tx, size.width, size.height).and_then(|app| run(app, &mut terminal, rx));
    let _ = io::stdout().write_all(&vt::mode_bytes(vt::Modes::default()));
    ratatui::restore();
    result
}

fn run(mut app: App, terminal: &mut DefaultTerminal, rx: Receiver<Event>) -> Result<()> {
    app.draw(terminal)?;
    while let Ok(event) = rx.recv() {
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
    app.kill_all();
    Ok(())
}
