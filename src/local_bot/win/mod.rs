//! Windows helpers: std handles, and (Windows only) the session-end window,
//! the stop event and the autostart Run value.

#[cfg(windows)]
pub mod run_key;
#[cfg(windows)]
pub mod session;
#[cfg(windows)]
pub mod stop_event;

/// `s` as a NUL-terminated UTF-16 string.
#[cfg(windows)]
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Clear `HANDLE_FLAG_INHERIT` on this process's own std handles, so a
/// caller's pipe never leaks into a child: std spawns with
/// `bInheritHandles = TRUE`, and a caller reading to EOF would hang for the
/// bot's life. A child's explicit `Stdio` still works (std duplicates those
/// handles itself). A no-op off Windows.
pub fn no_inherit_std_handles() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Console::{
            GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
        };
        for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let h = unsafe { GetStdHandle(which) };
            if !h.is_null() && h != INVALID_HANDLE_VALUE {
                unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn clearing_twice_is_harmless() {
        super::no_inherit_std_handles();
        super::no_inherit_std_handles();
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT};
            use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
            let h = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
            let mut flags = 0u32;
            if !h.is_null() && unsafe { GetHandleInformation(h, &mut flags) } != 0 {
                assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}
