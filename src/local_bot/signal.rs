//! Signals and waits that only ever reach the process a verified record
//! names. A target that fails re-verification is never signalled.
//!
//! Linux opens a pidfd first and re-verifies after, so the signal cannot hit
//! a reused pid. macOS re-verifies, then `kill` (a small race, accepted).
//! Windows checks the creation time on the same handle it terminates.

use std::time::{Duration, Instant};

/// A process from a verified record: pid and OS start token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub pid: u32,
    pub start: String,
}

#[derive(Debug)]
pub enum SignalError {
    /// The pid now names another process, or cannot be checked.
    NotVerified,
    Gone,
    Denied,
    Io(std::io::Error),
}

impl std::fmt::Display for SignalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignalError::NotVerified => write!(f, "cannot verify the process is this bot"),
            SignalError::Gone => write!(f, "the process is gone"),
            SignalError::Denied => write!(f, "permission denied"),
            SignalError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SignalError {}

fn in_range(t: &Target) -> Result<(), SignalError> {
    if (2..=i32::MAX as u32).contains(&t.pid) && !t.start.is_empty() {
        Ok(())
    } else {
        Err(SignalError::NotVerified)
    }
}

/// `t` is still the process it names. (Windows checks on its own handle.)
#[cfg_attr(windows, allow(dead_code))]
fn verify(t: &Target) -> Result<(), SignalError> {
    use super::proc::{lookup_identity, Lookup};
    match lookup_identity(t.pid) {
        Lookup::Alive(p) if p.start == t.start => Ok(()),
        Lookup::Alive(_) => Err(SignalError::NotVerified),
        Lookup::Gone => Err(SignalError::Gone),
        Lookup::Denied => Err(SignalError::Denied),
        Lookup::Unsupported => Err(SignalError::NotVerified),
    }
}

/// SIGTERM (Unix). Windows stops a bot through `POST /v1/stop` instead, so
/// this is `Io(Unsupported)` there.
pub fn terminate(t: &Target) -> Result<(), SignalError> {
    in_range(t)?;
    #[cfg(unix)]
    {
        unix::send(t, libc::SIGTERM)
    }
    #[cfg(not(unix))]
    {
        Err(SignalError::Io(std::io::Error::from(std::io::ErrorKind::Unsupported)))
    }
}

/// `--force`: SIGKILL / `TerminateProcess`. Positions stay open.
pub fn kill(t: &Target) -> Result<(), SignalError> {
    in_range(t)?;
    #[cfg(unix)]
    {
        unix::send(t, libc::SIGKILL)
    }
    #[cfg(windows)]
    {
        win::kill(t)
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(SignalError::Io(std::io::Error::from(std::io::ErrorKind::Unsupported)))
    }
}

/// Wait until the process `t` names is gone (exited, or its pid now names
/// another process), at most until `deadline`. Not the port and not the
/// lock: Windows can free a dead process's lock a moment late.
pub fn wait_gone(t: &Target, deadline: Instant) -> bool {
    if in_range(t).is_err() {
        return true;
    }
    #[cfg(target_os = "linux")]
    if let Some(gone) = unix::wait_pidfd(t, deadline) {
        return gone;
    }
    #[cfg(windows)]
    if let Some(gone) = win::wait(t, deadline) {
        return gone;
    }
    poll_gone(t, deadline)
}

/// A 250 ms loop on `lookup`.
fn poll_gone(t: &Target, deadline: Instant) -> bool {
    use super::proc::{lookup_identity, Lookup};
    loop {
        match lookup_identity(t.pid) {
            Lookup::Alive(p) if p.start == t.start => {}
            Lookup::Alive(_) | Lookup::Gone => return true,
            // Cannot tell: keep waiting.
            Lookup::Denied | Lookup::Unsupported => {}
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(250).min(deadline - now));
    }
}

#[cfg(unix)]
mod unix {
    use super::{verify, SignalError, Target};

    fn errno() -> i32 {
        std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
    }

    fn kill_error(e: i32) -> SignalError {
        match e {
            libc::ESRCH => SignalError::Gone,
            libc::EPERM => SignalError::Denied,
            _ => SignalError::Io(std::io::Error::from_raw_os_error(e)),
        }
    }

    pub(super) fn send(t: &Target, sig: libc::c_int) -> Result<(), SignalError> {
        #[cfg(target_os = "linux")]
        if let Some(r) = send_pidfd(t, sig) {
            return r;
        }
        verify(t)?;
        if unsafe { libc::kill(t.pid as libc::pid_t, sig) } == 0 {
            Ok(())
        } else {
            Err(kill_error(errno()))
        }
    }

    /// `pidfd_open`, `None` when the kernel has no pidfd (before 5.3).
    #[cfg(target_os = "linux")]
    fn pidfd(pid: u32) -> Option<Result<std::os::fd::OwnedFd, i32>> {
        use std::os::fd::FromRawFd;
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::c_int, 0) };
        if fd >= 0 {
            return Some(Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd as libc::c_int) }));
        }
        match errno() {
            libc::ENOSYS => None,
            e => Some(Err(e)),
        }
    }

    /// Open a pidfd, re-verify, signal through the pidfd: the signal cannot
    /// reach a process that took the pid after the check.
    #[cfg(target_os = "linux")]
    fn send_pidfd(t: &Target, sig: libc::c_int) -> Option<Result<(), SignalError>> {
        use std::os::fd::AsRawFd;
        let fd = match pidfd(t.pid)? {
            Ok(fd) => fd,
            Err(e) => return Some(Err(kill_error(e))),
        };
        if let Err(e) = verify(t) {
            return Some(Err(e));
        }
        let r = unsafe {
            libc::syscall(libc::SYS_pidfd_send_signal, fd.as_raw_fd(), sig, std::ptr::null::<libc::siginfo_t>(), 0)
        };
        match r {
            0 => Some(Ok(())),
            _ => match errno() {
                // Old kernel with pidfd_open but no send: fall back.
                libc::ENOSYS => None,
                e => Some(Err(kill_error(e))),
            },
        }
    }

    /// `poll` on a verified pidfd. `None`: no pidfd here, use the loop.
    #[cfg(target_os = "linux")]
    pub(super) fn wait_pidfd(t: &Target, deadline: std::time::Instant) -> Option<bool> {
        use std::os::fd::AsRawFd;
        let fd = match pidfd(t.pid)? {
            Ok(fd) => fd,
            Err(_) => return Some(true),
        };
        match verify(t) {
            Ok(()) => {}
            Err(SignalError::Gone | SignalError::NotVerified) => return Some(true),
            Err(_) => return None,
        }
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let mut p = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            let ms = left.as_millis().min(i32::MAX as u128) as libc::c_int;
            match unsafe { libc::poll(&mut p, 1, ms) } {
                n if n > 0 => return Some(true),
                0 => return Some(false),
                _ if errno() == libc::EINTR => continue,
                _ => return None,
            }
        }
    }
}

#[cfg(windows)]
mod win {
    use super::{SignalError, Target};
    use crate::local_bot::proc::windows::{creation, open, token};
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        TerminateProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        PROCESS_TERMINATE,
    };

    fn open_err(code: u32) -> SignalError {
        match code {
            ERROR_INVALID_PARAMETER => SignalError::Gone,
            ERROR_ACCESS_DENIED => SignalError::Denied,
            c => SignalError::Io(std::io::Error::from_raw_os_error(c as i32)),
        }
    }

    pub(super) fn kill(t: &Target) -> Result<(), SignalError> {
        let p = open(t.pid, PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE)
            .map_err(open_err)?;
        if creation(&p).map(token).as_deref() != Some(t.start.as_str()) {
            return Err(SignalError::NotVerified);
        }
        if unsafe { TerminateProcess(p.0, 1) } == 0 {
            return Err(SignalError::Io(std::io::Error::last_os_error()));
        }
        Ok(())
    }

    /// `WaitForSingleObject` on a verified handle. `None`: cannot open it
    /// for waiting (denied), use the loop.
    pub(super) fn wait(t: &Target, deadline: std::time::Instant) -> Option<bool> {
        let p = match open(t.pid, PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION) {
            Ok(p) => p,
            Err(ERROR_INVALID_PARAMETER) => return Some(true),
            Err(_) => return None,
        };
        if creation(&p).map(token).as_deref() != Some(t.start.as_str()) {
            return Some(true);
        }
        let ms = deadline.saturating_duration_since(std::time::Instant::now()).as_millis().min(u32::MAX as u128 - 1);
        Some(unsafe { WaitForSingleObject(p.0, ms as u32) } == WAIT_OBJECT_0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_bot::proc::{lookup, Lookup};

    fn target_of(child: &std::process::Child) -> Target {
        let Lookup::Alive(p) = lookup(child.id()) else { panic!("alive") };
        Target { pid: child.id(), start: p.start }
    }

    #[test]
    fn kill_reaches_a_verified_child_and_wait_sees_it_gone() {
        let mut child = crate::local_bot::proc::tests::idle_child();
        let t = target_of(&child);
        assert!(!wait_gone(&t, Instant::now() + Duration::from_millis(300)), "still alive");
        kill(&t).unwrap();
        assert!(wait_gone(&t, Instant::now() + Duration::from_secs(5)));
        child.wait().unwrap();
        assert!(matches!(kill(&t), Err(SignalError::Gone | SignalError::NotVerified)));
    }

    #[cfg(unix)]
    #[test]
    fn terminate_reaches_a_verified_child() {
        use std::os::unix::process::ExitStatusExt;
        let mut child = crate::local_bot::proc::tests::idle_child();
        let t = target_of(&child);
        terminate(&t).unwrap();
        let status = child.wait().unwrap();
        assert_eq!(status.signal(), Some(libc::SIGTERM));
    }

    #[test]
    fn a_wrong_start_is_never_signalled() {
        let mut child = crate::local_bot::proc::tests::idle_child();
        let mut t = target_of(&child);
        t.start.push('9');
        assert!(matches!(kill(&t), Err(SignalError::NotVerified)));
        assert!(matches!(terminate(&t), Err(SignalError::NotVerified | SignalError::Io(_))));
        assert!(matches!(lookup(child.id()), Lookup::Alive(_)), "untouched");
        // A record naming another process: its process is "gone".
        assert!(wait_gone(&t, Instant::now() + Duration::from_secs(1)));
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn pids_out_of_range_are_refused() {
        for pid in [0, 1, i32::MAX as u32 + 1, u32::MAX] {
            let t = Target { pid, start: "x".into() };
            assert!(matches!(kill(&t), Err(SignalError::NotVerified)), "{pid}");
            assert!(matches!(terminate(&t), Err(SignalError::NotVerified)), "{pid}");
        }
        let empty = Target { pid: std::process::id(), start: String::new() };
        assert!(matches!(kill(&empty), Err(SignalError::NotVerified)));
    }
}
