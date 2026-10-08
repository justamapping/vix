use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// What `vix --resume` brings back: the list and each terminal's directory, not the programs or output.
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
    let path = path().context("no $HOME")?;
    std::fs::create_dir_all(path.parent().expect("has a parent"))?;
    std::fs::write(&path, toml::to_string(session)?).with_context(|| format!("saving {}", path.display()))
}

pub fn load() -> Result<Session> {
    let path = path().context("no $HOME")?;
    let text = std::fs::read_to_string(&path).with_context(|| format!("no session to resume at {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("reading {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let s = Session {
            cursor: 1,
            terms: vec![
                Saved { name: "web".into(), cwd: "/tmp/web".into() },
                Saved { name: "api".into(), cwd: "/tmp".into() },
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
}
