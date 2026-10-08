use std::fs::File;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

static FILE: OnceLock<Option<Mutex<File>>> = OnceLock::new();

/// Appends a line to `$VIX_LOG` if set. For debugging only.
pub fn log(msg: impl std::fmt::Display) {
    let file = FILE.get_or_init(|| std::env::var("VIX_LOG").ok().and_then(|p| File::create(p).ok()).map(Mutex::new));
    if let Some(f) = file {
        let _ = writeln!(f.lock().unwrap(), "{msg}");
    }
}
