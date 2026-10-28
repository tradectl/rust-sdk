//! `bot_running`: the one answer to "is this bot running" (section 4.4).
//!
//! Running = the lock is held AND the holder record is verified, checked in
//! this order: the record parses and names this bot; its boot id is this
//! boot's; its PID namespace is ours (Linux); its pid is alive with the same
//! start token. With the lock free, a v0.2.7-or-older process is looked for
//! through `bot.pid` (section 5). Unknown never leads to a signal or a kill.

use std::time::{Duration, Instant};

use super::holder::{self, Holder, Mode};
use super::legacy::{self, LegacyProc};
use super::lock::{self, LockProbe};
use super::proc::{self, Lookup};
use super::signal::Target;
pub use super::status::Last;
use super::Home;

/// How long `bot_running` retries a held lock whose record is not verified
/// yet (a holder in its first milliseconds, Windows' late release).
const STARTING_RETRY: Duration = Duration::from_secs(3);
const RETRY_EVERY: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Liveness {
    /// Lock held, record verified, a bot mode.
    Running(Holder),
    /// Lock held by a Delete (mode `delete`): never signalled.
    Deleting(Holder),
    /// Lock held, record missing, unreadable or not verified (yet).
    Starting { why: String },
    /// Lock free, a v0.2.7-or-older process runs this bot.
    Legacy(LegacyProc),
    /// Lock free, nothing alive.
    NotRunning(Last),
    /// Cannot decide.
    Unknown(String),
}

impl Liveness {
    /// Counts as alive: Start refuses, Delete waits.
    pub fn alive(&self) -> bool {
        !matches!(self, Liveness::NotRunning(_))
    }

    /// The process a Unix signal or `--force` may go to: a verified holder,
    /// or a verified legacy bot. Never for `Deleting`, `Starting`, `Unknown`.
    pub fn target(&self) -> Option<Target> {
        match self {
            Liveness::Running(h) => Some(h.target()),
            Liveness::Legacy(l) => l.target(),
            _ => None,
        }
    }

    /// The pid it is known by, when there is one.
    pub fn pid(&self) -> Option<u32> {
        match self {
            Liveness::Running(h) | Liveness::Deleting(h) => Some(h.pid),
            Liveness::Legacy(l) => Some(l.pid),
            _ => None,
        }
    }
}

/// [`bot_running_now`], retrying `Starting` for up to 3 s. For decisions
/// (Start, Stop, Delete).
pub fn bot_running(home: &Home, name: &str) -> Liveness {
    decide(&Real { home, name }, name, STARTING_RETRY)
}

/// One sample, for UI polls.
pub fn bot_running_now(home: &Home, name: &str) -> Liveness {
    decide(&Real { home, name }, name, Duration::ZERO)
}

/// What `decide` reads; faked in tests.
pub(crate) trait World {
    fn probe(&self) -> LockProbe;
    fn holder(&self) -> Result<Option<Holder>, String>;
    fn lookup(&self, pid: u32) -> Lookup;
    fn boot_id(&self) -> Option<String>;
    fn pid_ns(&self) -> Option<u64>;
    fn legacy(&self) -> Option<LegacyProc>;
    fn last(&self) -> Last;
    fn lock_path(&self) -> String;
}

struct Real<'a> {
    home: &'a Home,
    name: &'a str,
}

impl World for Real<'_> {
    fn probe(&self) -> LockProbe {
        lock::probe(self.home, self.name)
    }
    fn holder(&self) -> Result<Option<Holder>, String> {
        holder::read(self.home, self.name)
    }
    fn lookup(&self, pid: u32) -> Lookup {
        proc::lookup_identity(pid)
    }
    fn boot_id(&self) -> Option<String> {
        proc::boot_id()
    }
    fn pid_ns(&self) -> Option<u64> {
        proc::pid_ns()
    }
    fn legacy(&self) -> Option<LegacyProc> {
        legacy::check(self.home, self.name)
    }
    fn last(&self) -> Last {
        super::status::last(self.home, self.name)
    }
    fn lock_path(&self) -> String {
        self.home.lock_path(self.name).display().to_string()
    }
}

enum Record {
    Verified(Holder),
    /// Not verified yet; may change within the retry.
    Retry(String),
    /// Decided at once.
    Unknown(String),
}

/// Checks 1-4 of section 4.4, in order (M8).
fn verify(w: &dyn World, name: &str) -> Record {
    let rec = match w.holder() {
        Ok(Some(r)) => r,
        Ok(None) => return Record::Retry("the lock is held but there is no holder record yet".into()),
        Err(e) => return Record::Retry(format!("the holder record cannot be read: {e}")),
    };
    if rec.name != name {
        return Record::Retry(format!("the holder record names another bot ({})", rec.name));
    }
    if let (Some(a), Some(b)) = (&rec.boot, w.boot_id()) {
        if *a != b {
            return Record::Retry("the holder record is from an earlier boot".into());
        }
    }
    if let (Some(a), Some(b)) = (rec.pid_ns, w.pid_ns()) {
        if a != b {
            return Record::Unknown("runs in another PID namespace (a container)".into());
        }
    }
    match w.lookup(rec.pid) {
        Lookup::Alive(p) if p.start == rec.start => Record::Verified(rec),
        Lookup::Alive(_) | Lookup::Gone => {
            Record::Retry(format!("the holder record names PID {}, which is not running", rec.pid))
        }
        Lookup::Denied => Record::Unknown(format!("cannot read PID {}: permission denied", rec.pid)),
        Lookup::Unsupported => Record::Unknown("cannot read processes on this system".into()),
    }
}

fn held(rec: Holder) -> Liveness {
    if rec.mode == Mode::Delete {
        Liveness::Deleting(rec)
    } else {
        Liveness::Running(rec)
    }
}

fn free(w: &dyn World) -> Liveness {
    match w.legacy() {
        Some(l) => Liveness::Legacy(l),
        None => Liveness::NotRunning(w.last()),
    }
}

/// One sample: `Err(why)` = held, not verified yet (retry).
fn sample(w: &dyn World, name: &str) -> Result<Liveness, String> {
    match w.probe() {
        // The record is history then.
        LockProbe::Absent | LockProbe::Free => Ok(free(w)),
        LockProbe::Held => match verify(w, name) {
            Record::Verified(rec) => Ok(held(rec)),
            Record::Retry(why) => Err(why),
            Record::Unknown(why) => Ok(Liveness::Unknown(why)),
        },
        LockProbe::Error(e) => match verify(w, name) {
            Record::Verified(rec) => Ok(held(rec)),
            Record::Retry(_) | Record::Unknown(_) => Ok(match w.legacy() {
                Some(l) => Liveness::Legacy(l),
                None => Liveness::Unknown(format!("cannot lock {}: {e}", w.lock_path())),
            }),
        },
    }
}

pub(crate) fn decide(w: &dyn World, name: &str, retry_for: Duration) -> Liveness {
    let t0 = Instant::now();
    loop {
        match sample(w, name) {
            Ok(l) => return l,
            Err(why) if t0.elapsed() >= retry_for => return Liveness::Starting { why },
            Err(_) => std::thread::sleep(RETRY_EVERY),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_bot::proc::ProcInfo;
    use crate::local_bot::status::{CloseOut, Step};
    use crate::local_bot::testutil::{child_env, idle_until_stopped, respawn, wait_for, ChildGuard, TempHome};
    use std::cell::Cell;

    #[derive(Clone, Copy)]
    enum P {
        Absent,
        Free,
        Held,
        Error,
    }

    struct Fake {
        probe: P,
        holder: Result<Option<Holder>, String>,
        alive: Option<(u32, &'static str)>,
        lookup_other: Lookup,
        boot: Option<String>,
        ns: Option<u64>,
        legacy: Option<LegacyProc>,
        samples: Cell<u32>,
    }

    impl Default for Fake {
        fn default() -> Fake {
            Fake {
                probe: P::Held,
                holder: Ok(Some(rec(Mode::DaemonChild))),
                alive: Some((42, "lnx:7")),
                lookup_other: Lookup::Gone,
                boot: Some("boot-1".into()),
                ns: Some(9),
                legacy: None,
                samples: Cell::new(0),
            }
        }
    }

    impl World for Fake {
        fn probe(&self) -> LockProbe {
            self.samples.set(self.samples.get() + 1);
            match self.probe {
                P::Absent => LockProbe::Absent,
                P::Free => LockProbe::Free,
                P::Held => LockProbe::Held,
                P::Error => LockProbe::Error(std::io::Error::other("odd fs")),
            }
        }
        fn holder(&self) -> Result<Option<Holder>, String> {
            self.holder.clone()
        }
        fn lookup(&self, pid: u32) -> Lookup {
            match self.alive {
                Some((p, start)) if p == pid => {
                    Lookup::Alive(ProcInfo { start: start.into(), start_unix_ms: Some(1), argv: None, exe: None })
                }
                _ => self.lookup_other.clone(),
            }
        }
        fn boot_id(&self) -> Option<String> {
            self.boot.clone()
        }
        fn pid_ns(&self) -> Option<u64> {
            self.ns
        }
        fn legacy(&self) -> Option<LegacyProc> {
            self.legacy.clone()
        }
        fn last(&self) -> Last {
            Last::Stopped { close_out: Some(CloseOut::Done { closed: 1 }) }
        }
        fn lock_path(&self) -> String {
            "/h/locks/paper.lock".into()
        }
    }

    fn rec(mode: Mode) -> Holder {
        Holder {
            v: 1,
            name: "paper".into(),
            bot_id: Some("bx".into()),
            run_id: "r".into(),
            pid: 42,
            start: "lnx:7".into(),
            boot: Some("boot-1".into()),
            pid_ns: Some(9),
            exe: None,
            version: "0.2.8".into(),
            mode,
            since_ms: 0,
        }
    }

    fn old_bot(verified: bool) -> LegacyProc {
        LegacyProc { pid: 77, verified, start: Some("lnx:1".into()), why: String::new() }
    }

    fn now(f: &Fake) -> Liveness {
        decide(f, "paper", Duration::ZERO)
    }

    fn starting(l: &Liveness) -> bool {
        matches!(l, Liveness::Starting { .. })
    }

    #[test]
    fn matrix() {
        // Held, verified, a bot mode.
        for m in [Mode::Foreground, Mode::DaemonChild, Mode::Service, Mode::Other] {
            assert_eq!(now(&Fake { holder: Ok(Some(rec(m))), ..Fake::default() }), Liveness::Running(rec(m)));
        }
        // Held, verified, a Delete.
        assert_eq!(
            now(&Fake { holder: Ok(Some(rec(Mode::Delete))), ..Fake::default() }),
            Liveness::Deleting(rec(Mode::Delete))
        );
        // Held: missing, unreadable, another bot, dead, another process, an old boot.
        assert!(starting(&now(&Fake { holder: Ok(None), ..Fake::default() })));
        assert!(starting(&now(&Fake { holder: Err("bad json".into()), ..Fake::default() })));
        let mut other = rec(Mode::DaemonChild);
        other.name = "other".into();
        assert!(starting(&now(&Fake { holder: Ok(Some(other)), ..Fake::default() })));
        assert!(starting(&now(&Fake { alive: None, ..Fake::default() })), "dead");
        assert!(starting(&now(&Fake { alive: Some((42, "lnx:8")), ..Fake::default() })), "reused pid");
        assert!(starting(&now(&Fake { boot: Some("boot-2".into()), ..Fake::default() })));
        // A side without a boot id or namespace skips that check.
        assert!(matches!(now(&Fake { boot: None, ns: None, ..Fake::default() }), Liveness::Running(_)));
        // Held, another PID namespace: unknown.
        assert!(
            matches!(now(&Fake { ns: Some(10), ..Fake::default() }), Liveness::Unknown(w) if w.contains("namespace"))
        );
        // Held, lookup denied or unsupported: unknown.
        assert!(matches!(
            now(&Fake { alive: None, lookup_other: Lookup::Denied, ..Fake::default() }),
            Liveness::Unknown(_)
        ));
        assert!(matches!(
            now(&Fake { alive: None, lookup_other: Lookup::Unsupported, ..Fake::default() }),
            Liveness::Unknown(_)
        ));
        // Free or absent: the record is ignored; legacy or not running.
        for p in [P::Free, P::Absent] {
            let l = now(&Fake { probe: p, ..Fake::default() });
            assert_eq!(l, Liveness::NotRunning(Last::Stopped { close_out: Some(CloseOut::Done { closed: 1 }) }));
            let l = now(&Fake { probe: p, legacy: Some(old_bot(true)), ..Fake::default() });
            assert_eq!(l, Liveness::Legacy(old_bot(true)));
            let l = now(&Fake { probe: p, legacy: Some(old_bot(false)), ..Fake::default() });
            assert_eq!(l, Liveness::Legacy(old_bot(false)));
        }
        // Probe error: verified still runs; else legacy, else unknown.
        assert!(matches!(now(&Fake { probe: P::Error, ..Fake::default() }), Liveness::Running(_)));
        let l = now(&Fake { probe: P::Error, holder: Ok(None), legacy: Some(old_bot(true)), ..Fake::default() });
        assert_eq!(l, Liveness::Legacy(old_bot(true)));
        let l = now(&Fake { probe: P::Error, holder: Ok(None), ..Fake::default() });
        assert!(matches!(l, Liveness::Unknown(ref w) if w.starts_with("cannot lock /h/locks/paper.lock")), "{l:?}");
    }

    #[test]
    fn what_each_result_allows() {
        let run = Liveness::Running(rec(Mode::DaemonChild));
        assert!(run.alive());
        assert_eq!(run.target(), Some(Target { pid: 42, start: "lnx:7".into() }));
        let del = Liveness::Deleting(rec(Mode::Delete));
        assert!(del.alive() && del.target().is_none());
        for l in [Liveness::Starting { why: "x".into() }, Liveness::Unknown("x".into())] {
            assert!(l.alive() && l.target().is_none(), "{l:?}");
        }
        assert!(Liveness::Legacy(old_bot(true)).target().is_some());
        assert!(Liveness::Legacy(old_bot(false)).alive());
        assert!(Liveness::Legacy(old_bot(false)).target().is_none());
        let stopped = Liveness::NotRunning(Last::Never);
        assert!(!stopped.alive() && stopped.target().is_none());
    }

    #[test]
    fn starting_is_retried_then_reported() {
        let f = Fake { holder: Ok(None), ..Fake::default() };
        let t0 = Instant::now();
        assert!(starting(&decide(&f, "paper", Duration::from_millis(350))));
        assert!(t0.elapsed() >= Duration::from_millis(350));
        assert!(f.samples.get() >= 3, "sampled again: {}", f.samples.get());
    }

    #[test]
    fn pid_ns_mismatch_is_unknown_without_retry() {
        let f = Fake { ns: Some(10), ..Fake::default() };
        let t0 = Instant::now();
        assert!(matches!(decide(&f, "paper", STARTING_RETRY), Liveness::Unknown(_)));
        assert!(t0.elapsed() < Duration::from_millis(100));
        assert_eq!(f.samples.get(), 1);
    }

    const CHILD: &str = "local_bot::running::tests::child_locks_without_a_record";

    /// Child: hold the lock, write no record.
    #[test]
    #[ignore]
    fn child_locks_without_a_record() {
        let Some((home, name)) = child_env() else { return };
        let lock = crate::local_bot::lock::BotLock::acquire(&home, &name, Duration::from_secs(5)).unwrap();
        std::fs::write(home.root().join("child.ready"), "").unwrap();
        idle_until_stopped(&home, "child");
        drop(lock);
    }

    #[test]
    fn holder_last_is_never_read_as_current() {
        let t = TempHome::new();
        t.bot("paper");
        let child = ChildGuard(respawn(CHILD, &t.home, "paper").spawn().unwrap());
        assert!(wait_for(&t.home.root().join("child.ready"), Duration::from_secs(20)));
        let Lookup::Alive(p) = proc::lookup(child.pid()) else { panic!("alive") };
        let mut rec = Holder::for_this_process("paper", None, Mode::DaemonChild, "0.2.8");
        rec.pid = child.pid();
        rec.start = p.start;
        // A verified record, but only as history.
        let text = serde_json::to_string(&rec).unwrap();
        std::fs::write(t.home.holder_last_path("paper"), &text).unwrap();
        assert!(starting(&bot_running_now(&t.home, "paper")));
        // The same record as the current one: running.
        std::fs::write(t.home.holder_path("paper"), &text).unwrap();
        assert_eq!(bot_running_now(&t.home, "paper"), Liveness::Running(rec));
        drop(child);
        // Lock free: the record is history, the last status says how it ended.
        let l = bot_running(&t.home, "paper");
        assert_eq!(l, Liveness::NotRunning(Last::Never));
    }

    #[test]
    fn a_real_holder_is_running_and_its_own_status_is_current() {
        let t = TempHome::new();
        t.bot("paper");
        let child = ChildGuard(respawn(CHILD, &t.home, "paper").spawn().unwrap());
        assert!(wait_for(&t.home.root().join("child.ready"), Duration::from_secs(20)));
        let Lookup::Alive(p) = proc::lookup(child.pid()) else { panic!("alive") };
        let mut rec = Holder::for_this_process("paper", None, Mode::Foreground, "0.2.8");
        rec.pid = child.pid();
        rec.start = p.start;
        std::fs::write(t.home.holder_path("paper"), serde_json::to_string(&rec).unwrap()).unwrap();
        let l = bot_running(&t.home, "paper");
        assert_eq!(l.pid(), Some(child.pid()));
        assert!(l.target().is_some());
        // status.json of another run is not this holder's state yet.
        let h = crate::local_bot::status::StatusWriter::spawn(
            &t.home,
            "paper",
            "other",
            1,
            "0.2.8",
            Mode::Foreground,
            None,
        );
        h.terminal_blocking(crate::local_bot::status::Terminal::Stopped { close_out: None, alert: None });
        assert!(crate::local_bot::status::current(&t.home, &rec).is_none());
        let h = crate::local_bot::status::StatusWriter::spawn(
            &t.home,
            "paper",
            &rec.run_id,
            1,
            "0.2.8",
            Mode::Foreground,
            None,
        );
        h.step(Step::Stopping, None);
        h.terminal_blocking(crate::local_bot::status::Terminal::Exited {
            code: 1,
            reason: "x".into(),
            reason_key: None,
        });
        assert!(crate::local_bot::status::current(&t.home, &rec).is_some());
    }
}
