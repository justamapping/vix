use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::config;
use crate::session::Session;

/// `$XDG_CONFIG_HOME/vix/workflows/<name>.toml`, else under `~/.config`.
pub fn path(name: &str) -> Result<PathBuf> {
    if name.is_empty() || name.starts_with('.') || name.contains('/') {
        bail!("E32: bad workflow name {name:?}");
    }
    Ok(config::dir().context("no $HOME")?.join("workflows").join(format!("{name}.toml")))
}

/// A workflow that doesn't exist yet is `None`, like vim opening a new file.
pub fn load(name: &str) -> Result<Option<Session>> {
    load_from(&path(name)?)
}

/// Writes the list as a workflow. Output isn't kept; directories under `$HOME` are written as `~`.
pub fn save(name: &str, session: Session) -> Result<()> {
    save_to(&path(name)?, session)
}

fn load_from(path: &Path) -> Result<Option<Session>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut session: Session = toml::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
    for t in &mut session.terms {
        t.cwd = untilde(&t.cwd);
    }
    Ok(Some(session))
}

fn save_to(path: &Path, mut session: Session) -> Result<()> {
    for t in &mut session.terms {
        t.cwd = tilde(&t.cwd).into();
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, toml::to_string(&session)?).with_context(|| format!("saving {}", path.display()))
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// `path` with the home directory as `~`.
pub fn tilde(path: &Path) -> String {
    match home().as_deref().and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// `~` or `~/rest` as a path under the home directory.
fn untilde(path: &Path) -> PathBuf {
    match (path.strip_prefix("~"), home()) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Saved;

    #[test]
    fn names() {
        assert!(path("web").unwrap().ends_with("vix/workflows/web.toml"));
        assert!(path("").is_err() && path("a/b").is_err() && path("..").is_err());
    }

    #[test]
    fn round_trip_with_home_as_tilde() {
        let home = home().unwrap();
        let dir = std::env::temp_dir().join(format!("vix-workflow-{}", std::process::id()));
        let path = dir.join("web.toml");
        assert_eq!(load_from(&path).unwrap(), None);
        let web = Saved { cmd: Some("npm run dev".into()), ..Saved::new("web", home.join("proj")) };
        let s = Session { cursor: 1, terms: vec![web, Saved::new("tmp", "/tmp")] };
        let expected = Session { cursor: 1, terms: s.terms.iter().map(|t| Saved { output: Vec::new(), ..t.clone() }).collect() };
        save_to(&path, s).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("cwd = \"~/proj\"\ncmd = \"npm run dev\""), "{text}");
        assert!(!text.contains("cmd = \"\""));
        assert_eq!(load_from(&path).unwrap(), Some(expected));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
