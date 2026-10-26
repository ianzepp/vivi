//! Process hardening applied at boot, before any secret is read.

/// Make the process non-dumpable (`prctl(PR_SET_DUMPABLE, 0)`) on Linux.
///
/// The kernel then owns `/proc/<pid>/*` as root, so another process running
/// as the same user can no longer read this one's `environ` or `mem`, and a
/// crash writes no core dump. systemd copies `EnvironmentFile=` values into
/// the process environment block, which `/proc/<pid>/environ` keeps for the
/// life of the process; this closes that read. A no-op elsewhere.
///
/// # Errors
/// Returns the OS error when the kernel refuses the call.
#[cfg(target_os = "linux")]
pub fn disable_dumping() -> std::io::Result<()> {
    let zero: libc::c_ulong = 0;
    // SAFETY: `prctl(PR_SET_DUMPABLE, 0)` takes plain integers, reads and
    // writes no memory of ours, and has no preconditions.
    let rc = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, zero, zero, zero, zero) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// See the Linux version: there is nothing to do on this platform.
///
/// # Errors
/// Never fails here.
#[cfg(not(target_os = "linux"))]
pub fn disable_dumping() -> std::io::Result<()> {
    Ok(())
}

/// Remove each named variable from the process environment, so a child
/// process does not inherit it. The environment block that
/// `/proc/<pid>/environ` shows is not changed by this; see
/// [`disable_dumping`].
///
/// Call it only while the process is single-threaded, before the async
/// runtime exists: the environment is not safe to change beside a thread
/// that reads it.
pub fn scrub_env(names: &[&str]) {
    for name in names {
        // SAFETY: the caller runs this on the boot path before any other
        // thread exists (see above), so nothing reads the environment
        // concurrently.
        unsafe { std::env::remove_var(name) };
    }
}
