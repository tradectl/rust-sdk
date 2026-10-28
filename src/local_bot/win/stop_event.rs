//! `Local\tradectl-stop-<run_id>`: a manual-reset event the holder creates
//! and waits on, and `tradectl stop` sets (section 3.3). It stops a bot that
//! has no port. The name is per logon session: a CLI in another session (SSH,
//! a service) gets `Ok(false)` and falls back to the port.

use std::io;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_FILE_NOT_FOUND, HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{
    CreateEventW, OpenEventW, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE, INFINITE,
};

fn name(run_id: &str) -> Vec<u16> {
    super::wide(&format!("Local\\tradectl-stop-{run_id}"))
}

/// The holder's event. Keep it for the process life: the name goes with
/// its last handle, and [`set`] then answers `Ok(false)`.
pub struct StopEvent(HANDLE);

// An event handle may be used from any thread.
unsafe impl Send for StopEvent {}
unsafe impl Sync for StopEvent {}

impl StopEvent {
    /// Block until the event is set.
    pub fn wait(&self) {
        unsafe { WaitForSingleObject(self.0, INFINITE) };
    }

    /// Set already? Never waits.
    pub fn is_set(&self) -> bool {
        unsafe { WaitForSingleObject(self.0, 0) == WAIT_OBJECT_0 }
    }
}

impl Drop for StopEvent {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

/// The event of run `run_id`, not set. Default security: this user.
pub fn create(run_id: &str) -> io::Result<StopEvent> {
    let n = name(run_id);
    let h = unsafe { CreateEventW(std::ptr::null(), 1, 0, n.as_ptr()) };
    if h.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(StopEvent(h))
}

/// Set the event of run `run_id`. `Ok(false)`: there is none (a holder from
/// before Phase 3, or another session).
pub fn set(run_id: &str) -> io::Result<bool> {
    let n = name(run_id);
    let h = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, n.as_ptr()) };
    if h.is_null() {
        let code = unsafe { GetLastError() };
        if code == ERROR_FILE_NOT_FOUND {
            return Ok(false);
        }
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    let r = if unsafe { SetEvent(h) } != 0 { Ok(true) } else { Err(io::Error::last_os_error()) };
    unsafe { CloseHandle(h) };
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_reaches_the_holder() {
        let id = format!("test-{}-{:x}", std::process::id(), rand::random::<u64>());
        assert!(!set(&id).unwrap(), "no event yet");
        let ev = create(&id).unwrap();
        assert!(!ev.is_set());
        assert!(set(&id).unwrap());
        assert!(ev.is_set());
        ev.wait();
        assert!(ev.is_set(), "manual reset: it stays set");
        drop(ev);
        assert!(!set(&id).unwrap(), "gone with its last handle");
    }
}
