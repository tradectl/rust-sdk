//! Values under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`: Windows
//! runs each one's command when this user logs in (section 3.5).

use std::io;

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, WIN32_ERROR};
use windows_sys::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};

use super::wide;

pub const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
/// The value `tradectl autostart on` writes.
pub const AUTOSTART: &str = "tradectl-autostart";

/// The command of value `name`; `None` when there is none.
pub fn get(name: &str) -> io::Result<Option<String>> {
    get_in(RUN_KEY, name)
}

/// Write value `name` (`REG_SZ`).
pub fn set(name: &str, command: &str) -> io::Result<()> {
    set_in(RUN_KEY, name, command)
}

/// Remove value `name`. `Ok(false)`: there was none.
pub fn remove(name: &str) -> io::Result<bool> {
    remove_in(RUN_KEY, name)
}

fn os_err(code: WIN32_ERROR) -> io::Error {
    io::Error::from_raw_os_error(code as i32)
}

fn get_in(key: &str, name: &str) -> io::Result<Option<String>> {
    let (key, name) = (wide(key), wide(name));
    let mut buf = vec![0u16; 512];
    // The value may grow between the calls; a few tries are plenty.
    for _ in 0..4 {
        let mut len = (buf.len() * 2) as u32;
        let r = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut len,
            )
        };
        match r {
            0 => {
                let chars = (len as usize / 2).min(buf.len());
                return Ok(Some(String::from_utf16_lossy(&buf[..chars]).trim_end_matches('\0').to_string()));
            }
            ERROR_FILE_NOT_FOUND => return Ok(None),
            ERROR_MORE_DATA => buf = vec![0u16; (len as usize).div_ceil(2) + 1],
            e => return Err(os_err(e)),
        }
    }
    Err(os_err(ERROR_MORE_DATA))
}

fn set_in(key: &str, name: &str, command: &str) -> io::Result<()> {
    let (key, name, data) = (wide(key), wide(name), wide(command));
    let r = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            REG_SZ,
            data.as_ptr().cast(),
            (data.len() * 2) as u32,
        )
    };
    if r == 0 {
        Ok(())
    } else {
        Err(os_err(r))
    }
}

fn remove_in(key: &str, name: &str) -> io::Result<bool> {
    let (key, name) = (wide(key), wide(name));
    match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) } {
        0 => Ok(true),
        ERROR_FILE_NOT_FOUND => Ok(false),
        e => Err(os_err(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key of this test's own, never the real Run key.
    struct TestKey(String);

    impl Drop for TestKey {
        fn drop(&mut self) {
            use windows_sys::Win32::System::Registry::RegDeleteKeyW;
            let k = wide(&self.0);
            unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, k.as_ptr()) };
        }
    }

    #[test]
    fn a_value_is_set_read_and_removed() {
        let key = TestKey(format!("Software\\tradectl-test-{}-{:x}", std::process::id(), rand::random::<u64>()));
        assert_eq!(get_in(&key.0, AUTOSTART).unwrap(), None, "no key yet");
        let cmd = "\"C:\\Program Files\\tradectl\\tradectl-lab.exe\" --autostart-run --home \"C:\\Users\\Анна\\.tradectl\"";
        set_in(&key.0, AUTOSTART, cmd).unwrap();
        assert_eq!(get_in(&key.0, AUTOSTART).unwrap().as_deref(), Some(cmd));
        let long = "x".repeat(2000);
        set_in(&key.0, AUTOSTART, &long).unwrap();
        assert_eq!(get_in(&key.0, AUTOSTART).unwrap(), Some(long), "longer than the first buffer");
        assert!(remove_in(&key.0, AUTOSTART).unwrap());
        assert!(!remove_in(&key.0, AUTOSTART).unwrap());
        assert_eq!(get_in(&key.0, AUTOSTART).unwrap(), None);
    }
}
