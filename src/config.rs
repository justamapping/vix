use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::input::{CTRL_BACKSLASH, Key};
use crate::keyspec;
use crate::remap::Map;
use crate::status::Template;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// legacy byte of the key that leaves view i
    pub escape: u8,
    /// how long a partly typed mapping waits, like vim's 'timeoutlen'
    pub timeout: Duration,
    pub list: Vec<Map>,
    pub list_insert: Vec<Map>,
    pub view: Vec<Map>,
    pub status: Status,
    /// vix takes the mouse for scrolling and selecting, unless the program in view asked for it
    pub mouse: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub enabled: bool,
    pub left: Template,
    pub right: Template,
}

impl Default for Status {
    fn default() -> Self {
        let t = |s| Template::parse(s).expect("default template parses");
        Self { enabled: true, left: t("{mode}"), right: t("{modified} {terms}") }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            escape: CTRL_BACKSLASH,
            timeout: Duration::from_millis(1000),
            list: Vec::new(),
            list_insert: Vec::new(),
            view: default_view(),
            status: Status::default(),
            mouse: true,
        }
    }
}

/// Built-in view n mappings; a config mapping with the same lhs replaces one.
fn default_view() -> Vec<Map> {
    vec![Map { lhs: vec![Key::Char('q')], rhs: vec![Key::Char('-')] }]
}

fn with_defaults(defaults: Vec<Map>, user: Vec<Map>) -> Vec<Map> {
    let mut maps: Vec<Map> = defaults.into_iter().filter(|d| !user.iter().any(|u| u.lhs == d.lhs)).collect();
    maps.extend(user);
    maps
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct File {
    escape: Option<String>,
    timeoutlen: Option<u64>,
    mouse: Option<bool>,
    map: Maps,
    status: StatusFile,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct StatusFile {
    enabled: Option<bool>,
    left: Option<String>,
    right: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct Maps {
    list: BTreeMap<String, String>,
    list_insert: BTreeMap<String, String>,
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
    if let Some(mouse) = file.mouse {
        config.mouse = mouse;
    }
    config.list = maps(&file.map.list)?;
    config.list_insert = maps(&file.map.list_insert)?;
    config.view = with_defaults(default_view(), maps(&file.map.view)?);
    let status = &mut config.status;
    if let Some(enabled) = file.status.enabled {
        status.enabled = enabled;
    }
    if let Some(left) = &file.status.left {
        status.left = Template::parse(left).context("status.left")?;
    }
    if let Some(right) = &file.status.right {
        status.right = Template::parse(right).context("status.right")?;
    }
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
        assert_eq!(c.view, vec![Map { lhs: vec![Key::Char('q')], rhs: vec![Key::Char(':'), Key::Char('q'), Key::Enter] }]);
    }

    #[test]
    fn default_q_and_status() {
        let c = parse("[map.view]\n\"<C-j>\" = \"J\"\n[status]\nenabled = false\nleft = \"{name}\"").unwrap();
        assert_eq!(c.view[0], default_view()[0]);
        assert_eq!(c.view.len(), 2);
        assert!(!c.status.enabled);
        assert_eq!(c.status.left, Template::parse("{name}").unwrap());
        assert_eq!(c.status.right, Status::default().right);
        assert!(c.list_insert.is_empty());
        let c = parse("[map.list_insert]\njk = \"<Esc>\"").unwrap();
        assert_eq!(c.list_insert, vec![Map { lhs: vec![Key::Char('j'), Key::Char('k')], rhs: vec![Key::Esc] }]);
    }

    #[test]
    fn errors() {
        assert!(parse("escape = \"x\"").is_err());
        assert!(parse("bogus = 1").is_err());
        assert!(parse("[map.insert]\na = \"b\"").is_err());
        assert!(parse("[map.list]\n\"\" = \"b\"").is_err());
        assert!(parse("[status]\nleft = \"{nope}\"").is_err());
    }
}
