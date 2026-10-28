//! The holder record, `locks/<bot>.holder`: who holds the bot's lock.
//!
//! Written temp + rename right after the lock is taken. Before the holder
//! unlocks, it renames the record to `<bot>.holder.last` (L1), so a new
//! holder in its first milliseconds is never read as "held + the old,
//! still-verified record". Liveness reads only `<bot>.holder`.

use std::io;

use serde::{Deserialize, Serialize};

use super::lock::BotLock;
use super::signal::Target;
use super::Home;

pub const HOLDER_V: u32 = 1;

/// What the holding process is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// `tradectl run <bot>` in a terminal, Docker.
    Foreground,
    /// The background bot `run --daemon` starts.
    DaemonChild,
    /// Under systemd (`TRADECTL_SERVICE` set).
    Service,
    /// A Delete holds the lock: never signalled.
    Delete,
    /// A mode a newer tradectl wrote. Counts as a bot.
    #[serde(other)]
    Other,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Foreground => "foreground",
            Mode::DaemonChild => "daemon-child",
            Mode::Service => "service",
            Mode::Delete => "delete",
            Mode::Other => "other",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holder {
    pub v: u32,
    pub name: String,
    #[serde(default)]
    pub bot_id: Option<String>,
    /// 128 random bits per process; ties `status.json` and `/v1/status` to it.
    pub run_id: String,
    pub pid: u32,
    /// The OS start token, compared for exact equality (`proc::lookup`).
    pub start: String,
    /// Linux and macOS boot id; `None` on Windows.
    #[serde(default)]
    pub boot: Option<String>,
    /// Linux: inode of `/proc/self/ns/pid`.
    #[serde(default)]
    pub pid_ns: Option<u64>,
    #[serde(default)]
    pub exe: Option<String>,
    pub version: String,
    pub mode: Mode,
    pub since_ms: i64,
}

impl Holder {
    /// This process as the holder of bot `name`, with a fresh `run_id`.
    pub fn for_this_process(name: &str, bot_id: Option<String>, mode: Mode, version: &str) -> Holder {
        let me = super::proc::me();
        Holder {
            v: HOLDER_V,
            name: name.to_string(),
            bot_id,
            run_id: new_run_id(),
            pid: std::process::id(),
            start: me.start,
            boot: super::proc::boot_id(),
            pid_ns: super::proc::pid_ns(),
            exe: std::env::current_exe().ok().map(|p| p.to_string_lossy().into_owned()),
            version: version.to_string(),
            mode,
            since_ms: super::now_ms(),
        }
    }

    /// The process to signal; only from a record `bot_running` verified.
    pub fn target(&self) -> Target {
        Target { pid: self.pid, start: self.start.clone() }
    }
}

/// 128 random bits as 32 hex chars.
pub fn new_run_id() -> String {
    super::random_hex128()
}

/// The current record: `Ok(None)` when there is none, `Err` when it cannot
/// be read or parsed (a holder mid-write never shows: the write renames).
pub fn read(home: &Home, name: &str) -> Result<Option<Holder>, String> {
    let path = home.holder_path(name);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map(Some).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// The last holder's record, history for messages only.
pub fn read_last(home: &Home, name: &str) -> Option<Holder> {
    serde_json::from_str(&std::fs::read_to_string(home.holder_last_path(name)).ok()?).ok()
}

impl BotLock {
    /// Write this holder's record (in `locks/`, outside the bot folder).
    pub fn write_holder(&self, record: &Holder) -> io::Result<()> {
        let text = serde_json::to_string_pretty(record).map_err(io::Error::other)?;
        super::fsx::write_atomic(&self.home().holder_path(self.name()), text.as_bytes(), false)
    }

    /// `<bot>.holder` to `<bot>.holder.last`; call just before `release`.
    pub fn retire_holder(&self) -> io::Result<()> {
        let (from, to) = (self.home().holder_path(self.name()), self.home().holder_last_path(self.name()));
        match super::fsx::rename_retry(&from, &to, std::time::Duration::from_secs(2)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            r => r,
        }
    }

    /// Remove the record (Delete's own record never goes to `.last`).
    pub fn remove_holder(&self) -> io::Result<()> {
        match std::fs::remove_file(self.home().holder_path(self.name())) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            r => r,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_bot::testutil::TempHome;
    use std::time::Duration;

    #[test]
    fn a_record_round_trips_and_retires() {
        let t = TempHome::new();
        let lock = BotLock::acquire(&t.home, "paper", Duration::ZERO).unwrap();
        let rec = Holder::for_this_process("paper", Some("bx".into()), Mode::DaemonChild, "0.2.8");
        assert_eq!(rec.run_id.len(), 32);
        assert_eq!(rec.pid, std::process::id());
        assert!(!rec.start.is_empty(), "this OS has a start token");
        lock.write_holder(&rec).unwrap();
        assert_eq!(read(&t.home, "paper").unwrap(), Some(rec.clone()));
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(t.home.holder_path("paper")).unwrap()).unwrap();
        assert_eq!(raw["mode"], "daemon-child");
        assert_eq!(raw["v"], 1);
        lock.retire_holder().unwrap();
        assert_eq!(read(&t.home, "paper").unwrap(), None);
        assert_eq!(read_last(&t.home, "paper"), Some(rec));
        lock.retire_holder().unwrap();
        lock.release();
    }

    #[test]
    fn an_unknown_mode_still_parses() {
        let text = r#"{"v":1,"name":"a","run_id":"r","pid":5,"start":"lnx:1","version":"0.3","mode":"supervised","since_ms":0}"#;
        let h: Holder = serde_json::from_str(text).unwrap();
        assert_eq!(h.mode, Mode::Other);
        assert_eq!(h.bot_id, None);
    }

    #[test]
    fn garbage_is_an_error_not_none() {
        let t = TempHome::new();
        std::fs::create_dir_all(t.home.locks_dir()).unwrap();
        std::fs::write(t.home.holder_path("paper"), "{not json").unwrap();
        assert!(read(&t.home, "paper").is_err());
        assert!(read(&t.home, "other").unwrap().is_none());
    }

    #[test]
    fn run_ids_differ() {
        assert_ne!(new_run_id(), new_run_id());
    }
}
