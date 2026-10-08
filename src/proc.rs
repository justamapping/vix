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

#[cfg(test)]
mod tests {
    #[test]
    fn own_cwd() {
        assert_eq!(super::cwd(std::process::id()), std::env::current_dir().ok());
    }
}
