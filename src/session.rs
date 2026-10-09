use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// What `vix --resume` brings back: the list, each terminal's directory and output, not the programs.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// list row of the terminal last looked at
    #[serde(default)]
    pub cursor: usize,
    #[serde(default, rename = "term")]
    pub terms: Vec<Saved>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    pub name: String,
    pub cwd: PathBuf,
    /// a `dump`, kept beside the toml as `last/<index>.out`
    #[serde(skip)]
    pub output: Vec<u8>,
}

/// `$XDG_STATE_HOME/vix/last.toml`, else `~/.local/state/vix/last.toml`.
pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("vix/last.toml"))
}

pub fn save(session: &Session) -> Result<()> {
    save_to(&path().context("no $HOME")?, session)
}

pub fn load() -> Result<Session> {
    load_from(&path().context("no $HOME")?)
}

fn save_to(path: &Path, session: &Session) -> Result<()> {
    let dir = path.with_extension("");
    match std::fs::remove_dir_all(&dir) {
        Err(e) if e.kind() != ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    std::fs::create_dir_all(&dir)?;
    for (i, t) in session.terms.iter().enumerate().filter(|(_, t)| !t.output.is_empty()) {
        // output can hold secrets, so only the user may read it
        let mut f = OpenOptions::new().write(true).create_new(true).mode(0o600).open(dir.join(format!("{i}.out")))?;
        f.write_all(&t.output)?;
    }
    std::fs::write(path, toml::to_string(session)?).with_context(|| format!("saving {}", path.display()))
}

fn load_from(path: &Path) -> Result<Session> {
    let text = std::fs::read_to_string(path).with_context(|| format!("no session to resume at {}", path.display()))?;
    let mut session: Session = toml::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
    let dir = path.with_extension("");
    for (i, t) in session.terms.iter_mut().enumerate() {
        t.output = std::fs::read(dir.join(format!("{i}.out"))).unwrap_or_default();
    }
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let s = Session {
            cursor: 1,
            terms: vec![
                Saved { name: "web".into(), cwd: "/tmp/web".into(), output: Vec::new() },
                Saved { name: "api".into(), cwd: "/tmp".into(), output: Vec::new() },
            ],
        };
        let text = toml::to_string(&s).unwrap();
        assert!(text.contains("[[term]]\nname = \"web\""));
        assert_eq!(toml::from_str::<Session>(&text).unwrap(), s);
    }

    #[test]
    fn hand_written() {
        let s: Session = toml::from_str("[[term]]\nname = \"a\"\ncwd = \"/\"").unwrap();
        assert_eq!(s.cursor, 0);
        assert_eq!(s.terms.len(), 1);
    }

    #[test]
    fn output_beside_the_toml() {
        let dir = std::env::temp_dir().join(format!("vix-session-{}", std::process::id()));
        let path = dir.join("last.toml");
        let term = |name: &str, output: &[u8]| Saved { name: name.into(), cwd: "/".into(), output: output.to_vec() };
        save_to(&path, &Session { cursor: 0, terms: vec![term("a", b"x"), term("b", b""), term("c", b"z")] }).unwrap();
        assert!(dir.join("last/2.out").exists() && !dir.join("last/1.out").exists());
        save_to(&path, &Session { cursor: 0, terms: vec![term("c", b"z")] }).unwrap();
        let s = load_from(&path).unwrap();
        assert_eq!(s.terms, vec![term("c", b"z")]);
        assert!(!dir.join("last/2.out").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
