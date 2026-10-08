use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::input::CTRL_BACKSLASH;
use crate::keyspec;
use crate::remap::Map;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// legacy byte of the key that leaves view i
    pub escape: u8,
    /// how long a partly typed mapping waits, like vim's 'timeoutlen'
    pub timeout: Duration,
    pub list: Vec<Map>,
    pub view: Vec<Map>,
}

impl Default for Config {
    fn default() -> Self {
        Self { escape: CTRL_BACKSLASH, timeout: Duration::from_millis(1000), list: Vec::new(), view: Vec::new() }
    }
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct File {
    escape: Option<String>,
    timeoutlen: Option<u64>,
    map: Maps,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct Maps {
    list: BTreeMap<String, String>,
    view: BTreeMap<String, String>,
}

fn maps(table: &BTreeMap<String, String>) -> Result<Vec<Map>> {
    table
        .iter()
        .map(|(l, r)| Ok(Map { lhs: keyspec::parse(l).with_context(|| format!("map {l:?}"))?, rhs: keyspec::parse(r)? }))
        .collect()
}

pub fn parse(text: &str) -> Result<Config> {
    let file: File = toml::from_str(text)?;
    let mut config = Config::default();
    if let Some(escape) = &file.escape {
        config.escape = keyspec::escape(escape)?;
    }
    if let Some(ms) = file.timeoutlen {
        config.timeout = Duration::from_millis(ms);
    }
    config.list = maps(&file.map.list)?;
    config.view = maps(&file.map.view)?;
    Ok(config)
}

/// `$XDG_CONFIG_HOME/vix`, else `~/.config/vix`.
pub fn dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("vix"))
}

/// Loads `config.toml`; a missing file is the default, a broken one is the default plus an error to show.
pub fn load() -> (Config, Option<String>) {
    let Some(path) = dir().map(|d| d.join("config.toml")) else { return (Config::default(), None) };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Config::default(), None),
        Err(e) => return (Config::default(), Some(format!("{}: {e}", path.display()))),
    };
    match parse(&text) {
        Ok(config) => (config, None),
        Err(e) => (Config::default(), Some(format!("{}: {e:#}", path.display()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Key;

    #[test]
    fn empty_is_default() {
        assert_eq!(parse("").unwrap(), Config::default());
    }

    #[test]
    fn full() {
        let c = parse(
            r#"
            escape = "<C-Space>"
            timeoutlen = 300
            [map.list]
            "<C-j>" = "J"
            [map.view]
            q = ":q<CR>"
            "#,
        )
        .unwrap();
        assert_eq!(c.escape, 0);
        assert_eq!(c.timeout, Duration::from_millis(300));
        assert_eq!(c.list, vec![Map { lhs: vec![Key::Ctrl('j')], rhs: vec![Key::Char('J')] }]);
        assert_eq!(c.view[0].rhs, vec![Key::Char(':'), Key::Char('q'), Key::Enter]);
    }

    #[test]
    fn errors() {
        assert!(parse("escape = \"x\"").is_err());
        assert!(parse("bogus = 1").is_err());
        assert!(parse("[map.insert]\na = \"b\"").is_err());
        assert!(parse("[map.list]\n\"\" = \"b\"").is_err());
    }
}
