//! Delete a stopped bot under its own lock (section 13.2).
//!
//! The folder is renamed out of `bot/` in one step (into `trash/`, or to
//! `<bot>.deleted[-N]` to keep its history), so it is either all there or
//! all gone. The rename works on Windows while the lock is held because the
//! lock file is not in the folder. A start that meets the Delete's holder
//! record refuses with `deleting`.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::holder::{Holder, Mode};
use super::lock::{BotLock, LockError};
use super::running::Liveness;
use super::Home;

const LOCK_WAIT: Duration = Duration::from_secs(5);
/// Windows: antivirus or an indexer may hold a file in the folder.
const RENAME_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub enum DeleteError {
    /// A process runs the bot (an older bot, found under the lock).
    Running(Box<Liveness>),
    /// The lock is held: the bot started again. Stop it first.
    Locked,
    /// A program has a file in the folder open. Nothing was deleted.
    Busy(io::Error),
    Io(io::Error),
}

impl std::fmt::Display for DeleteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeleteError::Running(l) => match l.pid() {
                Some(pid) => write!(f, "the bot runs (PID {pid}); stop it first"),
                None => write!(f, "the bot runs; stop it first"),
            },
            DeleteError::Locked => write!(f, "the bot started again; stop it first"),
            DeleteError::Busy(e) => write!(f, "A program has a file in the folder open ({e}). Nothing was deleted."),
            DeleteError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for DeleteError {}

/// Delete bot `name`, which the caller has stopped (and waited for). `keep`
/// moves the folder to `bot/<bot>.deleted[-N]`; otherwise it goes to
/// `trash/` and is removed. Returns where the folder went.
pub fn delete_stopped(home: &Home, name: &str, keep: bool) -> Result<PathBuf, DeleteError> {
    delete_with(home, name, keep, LOCK_WAIT, RENAME_WAIT)
}

#[cfg(test)]
static PAUSE_AFTER_RECORD_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn delete_with(
    home: &Home,
    name: &str,
    keep: bool,
    lock_wait: Duration,
    rename_wait: Duration,
) -> Result<PathBuf, DeleteError> {
    let lock = match BotLock::acquire(home, name, lock_wait) {
        Ok(l) => l,
        Err(LockError::Held) => return Err(DeleteError::Locked),
        Err(LockError::Io(e) | LockError::Unsupported(e) | LockError::Permission(e)) => return Err(DeleteError::Io(e)),
    };
    let bot_id = super::bot_id::read(home, name).map(|f| f.id);
    let record = Holder::for_this_process(name, bot_id, Mode::Delete, env!("CARGO_PKG_VERSION"));
    if let Err(e) = lock.write_holder(&record) {
        lock.release();
        return Err(DeleteError::Io(e));
    }
    #[cfg(test)]
    std::thread::sleep(Duration::from_millis(PAUSE_AFTER_RECORD_MS.load(std::sync::atomic::Ordering::SeqCst)));
    let fail = |lock: BotLock, e: DeleteError| {
        let _ = lock.remove_holder();
        lock.release();
        Err(e)
    };
    // A bot started by v0.2.7 or older takes no lock.
    if let Some(l) = super::legacy::check(home, name) {
        return fail(lock, DeleteError::Running(Box::new(Liveness::Legacy(l))));
    }
    match std::fs::remove_file(home.pid_path(name)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return fail(lock, DeleteError::Io(e)),
        _ => {}
    }
    let dir = home.bot_dir(name);
    let hex = format!("{:016x}", rand::random::<u64>());
    let dest = if keep {
        deleted_dir_for(&dir)
    } else {
        if let Err(e) = std::fs::create_dir_all(home.trash_dir()) {
            return fail(lock, DeleteError::Io(e));
        }
        home.trash_dir().join(format!("{name}-{hex}"))
    };
    let moved = match super::fsx::rename_retry(&dir, &dest, rename_wait) {
        Ok(()) => dest,
        // `bot/` on another volume (a symlink): trash in place.
        Err(e) if !keep && e.kind() == io::ErrorKind::CrossesDevices => {
            let alt = home.bots_dir().join(format!(".trash-{name}-{hex}"));
            match super::fsx::rename_retry(&dir, &alt, rename_wait) {
                Ok(()) => alt,
                Err(e) => return fail(lock, DeleteError::Busy(e)),
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => return fail(lock, DeleteError::Io(e)),
        Err(e) => return fail(lock, DeleteError::Busy(e)),
    };
    let _ = lock.remove_holder();
    lock.release();
    if !keep {
        // A failure leaves only trash, which nothing reads; swept later.
        let _ = std::fs::remove_dir_all(&moved);
    }
    Ok(moved)
}

/// Where Delete keeps a bot's history: `<name>.deleted`, then `-2`, `-3`…
pub fn deleted_dir_for(dir: &Path) -> PathBuf {
    let name = dir.file_name().and_then(|s| s.to_str()).unwrap_or("bot");
    let first = dir.with_file_name(format!("{name}.deleted"));
    if !first.exists() {
        return first;
    }
    (2..).map(|n| dir.with_file_name(format!("{name}.deleted-{n}"))).find(|p| !p.exists()).expect("a free name")
}

/// Remove what Deletes left in `trash/` and `bot/.trash-*`. Returns how many
/// entries went. The Lab runs it at start.
pub fn sweep_trash(home: &Home) -> usize {
    let remove = |e: &std::fs::DirEntry| match e.file_type() {
        Ok(t) if t.is_dir() => std::fs::remove_dir_all(e.path()).is_ok(),
        Ok(_) => std::fs::remove_file(e.path()).is_ok(),
        Err(_) => false,
    };
    let mut n = 0;
    if let Ok(entries) = std::fs::read_dir(home.trash_dir()) {
        n += entries.filter_map(Result::ok).filter(|e| remove(e)).count();
    }
    if let Ok(entries) = std::fs::read_dir(home.bots_dir()) {
        n += entries
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with(".trash-"))
            .filter(|e| remove(e))
            .count();
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_bot::testutil::{child_env, respawn, wait_for, ChildGuard, TempHome};
    use std::time::Instant;

    const CHILD: &str = "local_bot::delete::tests::child_deletes_slowly";

    /// Child: a Delete that pauses 3 s after its record.
    #[test]
    #[ignore]
    fn child_deletes_slowly() {
        let Some((home, name)) = child_env() else { return };
        PAUSE_AFTER_RECORD_MS.store(3_000, std::sync::atomic::Ordering::SeqCst);
        let r = delete_stopped(&home, &name, false);
        std::fs::write(home.root().join("child.done"), format!("{r:?}")).unwrap();
    }

    #[test]
    fn a_start_during_delete_is_refused() {
        let t = TempHome::new();
        t.bot("paper");
        let mut child = ChildGuard(respawn(CHILD, &t.home, "paper").spawn().unwrap());
        let t0 = Instant::now();
        while !crate::local_bot::holder::read(&t.home, "paper").ok().flatten().is_some_and(|h| h.mode == Mode::Delete) {
            assert!(t0.elapsed() < Duration::from_secs(20), "the delete record never showed");
            std::thread::sleep(Duration::from_millis(20));
        }
        // The start's own lock attempt fails, then it asks who holds it.
        assert!(matches!(BotLock::acquire(&t.home, "paper", Duration::from_millis(200)), Err(LockError::Held)));
        let l = crate::local_bot::bot_running(&t.home, "paper");
        assert!(matches!(l, Liveness::Deleting(ref h) if h.pid == child.pid()), "{l:?}");
        assert!(l.target().is_none(), "never signalled");
        assert_eq!(
            crate::local_bot::exit::when_held(&l),
            Some((crate::local_bot::exit::REFUSED, Some(crate::local_bot::exit::reason::DELETING)))
        );
        assert!(child.0.wait().unwrap().success());
        let done = std::fs::read_to_string(t.home.root().join("child.done")).unwrap();
        assert!(done.starts_with("Ok("), "{done}");
        assert!(!t.home.bot_dir("paper").exists());
        assert!(t.home.lock_path("paper").exists(), "the lock file stays");
        assert!(!t.home.holder_path("paper").exists());
        assert!(!t.home.holder_last_path("paper").exists());
        assert_eq!(std::fs::read_dir(t.home.trash_dir()).unwrap().count(), 0, "trash removed");
        assert!(!crate::local_bot::bot_running(&t.home, "paper").alive());
    }

    #[test]
    fn keep_moves_to_deleted_names() {
        let t = TempHome::new();
        t.bot("paper");
        std::fs::write(t.home.pid_path("paper"), "999999999").unwrap();
        let to = delete_stopped(&t.home, "paper", true).unwrap();
        assert_eq!(to, t.home.bots_dir().join("paper.deleted"));
        assert!(to.join("config.json").is_file());
        assert!(!to.join("bot.pid").exists(), "bot.pid removed first");
        t.bot("paper");
        assert_eq!(delete_stopped(&t.home, "paper", true).unwrap(), t.home.bots_dir().join("paper.deleted-2"));
        let names: Vec<_> = crate::local_bot::folders::bot_folders(&t.home).into_iter().map(|(n, _)| n).collect();
        assert!(names.is_empty(), "{names:?}");
    }

    #[test]
    fn a_missing_folder_is_an_error_and_leaves_no_record() {
        let t = TempHome::new();
        assert!(matches!(delete_stopped(&t.home, "ghost", false), Err(DeleteError::Io(_))));
        assert!(!t.home.holder_path("ghost").exists());
        BotLock::acquire(&t.home, "ghost", Duration::ZERO).unwrap().release();
    }

    #[test]
    fn a_held_lock_is_locked() {
        let t = TempHome::new();
        t.bot("paper");
        let _child = ChildGuard(respawn("local_bot::lock::tests::child_holds_lock", &t.home, "paper").spawn().unwrap());
        assert!(wait_for(&t.home.root().join("child.ready"), Duration::from_secs(20)));
        let r = delete_with(&t.home, "paper", false, Duration::from_millis(200), RENAME_WAIT);
        assert!(matches!(r, Err(DeleteError::Locked)), "{r:?}");
        assert!(t.home.bot_dir("paper").join("config.json").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn a_live_old_bot_is_not_deleted() {
        let t = TempHome::new();
        let dir = t.bot("paper");
        let bot = crate::local_bot::legacy::tests::fake_old_bot(&dir.join("config.json"));
        std::fs::write(t.home.pid_path("paper"), bot.pid().to_string()).unwrap();
        let r = delete_stopped(&t.home, "paper", false);
        assert!(
            matches!(&r, Err(DeleteError::Running(l)) if matches!(**l, Liveness::Legacy(ref l) if l.pid == bot.pid())),
            "{r:?}"
        );
        assert!(dir.join("config.json").is_file() && t.home.pid_path("paper").is_file(), "untouched");
        assert!(!t.home.holder_path("paper").exists());
        BotLock::acquire(&t.home, "paper", Duration::ZERO).unwrap().release();
    }

    #[test]
    fn sweep_removes_trash_only() {
        let t = TempHome::new();
        t.bot("paper");
        std::fs::create_dir_all(t.home.trash_dir().join("old-0123456789abcdef/logs")).unwrap();
        std::fs::write(t.home.trash_dir().join("stray"), "").unwrap();
        std::fs::create_dir_all(t.home.bots_dir().join(".trash-x-0123456789abcdef")).unwrap();
        std::fs::create_dir_all(t.home.bots_dir().join("paper.deleted")).unwrap();
        assert_eq!(sweep_trash(&t.home), 3);
        assert!(t.home.bot_dir("paper").is_dir());
        assert!(t.home.bots_dir().join("paper.deleted").is_dir());
        assert_eq!(sweep_trash(&t.home), 0);
    }

    /// Opens `file` without `FILE_SHARE_DELETE` for `hold`.
    #[cfg(windows)]
    fn hold_open(file: PathBuf, hold: Duration) -> std::thread::JoinHandle<()> {
        use std::os::windows::fs::OpenOptionsExt;
        let f = std::fs::OpenOptions::new().read(true).share_mode(1 /* FILE_SHARE_READ */).open(file).unwrap();
        std::thread::spawn(move || {
            std::thread::sleep(hold);
            drop(f);
        })
    }

    #[cfg(windows)]
    #[test]
    fn rename_retry_waits_for_a_handle() {
        let t = TempHome::new();
        let dir = t.bot("paper");
        let h = hold_open(dir.join("config.json"), Duration::from_secs(1));
        let to = delete_stopped(&t.home, "paper", true).unwrap();
        assert!(to.join("config.json").is_file());
        h.join().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn held_too_long_leaves_the_folder() {
        let t = TempHome::new();
        let dir = t.bot("paper");
        let h = hold_open(dir.join("config.json"), Duration::from_secs(3));
        let r = delete_with(&t.home, "paper", false, LOCK_WAIT, Duration::from_millis(500));
        assert!(matches!(r, Err(DeleteError::Busy(_))), "{r:?}");
        h.join().unwrap();
        assert!(dir.join("config.json").is_file(), "nothing deleted");
        assert!(!t.home.holder_path("paper").exists());
    }
}
