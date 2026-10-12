use std::path::PathBuf;

/// Current directory of a process, asked from the OS like tmux does.
#[cfg(target_os = "macos")]
pub fn cwd(pid: u32) -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;

    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    let n = unsafe { libc::proc_pidinfo(pid as libc::c_int, libc::PROC_PIDVNODEPATHINFO, 0, (&raw mut info).cast(), size) };
    if n != size {
        return None;
    }
    let path = unsafe { CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr().cast()) };
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(path.to_bytes())))
}

#[cfg(not(target_os = "macos"))]
pub fn cwd(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// A process's name, like `ps -o comm`.
#[cfg(target_os = "macos")]
pub fn name(pid: u32) -> Option<String> {
    let mut buf = [0u8; 256];
    let n = unsafe { libc::proc_name(pid as libc::c_int, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

#[cfg(not(target_os = "macos"))]
pub fn name(pid: u32) -> Option<String> {
    Some(std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn own_cwd() {
        assert_eq!(super::cwd(std::process::id()), std::env::current_dir().ok());
    }

    #[test]
    fn own_name() {
        let exe = std::env::current_exe().unwrap().file_name().unwrap().to_string_lossy().into_owned();
        assert!(exe.starts_with(&super::name(std::process::id()).unwrap()));
    }
}
