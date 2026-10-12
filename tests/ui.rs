//! What a user sees: vix in a pty, keys in, screens out, compared with `tests/snapshots`.
//! Accept new or changed screens with `INSTA_UPDATE=always cargo test --test ui`, then review the diff.

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use insta::assert_snapshot;
use portable_pty::{Child, CommandBuilder, PtySize, native_pty_system};

// output this long after the last keys or output means vix is done reacting
const QUIET: Duration = Duration::from_millis(300);
const TIMEOUT: Duration = Duration::from_secs(5);

/// A throwaway `$HOME` with config, state, and a working directory, shared by every vix started in one test.
struct Home(tempfile::TempDir);

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["config/vix/workflows", "state", "run", "work"] {
            std::fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        // a non-login sh with a fixed prompt, whatever the machine's profile says
        let shell = dir.path().join("shell");
        std::fs::write(&shell, "#!/bin/sh\nunset ENV\nexport PS1='$ '\nexec /bin/sh -i\n").unwrap();
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self(dir)
    }

    /// Under the real path: macOS temp dirs sit behind a symlink, and shells report where they really are.
    fn path(&self, rel: &str) -> PathBuf {
        self.0.path().canonicalize().unwrap().join(rel)
    }
}

/// Killing a client leaves its session running, so every session started under this home is killed with it.
impl Drop for Home {
    fn drop(&mut self) {
        let Ok(socks) = std::fs::read_dir(self.path("run/vix")) else { return };
        for sock in socks.flatten() {
            let Ok(mut stream) = UnixStream::connect(sock.path()) else { continue };
            let _ = stream.set_read_timeout(Some(TIMEOUT));
            let _ = stream.write_all(b"{\"type\":\"kill_session\",\"cursor\":0}\n");
            // the server hangs up once it's done
            let _ = stream.read_to_end(&mut Vec::new());
        }
    }
}

struct Vix {
    child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    /// the screen, and when output last arrived
    term: Arc<Mutex<(vt100::Parser, Instant)>>,
}

impl Vix {
    fn start(home: &Home, args: &[&str]) -> Self {
        Self::sized(home, args, 60, 12)
    }

    fn sized(home: &Home, args: &[&str], cols: u16, rows: u16) -> Self {
        let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }).unwrap();
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_vix"));
        cmd.args(args);
        cmd.cwd(home.path("work"));
        cmd.env_clear();
        for (key, rel) in [("HOME", ""), ("XDG_CONFIG_HOME", "config"), ("XDG_STATE_HOME", "state"), ("XDG_RUNTIME_DIR", "run"), ("SHELL", "shell")] {
            cmd.env(key, home.path(rel));
        }
        cmd.env("PATH", std::env::var("PATH").unwrap_or_default());
        cmd.env("TERM", "xterm-256color");
        cmd.env("BASH_SILENCE_DEPRECATION_WARNING", "1");
        let child = pair.slave.spawn_command(cmd).unwrap();
        drop(pair.slave);
        let term = Arc::new(Mutex::new((vt100::Parser::new(rows, cols, 0), Instant::now())));
        let mut reader = pair.master.try_clone_reader().unwrap();
        let t = term.clone();
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n @ 1..) = reader.read(&mut buf) {
                let mut t = t.lock().unwrap();
                t.0.process(&buf[..n]);
                t.1 = Instant::now();
            }
        });
        let writer = pair.master.take_writer().unwrap();
        // the master must outlive the child, or it hangs up
        std::mem::forget(pair.master);
        let mut vix = Self { child, writer, term };
        let start = Instant::now();
        while vix.term.lock().unwrap().0.screen().contents().is_empty() && start.elapsed() < TIMEOUT {
            thread::sleep(Duration::from_millis(20));
        }
        vix.settle();
        vix
    }

    /// Waits until vix has been quiet for a moment.
    fn settle(&mut self) {
        let start = Instant::now();
        thread::sleep(QUIET);
        while self.term.lock().unwrap().1.elapsed() < QUIET && start.elapsed() < TIMEOUT {
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Types `keys` and waits for vix to finish reacting.
    fn keys(&mut self, keys: &str) -> &mut Self {
        self.writer.write_all(keys.as_bytes()).unwrap();
        self.writer.flush().unwrap();
        self.settle();
        self
    }

    /// The screen as text, trailing blanks trimmed, with the cursor position.
    fn screen(&self) -> String {
        let t = self.term.lock().unwrap();
        let (row, col) = t.0.screen().cursor_position();
        let rows: Vec<String> = t.0.screen().rows(0, t.0.screen().size().1).map(|r| r.trim_end().to_string()).collect();
        format!("{}\n[cursor {row},{col}]", rows.join("\n"))
    }

    fn exited(&mut self) -> bool {
        let start = Instant::now();
        while start.elapsed() < TIMEOUT {
            if self.child.try_wait().unwrap().is_some() {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

impl Drop for Vix {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn list_write_renames_clones_and_spawns() {
    let home = Home::new();
    let mut vix = Vix::start(&home, &[]);
    assert_snapshot!("write_start", vix.screen());
    vix.keys("cwweb\x1b").keys("yyp").keys("oapi\x1b");
    assert_snapshot!("write_staged", vix.screen());
    vix.keys(":w\r");
    assert_snapshot!("write_written", vix.screen());
    vix.keys("\ri").keys("echo hi\r");
    assert_snapshot!("write_view_i", vix.screen());
    vix.keys("\x1c");
    assert_snapshot!("write_view_n", vix.screen());
    vix.keys("-");
    assert_snapshot!("write_back_to_list", vix.screen());
}

#[test]
fn open_modified_list_asks_first() {
    let home = Home::new();
    let mut vix = Vix::start(&home, &[]);
    vix.keys("oapi\x1b").keys("\r");
    assert_snapshot!("open_prompt", vix.screen());
    vix.keys("y");
    assert_snapshot!("open_opened", vix.screen());
}

#[test]
fn view_commands_edit_rename_delete() {
    let home = Home::new();
    let mut vix = Vix::start(&home, &[]);
    vix.keys(":e logs\r").keys("echo in logs\r");
    assert_snapshot!("view_cmds_edit_spawns", vix.screen());
    vix.keys("\x1c").keys(":r tail\r");
    assert_snapshot!("view_cmds_renamed", vix.screen());
    vix.keys("K");
    assert_snapshot!("view_cmds_switched", vix.screen());
    vix.keys(":d tail\r");
    assert_snapshot!("view_cmds_deleted_other", vix.screen());
    vix.keys(":e second\r").keys("\x1c").keys(":d\r");
    assert_snapshot!("view_cmds_deleted_viewed", vix.screen());
}

#[test]
fn exits_keep_unsaved_edits() {
    let home = Home::new();
    let mut vix = Vix::start(&home, &[]);
    vix.keys("oapi\x1b").keys(":w\r").keys("ggcwfoo\x1b").keys(":e logs\r").keys("\x1c-");
    assert_snapshot!("exits_spawned_under_edits", vix.screen());
    vix.keys(":e logs\r").keys("exit\r");
    assert_snapshot!("exits_exited_under_edits", vix.screen());
    vix.keys(":w\r");
    assert_snapshot!("exits_written", vix.screen());
}

#[test]
fn view_n_motions_and_search() {
    let home = Home::new();
    let mut vix = Vix::start(&home, &[]);
    vix.keys("\ri").keys("echo one\r").keys("echo two\r").keys("\x1c");
    vix.keys("gg");
    assert_snapshot!("view_n_top", vix.screen());
    vix.keys("/two\r");
    assert_snapshot!("view_n_searched", vix.screen());
    vix.keys("vey");
    assert_snapshot!("view_n_yanked", vix.screen());
    vix.keys("i");
    assert_snapshot!("view_n_back_to_insert", vix.screen());
}

#[test]
fn picker_filters_and_opens() {
    let home = Home::new();
    let mut vix = Vix::start(&home, &[]);
    vix.keys("cwweb\x1b").keys("oapi\x1b").keys("ologs\x1b").keys(":w\r");
    vix.keys("\x10");
    assert_snapshot!("picker_open", vix.screen());
    vix.keys("ap");
    assert_snapshot!("picker_filtered", vix.screen());
    vix.keys("\r");
    assert_snapshot!("picker_opened", vix.screen());
}

#[test]
fn bar_overflow_slides_around_current() {
    let home = Home::new();
    let mut vix = Vix::sized(&home, &[], 40, 8);
    vix.keys("cwapi\x1b").keys("yy5p").keys(":w\r").keys("G\r");
    assert_snapshot!("bar_last", vix.screen());
    vix.keys("\x1c").keys("1");
    assert_snapshot!("bar_first", vix.screen());
}

#[test]
fn quit_refuses_unsaved_list() {
    let home = Home::new();
    let mut vix = Vix::start(&home, &[]);
    vix.keys("oapi\x1b").keys(":qa\r");
    assert_snapshot!("quit_refused", vix.screen());
    vix.keys(":qa!\r");
    assert!(vix.exited());
}

#[test]
fn workflow_save_and_load() {
    let home = Home::new();
    let mut vix = Vix::start(&home, &["demo"]);
    assert_snapshot!("workflow_new", vix.screen());
    vix.keys("cwweb\x1b").keys("oapi\x1b").keys(":w\r");
    assert_snapshot!("workflow_written", vix.screen());
    assert_snapshot!("workflow_file", read(&home.path("config/vix/workflows/demo.toml")));
    vix.keys(":qa\r");
    assert!(vix.exited());

    std::fs::write(home.path("config/vix/workflows/cmd.toml"), "[[term]]\nname = \"dev\"\ncwd = \"~/work\"\ncmd = \"echo from-cmd\"\n").unwrap();
    let mut vix = Vix::start(&home, &["cmd"]);
    vix.keys("\r");
    assert_snapshot!("workflow_cmd_ran", vix.screen());
}

#[test]
fn resume_restores_names_and_output() {
    let home = Home::new();
    let mut vix = Vix::start(&home, &[]);
    vix.keys("cwkeep\x1b").keys(":w\r").keys("\ri").keys("echo persisted\r").keys("\x1c").keys(":qa\r");
    assert!(vix.exited());
    let mut vix = Vix::start(&home, &["-r"]);
    assert_snapshot!("resume_list", vix.screen());
    vix.keys("\r");
    assert_snapshot!("resume_output", vix.screen());
}

#[test]
fn detach_and_reattach_keep_the_screen() {
    let home = Home::new();
    let mut vix = Vix::start(&home, &["demo"]);
    vix.keys("\ri").keys("echo kept\r");
    let before = vix.screen();
    assert!(before.contains("$ echo kept\nkept\n$"), "{before}");
    vix.keys("\x1c").keys(":qd\r");
    assert!(vix.exited());
    assert!(vix.screen().contains("[detached from demo]"), "{}", vix.screen());

    // lands back in view i on what was viewed, as it was
    let vix = Vix::start(&home, &["demo"]);
    assert_eq!(vix.screen(), before);
    // closing the window detaches too
    drop(vix);
    let mut vix = Vix::start(&home, &["demo"]);
    assert_eq!(vix.screen(), before);

    vix.keys("\x1c-").keys("oapi\x1b").keys(":qd\r");
    assert_snapshot!("detach_refused", vix.screen());
}

#[test]
fn list_attach_and_nesting() {
    let home = Home::new();
    let vix_bin = env!("CARGO_BIN_EXE_vix");
    let mut vix = Vix::start(&home, &["demo"]);
    vix.keys("\x1c").keys(":qd\r");
    assert!(vix.exited());

    // from a shell inside session 0
    let mut vix = Vix::sized(&home, &[], 120, 12);
    vix.keys("\ri").keys(&format!("{vix_bin} -l\r"));
    let screen = vix.screen();
    assert!(screen.contains("\n0: 1 terminal (attached)\ndemo: 1 terminal\n"), "{screen}");
    vix.keys(&format!("{vix_bin} -a 0\r"));
    assert!(vix.screen().contains("already inside session 0"), "{}", vix.screen());

    // the session detached last
    let mut other = Vix::start(&home, &["-a"]);
    other.keys("\x1c").keys(":qd\r");
    assert!(other.exited());
    assert!(other.screen().contains("[detached from demo]"), "{}", other.screen());

    let mut none = Vix::start(&home, &["-a", "nope"]);
    assert!(none.exited());
    assert!(none.screen().contains("no session nope"), "{}", none.screen());
}
