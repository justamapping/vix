//! Where sessions' sockets live, what they're named, and finding or starting a session's server.

use std::fs::{self, DirBuilder, File};
use std::io::{self, BufReader, ErrorKind};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::protocol::{self, Reply, Request};
use crate::session;

// sun_path is 104 bytes on macOS and 108 on Linux, with the NUL
const MAX_PATH: usize = 103;
const START_TIMEOUT: Duration = Duration::from_secs(3);

/// `$XDG_RUNTIME_DIR/vix`, else `$TMPDIR/vix-<uid>` like tmux, created private.
pub fn dir() -> Result<PathBuf> {
    let dir = match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        Some(run) => PathBuf::from(run).join("vix"),
        None => std::env::temp_dir().join(format!("vix-{}", uid())),
    };
    private(&dir)?;
    Ok(dir)
}

fn uid() -> u32 {
    unsafe { libc::getuid() }
}

/// Creates `dir` as 0700, or checks an existing one is yours and private: anyone who can connect can type into its shells.
pub fn private(dir: &Path) -> Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let meta = fs::metadata(dir)?;
    if meta.uid() != uid() || meta.mode() & 0o077 != 0 {
        bail!("{} must be yours and private (chmod 700)", dir.display());
    }
    Ok(())
}

/// A session's socket in `dir`. Names that can't be a file, or make the path too long for a socket, error here.
pub fn path(dir: &Path, name: &str) -> Result<PathBuf> {
    if name.is_empty() || name.starts_with('.') || name.contains('/') {
        bail!("bad session name \"{name}\"");
    }
    let path = dir.join(format!("{name}.sock"));
    if path.as_os_str().len() > MAX_PATH {
        bail!("session name \"{name}\" is too long for a socket path");
    }
    Ok(path)
}

/// `<name>.log` in the state dir, where a server's errors go, since it has no screen.
pub fn log(name: &str) -> Result<PathBuf> {
    Ok(session::state_dir().context("no $HOME")?.join(format!("{name}.log")))
}

/// Connects to a live session. A socket nobody answers on was left by a crash, and is removed.
pub fn connect(path: &Path) -> Result<Option<UnixStream>> {
    match UnixStream::connect(path) {
        Ok(stream) => Ok(Some(stream)),
        Err(e) if e.kind() == ErrorKind::ConnectionRefused => {
            let _ = fs::remove_file(path);
            Ok(None)
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("connecting to {}", path.display())),
    }
}

/// The server's end of `path`. Fails if the session is already running.
pub fn bind(path: &Path) -> Result<UnixListener> {
    if connect(path)?.is_some() {
        bail!("{} is already running", path.display());
    }
    UnixListener::bind(path).with_context(|| format!("listening on {}", path.display()))
}

/// Live sessions in `dir`, numbers first, each connected. Stale sockets are removed on the way.
pub fn sessions(dir: &Path) -> Result<Vec<(String, UnixStream)>> {
    let mut live = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".sock")) else {
            continue;
        };
        if let Some(stream) = connect(&path)? {
            live.push((name.to_string(), stream));
        }
    }
    live.sort_by(|(a, _), (b, _)| order(a).cmp(&order(b)));
    Ok(live)
}

/// Numbers in order, then names.
fn order(name: &str) -> (bool, u64, &str) {
    match name.parse() {
        Ok(n) => (false, n, ""),
        Err(_) => (true, 0, name),
    }
}

/// The lowest number no live session uses.
pub fn next_name<'a>(live: impl IntoIterator<Item = &'a str>) -> String {
    let live: Vec<&str> = live.into_iter().collect();
    (0u64..).map(|n| n.to_string()).find(|n| !live.contains(&n.as_str())).expect("some number is free")
}

/// Asks a session about itself, for `vix -l`: (terminals, clients attached).
pub fn info(stream: UnixStream) -> Result<(usize, usize)> {
    protocol::send(&mut &stream, &Request::Info)?;
    match protocol::recv(&mut BufReader::new(&stream))? {
        Some(Reply::Info { terms, clients, .. }) => Ok((terms, clients)),
        Some(Reply::Bye { reason }) => bail!(reason),
        _ => bail!("no answer"),
    }
}

/// Starts `vix --server <name>` and connects once it listens.
pub fn start(path: &Path, name: &str) -> Result<UnixStream> {
    let log = log(name)?;
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.args(["--server", name]);
    let child = detach(cmd, &log)?;
    wait(path, child, &log)
}

/// Spawns `cmd` in its own session, so closing the terminal doesn't hang it up, with its output in `log`.
pub fn detach(mut cmd: Command, log: &Path) -> Result<Child> {
    if let Some(dir) = log.parent() {
        fs::create_dir_all(dir)?;
    }
    let out = File::create(log).with_context(|| format!("creating {}", log.display()))?;
    cmd.stdin(Stdio::null()).stdout(out.try_clone()?).stderr(out);
    unsafe {
        cmd.pre_exec(|| if libc::setsid() == -1 { Err(io::Error::last_os_error()) } else { Ok(()) });
    }
    Ok(cmd.spawn()?)
}

/// Connects once `child` listens on `path`, or says why it never did.
pub fn wait(path: &Path, mut child: Child, log: &Path) -> Result<UnixStream> {
    let start = Instant::now();
    loop {
        // not `connect`: refused here means not listening yet, not stale
        match UnixStream::connect(path) {
            Ok(stream) => return Ok(stream),
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {}
            Err(e) => return Err(e).with_context(|| format!("connecting to {}", path.display())),
        }
        if let Some(status) = child.try_wait()? {
            bail!("server exited ({status}), see {}", log.display());
        }
        if start.elapsed() > START_TIMEOUT {
            let _ = child.kill();
            bail!("server didn't start, see {}", log.display());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn tmp() -> tempfile::TempDir {
        // short, since socket paths are capped
        tempfile::Builder::new().prefix("vx").tempdir_in("/tmp").unwrap()
    }

    /// A socket file nobody listens on, like one a crashed server leaves.
    fn stale(path: &Path) {
        drop(UnixListener::bind(path).unwrap());
        // a process another test forks meanwhile holds the listener until it execs
        let start = Instant::now();
        while UnixStream::connect(path).is_ok() && start.elapsed() < START_TIMEOUT {
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn names_must_make_a_socket_path() {
        let dir = Path::new("/tmp/vix-1");
        assert_eq!(path(dir, "web").unwrap(), Path::new("/tmp/vix-1/web.sock"));
        for bad in ["", ".hidden", "a/b", &"x".repeat(100)] {
            assert!(path(dir, bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn private_dirs_only() {
        let tmp = tmp();
        let dir = tmp.path().join("a/vix");
        private(&dir).unwrap();
        assert_eq!(fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private(&dir).is_err());
    }

    #[test]
    fn stale_sockets_are_removed_live_ones_kept() {
        let tmp = tmp();
        let path = path(tmp.path(), "web").unwrap();
        assert!(connect(&path).unwrap().is_none());
        stale(&path);
        assert!(path.exists());
        assert!(connect(&path).unwrap().is_none());
        assert!(!path.exists());
        let _live = bind(&path).unwrap();
        assert!(connect(&path).unwrap().is_some());
        assert!(bind(&path).is_err());
    }

    #[test]
    fn sessions_lists_live_numbers_first() {
        let tmp = tmp();
        let _live: Vec<UnixListener> = ["web", "10", "2", "0"].iter().map(|n| bind(&path(tmp.path(), n).unwrap()).unwrap()).collect();
        stale(&path(tmp.path(), "dead").unwrap());
        fs::write(tmp.path().join("notes.txt"), "").unwrap();
        let names: Vec<String> = sessions(tmp.path()).unwrap().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["0", "2", "10", "web"]);
        assert!(!tmp.path().join("dead.sock").exists());
        assert_eq!(next_name(names.iter().map(String::as_str)), "1");
        assert_eq!(next_name([]), "0");
    }

    #[test]
    fn info_asks_the_server() {
        let tmp = tmp();
        let path = path(tmp.path(), "0").unwrap();
        let listener = bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let request: Option<Request> = protocol::recv(&mut BufReader::new(&stream)).unwrap();
            assert_eq!(request, Some(Request::Info));
            protocol::send(&mut &stream, &Reply::Info { name: "0".into(), terms: 3, clients: 1 }).unwrap();
        });
        assert_eq!(info(connect(&path).unwrap().unwrap()).unwrap(), (3, 1));
        server.join().unwrap();
    }

    #[test]
    fn detach_starts_its_own_session_logging_output() {
        let tmp = tmp();
        let log = tmp.path().join("state/web.log");
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo to-log; exec sleep 5"]);
        let mut child = detach(cmd, &log).unwrap();
        let pid = child.id() as i32;
        assert_eq!(unsafe { libc::getsid(pid) }, pid);
        let start = Instant::now();
        while fs::read_to_string(&log).unwrap() != "to-log\n" && start.elapsed() < START_TIMEOUT {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(fs::read_to_string(&log).unwrap(), "to-log\n");
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn wait_connects_once_listening_or_says_why_not() {
        let tmp = tmp();
        let log = tmp.path().join("web.log");
        let path = path(tmp.path(), "web").unwrap();
        let p = path.clone();
        let late = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            let listener = UnixListener::bind(p).unwrap();
            listener.accept().unwrap();
        });
        let sleeper = Command::new("sleep").arg("5").spawn().unwrap();
        let pid = sleeper.id();
        assert!(wait(&path, sleeper, &log).is_ok());
        late.join().unwrap();
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };

        let gone = path.with_file_name("gone.sock");
        let err = wait(&gone, Command::new("false").spawn().unwrap(), &log).unwrap_err().to_string();
        assert!(err.starts_with("server exited") && err.ends_with("web.log"), "{err}");
    }
}
