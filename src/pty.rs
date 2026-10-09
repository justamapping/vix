use std::io::{Read, Write};
use std::path::Path;

use anyhow::Result;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

pub struct Pty {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
}

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }
}

impl Pty {
    /// Spawns the user's shell in `cwd`.
    pub fn spawn(cols: u16, rows: u16, cwd: &Path) -> Result<Self> {
        let pair = native_pty_system().openpty(size(cols, rows))?;
        let mut cmd = CommandBuilder::new_default_prog();
        cmd.cwd(cwd);
        let child = pair.slave.spawn_command(cmd)?;
        let writer = pair.master.take_writer()?;
        Ok(Self { master: pair.master, writer, child })
    }

    pub fn reader(&self) -> Result<Box<dyn Read + Send>> {
        self.master.try_clone_reader()
    }

    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.master.resize(size(cols, rows))
    }

    /// Whether the program reads keys as they come, like a line editor, rather than the kernel's cooked lines.
    pub fn raw(&self) -> bool {
        let Some(fd) = self.master.as_raw_fd() else { return false };
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        unsafe { libc::tcgetattr(fd, &mut t) == 0 && t.c_lflag & libc::ICANON == 0 }
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.process_id()
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }
}
