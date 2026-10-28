//! Process identity per OS: is a pid alive, and is it still the process a
//! record names. The start token never changes when the wall clock is set:
//! Linux counts ticks since boot, macOS and Windows record the start once.
//!
//! | | Linux | macOS | Windows |
//! |---|---|---|---|
//! | token | `lnx:<stat field 22>` | `mac:<p_uniqueid>` | `win:<creation FILETIME>` |
//! | argv | `/proc/<pid>/cmdline` | `KERN_PROCARGS2` | `NtQueryInformationProcess(60)` |

use std::path::PathBuf;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as os;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as os;
#[cfg(windows)]
pub(crate) mod windows;
#[cfg(windows)]
use windows as os;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcInfo {
    /// The OS start token; equal tokens = the same process.
    pub start: String,
    /// Start time, unix ms. Linux: in the current wall-clock frame (it moves
    /// when the clock is set). `None` when unknown (macOS: another user).
    pub start_unix_ms: Option<i64>,
    /// `None` when it cannot be read (another user, hidepid).
    pub argv: Option<Vec<String>>,
    pub exe: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lookup {
    Alive(ProcInfo),
    /// No such process, or a zombie.
    Gone,
    /// It exists, but this user may not read it.
    Denied,
    /// This system cannot answer (no `/proc`, an untested OS).
    Unsupported,
}

/// Look up `pid`, with its argv and image path.
pub fn lookup(pid: u32) -> Lookup {
    lookup_with(pid, true)
}

/// [`lookup`] without argv and image path (`argv`, `exe` are `None`): the
/// identity checks that run on every poll need only the start token.
pub fn lookup_identity(pid: u32) -> Lookup {
    lookup_with(pid, false)
}

fn lookup_with(pid: u32, full: bool) -> Lookup {
    if pid == 0 || pid > i32::MAX as u32 {
        return Lookup::Gone;
    }
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    {
        os::lookup(pid, full)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = full;
        Lookup::Unsupported
    }
}

/// This process. Its `start` is empty when the OS cannot say, which no
/// lookup ever matches: such a holder reads "starting", never "running".
pub fn me() -> ProcInfo {
    match lookup(std::process::id()) {
        Lookup::Alive(p) => p,
        _ => ProcInfo { start: String::new(), start_unix_ms: None, argv: None, exe: std::env::current_exe().ok() },
    }
}

/// Linux `/proc/sys/kernel/random/boot_id`, macOS `kern.bootsessionuuid`;
/// `None` on Windows.
pub fn boot_id() -> Option<String> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        os::boot_id()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// When this computer started, unix ms.
pub fn boot_unix_ms() -> Option<i64> {
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    {
        os::boot_unix_ms()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        None
    }
}

/// Linux: the inode of `/proc/self/ns/pid`. A pid means nothing in another
/// namespace (a container).
pub fn pid_ns() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        os::pid_ns()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// `/proc/<pid>/stat`: the state letter and field 22 (start time in clock
/// ticks since boot). comm may hold spaces and `)`, so the fields start
/// after the LAST `)`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_stat(text: &str) -> Option<(char, u64)> {
    let (_, rest) = text.rsplit_once(')')?;
    let mut fields = rest.split_whitespace();
    let state = fields.next()?.chars().next()?;
    // Field 3 is the state; field 22 is 19 further on.
    let start = fields.nth(18)?.parse().ok()?;
    Some((state, start))
}

/// `KERN_PROCARGS2`: `argc` (i32), the exec path, NUL padding, then `argc`
/// NUL-terminated args (then the environment, not read).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn parse_procargs2(buf: &[u8]) -> Option<Vec<String>> {
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?);
    if argc <= 0 {
        return None;
    }
    let mut rest = &buf[4..];
    let path_end = rest.iter().position(|&b| b == 0)?;
    rest = &rest[path_end..];
    let first = rest.iter().position(|&b| b != 0)?;
    rest = &rest[first..];
    let mut argv = Vec::with_capacity(argc as usize);
    for arg in rest.split(|&b| b == 0).take(argc as usize) {
        argv.push(String::from_utf8_lossy(arg).into_owned());
    }
    (argv.len() == argc as usize).then_some(argv)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn me_round_trips() {
        let me = me();
        assert!(!me.start.is_empty());
        match lookup(std::process::id()) {
            Lookup::Alive(p) => {
                assert_eq!(p.start, me.start);
                assert!(p.argv.as_ref().is_some_and(|a| !a.is_empty()), "{p:?}");
                assert!(p.exe.is_some());
                let start = p.start_unix_ms.expect("own start time");
                let now = crate::local_bot::now_ms();
                assert!(start <= now + 2_000 && start > now - 24 * 3_600_000, "{start} vs {now}");
            }
            other => panic!("{other:?}"),
        }
        match lookup_identity(std::process::id()) {
            Lookup::Alive(p) => assert_eq!((p.start, p.argv, p.exe), (me.start.clone(), None, None)),
            other => panic!("{other:?}"),
        }
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            assert!(boot_id().is_some_and(|b| !b.is_empty()));
        }
        assert!(boot_unix_ms().is_some_and(|b| b < crate::local_bot::now_ms()));
        assert_eq!(pid_ns().is_some(), cfg!(target_os = "linux"));
    }

    #[test]
    fn dead_child_is_gone() {
        let mut child = idle_child();
        let pid = child.id();
        assert!(matches!(lookup(pid), Lookup::Alive(_)));
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(lookup(pid), Lookup::Gone);
        assert_eq!(lookup(0), Lookup::Gone);
        assert_eq!(lookup(u32::MAX), Lookup::Gone);
    }

    #[test]
    fn a_zombie_is_gone() {
        let mut child = idle_child();
        let pid = child.id();
        child.kill().unwrap();
        // Not reaped yet: a zombie on Unix.
        let t0 = std::time::Instant::now();
        while lookup(pid) != Lookup::Gone && t0.elapsed() < std::time::Duration::from_secs(5) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(lookup(pid), Lookup::Gone);
        child.wait().unwrap();
    }

    #[test]
    fn start_token_changes_detect_reuse() {
        let mut child = idle_child();
        let Lookup::Alive(p) = lookup(child.id()) else { panic!("alive") };
        assert_ne!(p.start, me().start, "two processes, two tokens");
        // A record whose token was edited no longer names this process.
        let edited = format!("{}9", p.start);
        let Lookup::Alive(again) = lookup(child.id()) else { panic!("alive") };
        assert_eq!(again.start, p.start, "stable");
        assert_ne!(again.start, edited);
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn argv_reads_own_child() {
        #[cfg(unix)]
        let (mut child, want) = {
            let args = ["-c", "read x; :", "tradectl-test", "a b", "c"];
            let c = Command::new("/bin/sh").args(args).stdin(std::process::Stdio::piped()).spawn().unwrap();
            let mut want = vec!["/bin/sh".to_string()];
            want.extend(args.iter().map(|s| s.to_string()));
            (c, want)
        };
        #[cfg(windows)]
        let (mut child, want) = {
            let c = Command::new("cmd").args(["/C", "pause"]).stdin(std::process::Stdio::piped()).spawn().unwrap();
            (c, vec!["cmd".to_string(), "/C".to_string(), "pause".to_string()])
        };
        let Lookup::Alive(p) = lookup(child.id()) else { panic!("alive") };
        let argv = p.argv.expect("argv of our own child");
        if cfg!(windows) {
            assert_eq!(&argv[1..], &want[1..]);
        } else {
            assert_eq!(argv, want);
        }
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn comm_with_parens_parses() {
        let mut text = String::from("4242 (my (odd) prog) S 1 4242 4242 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 ");
        text.push_str("987654 1000 200 18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0 0 0\n");
        assert_eq!(parse_stat(&text), Some(('S', 987654)));
        let zombie = "7 (a) b) Z 1 7 7 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 55 0 0";
        assert_eq!(parse_stat(zombie), Some(('Z', 55)));
        assert_eq!(parse_stat("garbage"), None);
        assert_eq!(parse_stat("1 (x) S 1 2"), None, "too short");
    }

    #[test]
    fn procargs2_parses() {
        let mut buf = 3i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/usr/local/bin/tradectl\0\0\0\0tradectl\0run\0paper\0HOME=/x\0");
        assert_eq!(parse_procargs2(&buf), Some(vec!["tradectl".into(), "run".into(), "paper".into()]));
        assert_eq!(parse_procargs2(&0i32.to_ne_bytes()), None);
        assert_eq!(parse_procargs2(b"\x05\0\0\0/x\0a\0"), None, "argc larger than what is there");
    }

    /// A child that waits on stdin; no grandchildren to leak.
    pub(crate) fn idle_child() -> std::process::Child {
        #[cfg(unix)]
        let mut cmd = Command::new("/bin/sh");
        #[cfg(unix)]
        cmd.args(["-c", "read x; :"]);
        #[cfg(windows)]
        let mut cmd = Command::new("cmd");
        #[cfg(windows)]
        cmd.args(["/C", "pause"]);
        cmd.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).spawn().unwrap()
    }
}
