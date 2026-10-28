//! Linux: `/proc`.

use std::io;
use std::path::PathBuf;
use std::sync::OnceLock;

use super::{parse_stat, Lookup, ProcInfo};

pub(super) fn lookup(pid: u32, full: bool) -> Lookup {
    if !proc_mounted() {
        return Lookup::Unsupported;
    }
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ESRCH) => return Lookup::Gone,
        Err(_) => return Lookup::Denied,
    };
    let Some((state, ticks)) = parse_stat(&stat) else { return Lookup::Denied };
    if matches!(state, 'Z' | 'X' | 'x') {
        return Lookup::Gone;
    }
    let argv = full.then(|| std::fs::read(format!("/proc/{pid}/cmdline")).ok()).flatten().and_then(|raw| {
        let mut args: Vec<String> = raw.split(|&b| b == 0).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
        if args.last().is_some_and(String::is_empty) {
            args.pop();
        }
        (!args.is_empty()).then_some(args)
    });
    Lookup::Alive(ProcInfo {
        start: format!("lnx:{ticks}"),
        start_unix_ms: start_unix_ms(ticks),
        argv,
        exe: full.then(|| std::fs::read_link(format!("/proc/{pid}/exe")).ok().map(PathBuf::from)).flatten(),
    })
}

/// Without `/proc` every pid would read as gone; say "cannot tell" instead.
fn proc_mounted() -> bool {
    static MOUNTED: OnceLock<bool> = OnceLock::new();
    *MOUNTED.get_or_init(|| std::path::Path::new("/proc/self/stat").exists())
}

/// `btime` + ticks. `btime` moves when the clock is set, so this does too.
fn start_unix_ms(ticks: u64) -> Option<i64> {
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if hz <= 0 {
        return None;
    }
    Some(btime()? * 1000 + (ticks as i64) * 1000 / hz as i64)
}

/// `/proc/stat` `btime`, seconds.
fn btime() -> Option<i64> {
    let text = std::fs::read_to_string("/proc/stat").ok()?;
    text.lines().find_map(|l| l.strip_prefix("btime ")).and_then(|v| v.trim().parse().ok())
}

pub(super) fn boot_id() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub(super) fn boot_unix_ms() -> Option<i64> {
    btime().map(|s| s * 1000)
}

pub(super) fn pid_ns() -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self/ns/pid").ok().map(|m| m.ino())
}
