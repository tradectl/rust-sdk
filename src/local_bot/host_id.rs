//! A stable id for this computer.

use std::sync::OnceLock;

/// First match wins: `TRADECTL_HOST_ID`, macOS `IOPlatformUUID`, Linux
/// `/etc/machine-id` then `/var/lib/dbus/machine-id`, Windows `MachineGuid`.
/// Never the hostname: it changes with the network on a Mac and per
/// container. Cached per process (macOS runs `ioreg`, and the Lab polls).
pub fn host_id() -> Option<String> {
    static ID: OnceLock<Option<String>> = OnceLock::new();
    ID.get_or_init(|| host_id_from(std::env::var("TRADECTL_HOST_ID").ok())).clone()
}

/// [`host_id`] with the env value given, not cached.
pub fn host_id_from(env: Option<String>) -> Option<String> {
    if let Some(v) = env.map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) {
        return Some(v);
    }
    os_host_id()
}

#[cfg(target_os = "macos")]
fn os_host_id() -> Option<String> {
    let out = std::process::Command::new("ioreg").args(["-rd1", "-c", "IOPlatformExpertDevice"]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find(|l| l.contains("\"IOPlatformUUID\""))
        .and_then(|l| l.rsplit('"').nth(1))
        .map(str::to_string)
        .filter(|v| !v.is_empty())
}

#[cfg(windows)]
fn os_host_id() -> Option<String> {
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY,
    };
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (key, value) = (wide("SOFTWARE\\Microsoft\\Cryptography"), wide("MachineGuid"));
    let mut buf = [0u16; 128];
    let mut len = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut len,
        )
    };
    if r != 0 {
        return None;
    }
    let chars = (len as usize / 2).min(buf.len());
    let s = String::from_utf16_lossy(&buf[..chars]).trim_end_matches('\0').trim().to_string();
    (!s.is_empty()).then_some(s)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn os_host_id() -> Option<String> {
    ["/etc/machine-id", "/var/lib/dbus/machine-id"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
}

/// Linux containers (`/.dockerenv`, `/run/.containerenv`, `$container`): a
/// recreated container may get a new machine id, so no host check there.
pub fn in_container() -> bool {
    cfg!(target_os = "linux")
        && (std::path::Path::new("/.dockerenv").exists()
            || std::path::Path::new("/run/.containerenv").exists()
            || std::env::var_os("container").is_some_and(|v| !v.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_env_wins_and_blank_is_ignored() {
        assert_eq!(host_id_from(Some(" box-1 ".into())), Some("box-1".into()));
        assert_eq!(host_id_from(Some("   ".into())), host_id_from(None));
        if cfg!(any(target_os = "macos", windows)) {
            assert!(host_id_from(None).is_some(), "this OS always has one");
        }
        assert_eq!(host_id(), host_id(), "cached");
    }
}
