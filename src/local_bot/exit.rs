//! Exit codes of `tradectl run` and `tradectl stop`, and the refusal reasons
//! of `start-result: refused reason=<reason>`. The Lab maps on both.

use super::running::Liveness;

/// A verified bot holds the lock, or a verified legacy bot runs.
pub const ALREADY_RUNNING: i32 = 73;
/// `run`: alive but not ready in time, or the lock is held by a process with
/// no record yet. `stop`: still stopping.
pub const STILL_GOING: i32 = 75;
/// `stop --force` killed it; positions NOT closed.
pub const KILLED: i32 = 76;
/// `stop`: the bot stopped but said positions were NOT closed.
pub const STOPPED_NOT_CLOSED: i32 = 77;
/// Needs the user; see [`reason`].
pub const REFUSED: i32 = 78;

/// `reason=` values of exit [`REFUSED`].
pub mod reason {
    /// The disk cannot lock files, or `~/.tradectl` is on a network drive.
    pub const LOCK_UNSUPPORTED: &str = "lock-unsupported";
    /// `locks/` or a lock file is not this user's (an old `sudo` run).
    pub const LOCK_PERMISSION: &str = "lock-permission";
    /// This folder is a copy of a stopped bot.
    pub const COPY: &str = "copy";
    /// This folder is a copy of a running bot.
    pub const COPY_OF_LIVE: &str = "copy-of-live";
    /// The folder came from another computer.
    pub const MOVED_HOST: &str = "moved-host";
    /// A Delete holds the lock.
    pub const DELETING: &str = "deleting";
    /// Something may run this bot, but it cannot be confirmed.
    pub const MAYBE_RUNNING: &str = "maybe-running";
}

/// A start whose lock stayed held: the exit code and, for [`REFUSED`], the
/// reason (section 8.4). `None` when the lock is free after all (try again).
pub fn when_held(l: &Liveness) -> Option<(i32, Option<&'static str>)> {
    Some(match l {
        Liveness::Running(_) => (ALREADY_RUNNING, None),
        Liveness::Legacy(p) if p.verified => (ALREADY_RUNNING, None),
        Liveness::Legacy(_) => (REFUSED, Some(reason::MAYBE_RUNNING)),
        Liveness::Deleting(_) => (REFUSED, Some(reason::DELETING)),
        Liveness::Starting { .. } => (STILL_GOING, None),
        Liveness::Unknown(_) => (REFUSED, Some(reason::MAYBE_RUNNING)),
        Liveness::NotRunning(_) => return None,
    })
}
