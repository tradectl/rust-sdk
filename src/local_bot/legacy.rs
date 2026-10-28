//! Bots started by v0.2.7 and older: no lock, only `bot/<bot>/bot.pid`.
//!
//! Checked only while the lock is free. A live process whose argv runs this
//! bot is always alive (H1): the start-time test can only lower it to
//! unverified (never signalled), never to "not running". A false "running"
//! costs one click; a false "stopped" starts two bots on one account.

use std::io::Read;
use std::path::Path;

use super::proc::{boot_unix_ms, lookup, Lookup};
use super::signal::Target;
use super::Home;

/// Slack for coarse mtimes (FAT, SMB) and Linux's 1 s `btime` grain.
const SLACK_MS: i64 = 2_000;

/// A process `bot.pid` names that looks like this bot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyProc {
    pub pid: u32,
    /// argv matches and the start fits `bot.pid`'s write time: may be
    /// signalled (re-verified right before).
    pub verified: bool,
    /// The OS start token, when the process could be read.
    pub start: Option<String>,
    /// Why it is not verified; empty when it is.
    pub why: String,
}

impl LegacyProc {
    /// The process a signal may go to; `None` unless verified.
    pub fn target(&self) -> Option<Target> {
        let start = self.start.clone().filter(|_| self.verified)?;
        Some(Target { pid: self.pid, start })
    }

    /// The refusal text for an unverified one (exit 78 `maybe-running`).
    pub fn unconfirmed(&self, home: &Home, name: &str) -> String {
        format!(
            "bot.pid names PID {}, which looks like this bot but I cannot confirm it. If no copy of this bot runs, delete {}.",
            self.pid,
            home.pid_path(name).display()
        )
    }
}

/// Read `bot/<name>/bot.pid` and decide (section 5.1).
pub fn check(home: &Home, name: &str) -> Option<LegacyProc> {
    let (pid, mtime) = read_pid_file(&home.pid_path(name))?;
    let config = home.config_path(name);
    let seen = match lookup(pid) {
        Lookup::Unsupported if !exists(pid) => Lookup::Gone,
        l => l,
    };
    decide(pid, std::process::id(), &seen, |argv| argv_runs_bot(argv, name, &config), mtime, boot_unix_ms())
}

/// The pid in a `bot.pid` and the file's write time (one open file, so both
/// are the same file's).
fn read_pid_file(path: &Path) -> Option<(u32, Option<i64>)> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut text = String::new();
    f.read_to_string(&mut text).ok()?;
    let pid: u64 = text.trim().parse().ok()?;
    if !(2..=i32::MAX as u64).contains(&pid) {
        return None;
    }
    let mtime = f.metadata().and_then(|m| m.modified()).ok().and_then(super::unix_ms);
    Some((pid as u32, mtime))
}

/// `kill(pid, 0)`: a process with this pid exists (any user). For systems
/// where `lookup` cannot tell.
fn exists(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let ok = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
        ok || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// Section 5.1 as a pure function: `seen` is what `lookup(pid)` said,
/// `mtime_ms` when `bot.pid` was written, `boot_ms` when this computer
/// started.
pub(crate) fn decide(
    pid: u32,
    own: u32,
    seen: &Lookup,
    runs_this_bot: impl Fn(&[String]) -> bool,
    mtime_ms: Option<i64>,
    boot_ms: Option<i64>,
) -> Option<LegacyProc> {
    let unverified =
        |start: Option<String>, why: &str| Some(LegacyProc { pid, verified: false, start, why: why.to_string() });
    if pid == own || !(2..=i32::MAX as u32).contains(&pid) {
        return None;
    }
    let p = match seen {
        Lookup::Gone => return None,
        Lookup::Denied => return unverified(None, "cannot read the process (another user?)"),
        Lookup::Unsupported => return unverified(None, "cannot read processes on this system"),
        Lookup::Alive(p) => p,
    };
    // Started before (or as) the file was written?
    let fits = match (p.start_unix_ms, mtime_ms) {
        (Some(s), Some(m)) => Some(s <= m + SLACK_MS),
        _ => None,
    };
    match &p.argv {
        Some(argv) if runs_this_bot(argv) => match fits {
            Some(true) => Some(LegacyProc { pid, verified: true, start: Some(p.start.clone()), why: String::new() }),
            // A clock step after the write can do this to a real old bot.
            Some(false) => unverified(Some(p.start.clone()), "it seems to have started after bot.pid was written"),
            None => unverified(Some(p.start.clone()), "its start time is unknown"),
        },
        // Another program, or a sibling bot (F22).
        Some(_) => None,
        // argv unreadable: almost always another user's process.
        None => match fits {
            // The pid was reused after the file was written.
            Some(false) => None,
            Some(true) => unverified(Some(p.start.clone()), "cannot read its command line"),
            None => {
                // No process outlives a reboot.
                if matches!((mtime_ms, boot_ms), (Some(m), Some(b)) if m + SLACK_MS < b) {
                    return None;
                }
                unverified(Some(p.start.clone()), "cannot read its command line")
            }
        },
    }
}

/// argv runs bot `name` whose config is `config` (section 5.2). The image
/// path is not used: after an update the old image can be renamed, but the
/// command line stays as it was passed.
pub fn argv_runs_bot(argv: &[String], name: &str, config: &Path) -> bool {
    let Some(first) = argv.first() else { return false };
    let file = first.rsplit(['/', '\\']).next().unwrap_or(first).to_ascii_lowercase();
    if !file.strip_suffix(".exe").unwrap_or(&file).starts_with("tradectl") {
        return false;
    }
    let args = &argv[1..];
    // Daemon form: `run --daemon-child <config>`.
    if args.windows(3).any(|w| w[0] == "run" && w[1] == "--daemon-child" && same_config(&w[2], name, config)) {
        return true;
    }
    match args.iter().position(|a| a == "run") {
        Some(i) => foreground_runs_bot(&args[i + 1..], name, config),
        None => false,
    }
}

/// Foreground form: after `run`, the first positional is the bot (or its
/// config), and no flag makes it something other than a bot run.
fn foreground_runs_bot(rest: &[String], name: &str, config: &Path) -> bool {
    const TAKES_VALUE: [&str; 4] = ["-c", "--config", "--replay", "--control-port"];
    const NOT_A_BOT_RUN: [&str; 6] = ["--daemon", "--dry-run", "--replay", "--print-pair", "--rotate-pair", "--manual"];
    let mut positional = None;
    let mut i = 0;
    while i < rest.len() {
        let a = rest[i].as_str();
        if a.starts_with('-') {
            let flag = a.split('=').next().unwrap_or(a);
            if NOT_A_BOT_RUN.contains(&flag) {
                return false;
            }
            if TAKES_VALUE.contains(&a) {
                i += 1;
            }
        } else if positional.is_none() {
            positional = Some(a);
        }
        i += 1;
    }
    positional.is_some_and(|p| p == name || same_config(p, name, config))
}

/// `p` is this bot's `config.json`. A relative path (the process's cwd is
/// not known) counts when it ends in `bot/<name>/config.json`.
fn same_config(p: &str, name: &str, config: &Path) -> bool {
    let p = Path::new(p);
    if p == config {
        return true;
    }
    if p.is_absolute() {
        return matches!((std::fs::canonicalize(p), std::fs::canonicalize(config)), (Ok(a), Ok(b)) if a == b);
    }
    let tail: Vec<_> = p.components().rev().take(3).map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    tail == ["config.json", name, "bot"]
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::local_bot::proc::ProcInfo;
    use crate::local_bot::testutil::TempHome;

    fn alive(argv: Option<&[&str]>, start_ms: Option<i64>) -> Lookup {
        Lookup::Alive(ProcInfo {
            start: "tok".into(),
            start_unix_ms: start_ms,
            argv: argv.map(|a| a.iter().map(|s| s.to_string()).collect()),
            exe: None,
        })
    }

    #[test]
    fn matrix() {
        let bot = |a: &[String]| a.first().is_some_and(|f| f == "tradectl");
        let me = 1000;
        let m = Some(10_000_000);
        let boot = Some(5_000_000);
        let run = Some(&["tradectl", "run", "x"][..]);
        let other = Some(&["sshd"][..]);
        let d = |pid, seen: &Lookup, mtime| decide(pid, me, seen, bot, mtime, boot);
        let verified = |r: Option<LegacyProc>| r.map(|l| l.verified);
        // Missing pid range, own pid.
        assert_eq!(d(0, &alive(run, Some(1)), m), None);
        assert_eq!(d(1, &alive(run, Some(1)), m), None);
        assert_eq!(d(i32::MAX as u32 + 1, &alive(run, Some(1)), m), None);
        assert_eq!(d(me, &alive(run, Some(1)), m), None, "own pid never matches");
        // Gone.
        assert_eq!(d(7, &Lookup::Gone, m), None);
        // Alive, matches, start <= mtime + 2 s: verified.
        assert_eq!(verified(d(7, &alive(run, Some(9_000_000)), m)), Some(true));
        assert_eq!(verified(d(7, &alive(run, Some(10_001_999)), m)), Some(true), "inside the slack");
        // Alive, matches, start later or unknown: unverified, never none.
        assert_eq!(verified(d(7, &alive(run, Some(10_002_001)), m)), Some(false));
        assert_eq!(verified(d(7, &alive(run, None), m)), Some(false));
        assert_eq!(verified(d(7, &alive(run, Some(1)), None)), Some(false), "mtime unknown");
        // A file older than the boot naming a matching live process: still alive (H1).
        assert_eq!(verified(d(7, &alive(run, Some(6_000_000)), Some(1_000))), Some(false));
        // Alive, another program: none.
        assert_eq!(d(7, &alive(other, Some(1)), m), None);
        // Alive, argv unreadable: the time test decides.
        assert_eq!(d(7, &alive(None, Some(10_002_001)), m), None, "pid reused after the write");
        assert_eq!(verified(d(7, &alive(None, Some(9_000_000)), m)), Some(false));
        assert_eq!(verified(d(7, &alive(None, None), m)), Some(false), "start unknown");
        assert_eq!(d(7, &alive(None, None), Some(1_000)), None, "start unknown, file before the boot");
        // Denied, unsupported: alive, unverified.
        assert_eq!(verified(d(7, &Lookup::Denied, m)), Some(false));
        assert_eq!(verified(d(7, &Lookup::Unsupported, m)), Some(false));
        // Only a verified one has a target.
        assert!(d(7, &alive(run, Some(9_000_000)), m).unwrap().target().is_some());
        assert!(d(7, &alive(run, None), m).unwrap().target().is_none());
        assert!(d(7, &Lookup::Denied, m).unwrap().target().is_none());
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn command_line_match() {
        let t = TempHome::new();
        let dir = t.bot("paper");
        let cfg = dir.join("config.json");
        let cfg_s = cfg.to_string_lossy().into_owned();
        let other = t.bot("other").join("config.json").to_string_lossy().into_owned();
        let yes = |a: &[&str]| assert!(argv_runs_bot(&s(a), "paper", &cfg), "{a:?}");
        let no = |a: &[&str]| assert!(!argv_runs_bot(&s(a), "paper", &cfg), "{a:?}");
        yes(&["/usr/local/bin/tradectl", "run", "--daemon-child", &cfg_s]);
        yes(&["/usr/local/bin/tradectl", "run", "--daemon-child", &cfg_s, "--separate-copy"]);
        yes(&["C:\\Users\\x\\.tradectl\\bin\\TRADECTL.EXE", "run", "--daemon-child", &cfg_s]);
        yes(&["tradectl", "run", "paper"]);
        yes(&["tradectl", "run", "paper", "--daemon-child"]);
        yes(&["tradectl", "run", "-c", "paper.json", "paper"]);
        yes(&["tradectl", "run", "--config", "x.json", "paper", "--force"]);
        yes(&["tradectl", "run", "--config=x.json", "paper"]);
        yes(&["tradectl", "run", &cfg_s]);
        yes(&["tradectl", "run", ".tradectl/bot/paper/config.json"]);
        yes(&["tradectl-0.2.7", "run", "paper"]);
        no(&["tradectl", "run", "--daemon-child", &other]);
        no(&["tradectl", "run", "other"]);
        no(&["tradectl", "run", &other]);
        no(&["tradectl", "run", "-c", "paper", "other"]);
        no(&["tradectl", "run", "paper", "--daemon"]);
        no(&["tradectl", "run", "paper", "--dry-run"]);
        no(&["tradectl", "run", "paper", "--replay", "x.bin"]);
        no(&["tradectl", "run", "paper", "--replay=x.bin"]);
        no(&["tradectl", "run", "paper", "--print-pair"]);
        no(&["tradectl", "run", "paper", "--rotate-pair"]);
        no(&["tradectl", "run", "--manual", "--daemon-child"]);
        no(&["tradectl", "stop", "paper"]);
        no(&["tradectl", "logs", "paper"]);
        no(&["/usr/bin/python3", "run", "paper"]);
        no(&["sh", "-c", "tradectl run paper"]);
        no(&[]);
    }

    /// A fake v0.2.7 bot: `/bin/sh` whose argv reads `tradectl ... run
    /// --daemon-child <cfg>` and that waits on stdin (no grandchild).
    #[cfg(unix)]
    pub(crate) fn fake_old_bot(cfg: &Path) -> crate::local_bot::testutil::ChildGuard {
        use std::os::unix::process::CommandExt;
        let child = std::process::Command::new("/bin/sh")
            .arg0("tradectl")
            .args(["-c", "read x; :", "run", "--daemon-child"])
            .arg(cfg)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        crate::local_bot::testutil::ChildGuard(child)
    }

    #[cfg(unix)]
    #[test]
    fn a_live_fake_old_bot_is_found() {
        let t = TempHome::new();
        let dir = t.bot("paper");
        let bot = fake_old_bot(&dir.join("config.json"));
        std::fs::write(t.home.pid_path("paper"), bot.pid().to_string()).unwrap();
        let l = check(&t.home, "paper").expect("found");
        assert_eq!(l.pid, bot.pid());
        assert!(l.verified, "{l:?}");
        assert!(l.target().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn a_sibling_bots_pid_is_not_this_bot() {
        let t = TempHome::new();
        t.bot("a");
        let b = t.bot("b");
        let bot_b = fake_old_bot(&b.join("config.json"));
        std::fs::write(t.home.pid_path("a"), bot_b.pid().to_string()).unwrap();
        assert_eq!(check(&t.home, "a"), None);
        std::fs::write(t.home.pid_path("b"), bot_b.pid().to_string()).unwrap();
        assert!(check(&t.home, "b").is_some_and(|l| l.verified));
    }

    #[cfg(unix)]
    #[test]
    fn a_clock_step_keeps_an_old_bot_alive() {
        let t = TempHome::new();
        let dir = t.bot("paper");
        let bot = fake_old_bot(&dir.join("config.json"));
        std::fs::write(t.home.pid_path("paper"), bot.pid().to_string()).unwrap();
        // An hour in the past: the process now seems to start after its file.
        let f = std::fs::File::options().write(true).open(t.home.pid_path("paper")).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600)).unwrap();
        drop(f);
        let l = check(&t.home, "paper").expect("still alive");
        assert!(!l.verified);
        assert!(l.target().is_none(), "never signalled");
        // Start is refused.
        let live = crate::local_bot::Liveness::Legacy(l.clone());
        assert_eq!(
            crate::local_bot::exit::when_held(&live),
            Some((crate::local_bot::exit::REFUSED, Some(crate::local_bot::exit::reason::MAYBE_RUNNING)))
        );
        assert!(live.target().is_none());
        assert!(l.unconfirmed(&t.home, "paper").contains("cannot confirm"));
    }

    #[test]
    fn own_pid_never_matches() {
        let t = TempHome::new();
        t.bot("paper");
        std::fs::write(t.home.pid_path("paper"), std::process::id().to_string()).unwrap();
        assert_eq!(check(&t.home, "paper"), None);
    }

    #[test]
    fn junk_pid_files_are_none() {
        let t = TempHome::new();
        t.bot("paper");
        assert_eq!(check(&t.home, "paper"), None, "missing");
        for junk in ["", "abc", "0", "1", "-5", "99999999999"] {
            std::fs::write(t.home.pid_path("paper"), junk).unwrap();
            assert_eq!(check(&t.home, "paper"), None, "{junk:?}");
        }
    }

    #[test]
    fn a_dead_pid_is_none() {
        let mut c = crate::local_bot::proc::tests::idle_child();
        let pid = c.id();
        c.kill().unwrap();
        c.wait().unwrap();
        let t = TempHome::new();
        t.bot("paper");
        std::fs::write(t.home.pid_path("paper"), pid.to_string()).unwrap();
        assert_eq!(check(&t.home, "paper"), None);
    }
}
