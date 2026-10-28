//! A hidden top-level window that hears the session end (section 3.2):
//! shut down, restart, sign out, or a Restart Manager close. Not a
//! message-only window: those get no broadcasts. Once user32 is loaded the
//! console handler no longer gets `CTRL_LOGOFF_EVENT` or
//! `CTRL_SHUTDOWN_EVENT`; this window gets the end instead.
//!
//! The first `WM_QUERYENDSESSION` starts the stop. The window then holds the
//! shutdown screen with a reason until the process exits, and Windows goes
//! on by itself. A cancelled end never undoes the stop.

use std::cell::RefCell;
use std::io;
use std::sync::mpsc;

use windows_sys::Win32::Foundation::{GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Shutdown::{ShutdownBlockReasonCreate, ShutdownBlockReasonDestroy};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassExW, TranslateMessage,
    ENDSESSION_CLOSEAPP, ENDSESSION_CRITICAL, ENDSESSION_LOGOFF, MSG, WM_CLOSE, WM_ENDSESSION, WM_QUERYENDSESSION,
    WNDCLASSEXW, WS_OVERLAPPED,
};

use super::wide;

/// The window class. With [`title`] it finds a bot's window.
pub const CLASS: &str = "tradectl-bot-session";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionEnd {
    Shutdown,
    Logoff,
    /// Restart Manager (an installer) asked the process to close.
    CloseApp,
}

impl SessionEnd {
    fn from_flags(flags: u32) -> SessionEnd {
        if flags & ENDSESSION_LOGOFF != 0 {
            SessionEnd::Logoff
        } else if flags & ENDSESSION_CLOSEAPP != 0 {
            SessionEnd::CloseApp
        } else {
            SessionEnd::Shutdown
        }
    }
}

/// The window title of bot `bot`.
pub fn title(bot: &str) -> String {
    format!("tradectl {bot}")
}

/// The text on the shutdown screen while the bot closes its positions.
pub fn reason(bot: &str) -> String {
    format!("tradectl is closing the positions of bot {bot}. Please wait.")
}

struct State {
    /// Taken at the first end, so it runs once and outside the borrow.
    on_end: Option<Box<dyn Fn(SessionEnd) + Send + 'static>>,
    reason: Vec<u16>,
}

thread_local! {
    // The window procedure always runs on the thread that made the window.
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Start the watch: one thread for the process life, which made the window
/// when this returns `Ok`. `on_end` runs once, on that thread, at the first
/// session end; it must return quickly.
pub fn watch(bot: &str, on_end: Box<dyn Fn(SessionEnd) + Send + 'static>) -> io::Result<()> {
    let (title, reason) = (wide(&title(bot)), wide(&reason(bot)));
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new().name("session-watch".into()).spawn(move || {
        STATE.with(|s| *s.borrow_mut() = Some(State { on_end: Some(on_end), reason }));
        let made = create_window(&title);
        let ok = made.is_ok();
        let _ = tx.send(made);
        if !ok {
            return;
        }
        let mut msg: MSG = unsafe { std::mem::zeroed() };
        while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    })?;
    rx.recv().unwrap_or_else(|_| Err(io::Error::other("the session watch thread ended")))
}

fn create_window(title: &[u16]) -> io::Result<()> {
    let class = wide(CLASS);
    let module = unsafe { GetModuleHandleW(std::ptr::null()) };
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(window_proc),
        hInstance: module,
        lpszClassName: class.as_ptr(),
        ..unsafe { std::mem::zeroed() }
    };
    if unsafe { RegisterClassExW(&wc) } == 0 {
        let code = unsafe { GetLastError() };
        if code != ERROR_CLASS_ALREADY_EXISTS {
            return Err(io::Error::from_raw_os_error(code as i32));
        }
    }
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            module,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Run `on_end` if this is the first end, then show the reason.
fn session_ends(hwnd: HWND, flags: u32) {
    let taken = STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut()?.on_end.take()));
    if let Some(on_end) = taken {
        on_end(SessionEnd::from_flags(flags));
    }
    STATE.with(|s| {
        if let Ok(s) = s.try_borrow() {
            if let Some(st) = s.as_ref() {
                unsafe { ShutdownBlockReasonCreate(hwnd, st.reason.as_ptr()) };
            }
        }
    });
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_QUERYENDSESSION => {
            session_ends(hwnd, lparam as u32);
            // A critical end waits for nobody: say yes and go on closing.
            if lparam as u32 & ENDSESSION_CRITICAL != 0 {
                1
            } else {
                0
            }
        }
        WM_ENDSESSION if wparam != 0 => {
            session_ends(hwnd, lparam as u32);
            // Windows may end the process once this returns: hold here
            // until the stop is done and the process exits by itself.
            loop {
                std::thread::park();
            }
        }
        WM_ENDSESSION => {
            // The user cancelled; the stop goes on.
            unsafe { ShutdownBlockReasonDestroy(hwnd) };
            0
        }
        // `taskkill` without /F: keep the window, or the session end would
        // go unheard. Stop a bot with `tradectl stop`.
        WM_CLOSE => 0,
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, SendMessageW};

    #[test]
    fn flags_name_the_end() {
        assert_eq!(SessionEnd::from_flags(0), SessionEnd::Shutdown);
        assert_eq!(SessionEnd::from_flags(ENDSESSION_LOGOFF), SessionEnd::Logoff);
        assert_eq!(SessionEnd::from_flags(ENDSESSION_CLOSEAPP), SessionEnd::CloseApp);
        assert_eq!(SessionEnd::from_flags(ENDSESSION_CRITICAL), SessionEnd::Shutdown);
        assert_eq!(SessionEnd::from_flags(ENDSESSION_LOGOFF | ENDSESSION_CRITICAL), SessionEnd::Logoff);
    }

    #[test]
    fn a_session_end_starts_the_stop_once_and_holds() {
        let bot = format!("test-{}-{:x}", std::process::id(), rand::random::<u32>());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        watch(&bot, Box::new(move |k| s.lock().unwrap().push(k))).unwrap();
        let (class, name) = (wide(CLASS), wide(&title(&bot)));
        let hwnd = unsafe { FindWindowW(class.as_ptr(), name.as_ptr()) };
        assert!(!hwnd.is_null(), "the window exists");
        let query = |flags: u32| unsafe { SendMessageW(hwnd, WM_QUERYENDSESSION, 0, flags as LPARAM) };
        assert_eq!(query(ENDSESSION_LOGOFF), 0, "held");
        assert_eq!(query(0), 0, "still held");
        assert_eq!(*seen.lock().unwrap(), vec![SessionEnd::Logoff], "the stop starts once");
        assert_eq!(unsafe { SendMessageW(hwnd, WM_ENDSESSION, 0, 0) }, 0, "cancelled");
        assert_eq!(query(ENDSESSION_CRITICAL), 1, "a critical end is not held");
        assert_eq!(unsafe { SendMessageW(hwnd, WM_CLOSE, 0, 0) }, 0);
        assert_eq!(unsafe { FindWindowW(class.as_ptr(), name.as_ptr()) }, hwnd, "WM_CLOSE keeps it");
        assert_eq!(seen.lock().unwrap().len(), 1);
    }
}
