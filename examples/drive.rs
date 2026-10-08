//! Runs vix in a pty, sends each arg as keystrokes, prints the screen after each.
//! `cargo build && cargo run --example drive -- 'jj\r' 'echo hi\r' '\x1c'`  (escapes: \r \e \xNN)

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{env, thread};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

fn unescape(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.extend(c.to_string().as_bytes());
            continue;
        }
        match chars.next() {
            Some('r') => out.push(b'\r'),
            Some('e') => out.push(0x1b),
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                out.push(u8::from_str_radix(&hex, 16).expect("bad \\x escape"));
            }
            Some(other) => out.extend(other.to_string().as_bytes()),
            None => out.push(b'\\'),
        }
    }
    out
}

fn main() -> anyhow::Result<()> {
    let size = |k: &str, d: u16| env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
    let (rows, cols) = (size("DRIVE_ROWS", 12), size("DRIVE_COLS", 60));
    let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
    let mut cmd = CommandBuilder::new(concat!(env!("CARGO_MANIFEST_DIR"), "/target/debug/vix"));
    cmd.cwd(env::current_dir()?);
    cmd.env("SHELL", env::var("DRIVE_SHELL").unwrap_or("/bin/sh".into()));
    cmd.env("PS1", "$ ");
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
    let mut reader = pair.master.try_clone_reader()?;
    let p = parser.clone();
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            p.lock().unwrap().process(&buf[..n]);
        }
    });
    let mut writer = pair.master.take_writer()?;

    let show = |label: &str| {
        thread::sleep(Duration::from_millis(500));
        let p = parser.lock().unwrap();
        let (r, c) = p.screen().cursor_position();
        println!("--- {label}  (cursor {r},{c})");
        let contents = p.screen().contents();
        println!("{}", contents.trim_end());
    };
    show("start");
    for step in env::args().skip(1) {
        writer.write_all(&unescape(&step))?;
        writer.flush()?;
        show(&step);
        if let Some(status) = child.try_wait()? {
            println!("--- vix exited: {status:?}");
            return Ok(());
        }
    }
    child.kill()?;
    Ok(())
}
