//! Process signalling helpers (graceful TERM, then KILL) shared by the reaper, the controller
//! lease check and the worktree pool.





#[allow(
    unsafe_code,
    reason = "signal 0 is the portable non-Linux Unix process-existence probe"
)]
pub(crate) fn process_exists(pid: i32) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        // SAFETY: kill with signal 0 only checks process existence/permission.
        unsafe { libc::kill(pid, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "sending a caller-selected Unix signal requires libc::kill"
)]
pub(crate) fn send_signal(pid: i32, sig: i32) -> std::io::Result<()> {
    // SAFETY: libc::kill does not dereference pointers; pid/signal are plain integers.
    let rc = unsafe { libc::kill(pid, sig) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
pub(crate) fn send_signal(_pid: i32, _sig: i32) -> std::io::Result<()> {
    Ok(())
}

pub(crate) fn signal_term() -> i32 {
    #[cfg(unix)]
    {
        libc::SIGTERM
    }
    #[cfg(not(unix))]
    {
        0
    }
}

pub(crate) fn signal_kill() -> i32 {
    #[cfg(unix)]
    {
        libc::SIGKILL
    }
    #[cfg(not(unix))]
    {
        0
    }
}
