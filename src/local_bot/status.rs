//! `bot/<bot>/status.json`: what the lock holder is doing.
//!
//! The lock decides "running"; this file says what the bot is doing, or,
//! once the lock is free, how it ended. Only the holder writes it, through
//! one writer thread ([`StatusWriter::spawn`]): engine code never waits on
//! the disk in front of a flatten.

use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::holder::{Holder, Mode};
use super::Home;

pub const STATUS_V: u32 = 1;
/// `reason` is cut to this many chars; it never holds a secret anyway.
pub const REASON_MAX: usize = 300;
/// Field-only updates are written at most once per this.
const COALESCE: Duration = Duration::from_secs(1);
/// How long an exit path waits for the terminal write.
const TERMINAL_WAIT: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// CLI: lock and record taken.
    Locked,
    /// CLI: reading and checking config.
    Config,
    /// CLI: binding the port.
    Port,
    /// CLI: loading the strategy plugin.
    Plugin,
    /// Engine: parked with no strategy, waiting for the Lab.
    AwaitingSetup,
    /// Engine: connecting to the exchange.
    Exchange,
    /// Engine: starting strategies.
    Strategies,
    /// Engine: trading pipeline up.
    Running,
    /// Park: network error; retry at `retry_at_ms`.
    WaitingNetwork,
    /// Park: until `retry_at_ms`.
    IpBanned,
    /// Park or CLI: unknown error, backoff.
    Restarting,
    /// Park: cannot trade; `reason`; needs the user.
    Parked,
    /// Close-out under way.
    Stopping,
    /// Terminal: clean exit; see `close_out`.
    Stopped,
    /// Terminal: ended on its own; `exit_code`, `reason`.
    Exited,
    /// A step a newer tradectl wrote.
    #[serde(other)]
    Other,
}

impl Step {
    pub fn as_str(self) -> &'static str {
        match self {
            Step::Locked => "locked",
            Step::Config => "config",
            Step::Port => "port",
            Step::Plugin => "plugin",
            Step::AwaitingSetup => "awaiting_setup",
            Step::Exchange => "exchange",
            Step::Strategies => "strategies",
            Step::Running => "running",
            Step::WaitingNetwork => "waiting_network",
            Step::IpBanned => "ip_banned",
            Step::Restarting => "restarting",
            Step::Parked => "parked",
            Step::Stopping => "stopping",
            Step::Stopped => "stopped",
            Step::Exited => "exited",
            Step::Other => "other",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Step::Stopped | Step::Exited)
    }

    /// `run --daemon` may return "ready" at this step (section 8.3). A bot
    /// without the lab API exits on an auth or config error, so `exchange`
    /// is too early to call it ready.
    pub fn is_ready(self, lab: bool) -> bool {
        match self {
            Step::Running | Step::WaitingNetwork | Step::IpBanned | Step::Restarting => true,
            Step::AwaitingSetup | Step::Exchange | Step::Strategies | Step::Parked => lab,
            _ => false,
        }
    }
}

/// `reason_key`: the Lab translates by key and shows `reason` as detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReasonKey {
    #[serde(rename = "network_dns")]
    NetworkDns,
    #[serde(rename = "network_timeout")]
    NetworkTimeout,
    #[serde(rename = "network_refused")]
    NetworkRefused,
    #[serde(rename = "server_5xx")]
    Server5xx,
    #[serde(rename = "ws_connect")]
    WsConnect,
    #[serde(rename = "rate_429")]
    Rate429,
    #[serde(rename = "ip_ban_418")]
    IpBan418,
    #[serde(rename = "geo_451")]
    Geo451,
    #[serde(rename = "waf_403")]
    Waf403,
    #[serde(rename = "tls_intercept")]
    TlsIntercept,
    #[serde(rename = "auth_2015")]
    Auth2015,
    #[serde(rename = "auth_2014")]
    Auth2014,
    #[serde(rename = "clock_1021")]
    Clock1021,
    #[serde(rename = "config")]
    Config,
    #[serde(rename = "plugin_incompatible")]
    PluginIncompatible,
    #[serde(rename = "plugin_missing")]
    PluginMissing,
    /// Windows code integrity (Smart App Control, WDAC, a policy) refused
    /// the strategy file.
    #[serde(rename = "plugin_blocked")]
    PluginBlocked,
    #[serde(rename = "port_taken")]
    PortTaken,
    #[serde(rename = "port_reserved")]
    PortReserved,
    #[serde(rename = "crash_loop")]
    CrashLoop,
    /// Also any key a newer tradectl wrote.
    #[serde(rename = "unknown", other)]
    Unknown,
}

impl ReasonKey {
    pub fn as_str(self) -> &'static str {
        match self {
            ReasonKey::NetworkDns => "network_dns",
            ReasonKey::NetworkTimeout => "network_timeout",
            ReasonKey::NetworkRefused => "network_refused",
            ReasonKey::Server5xx => "server_5xx",
            ReasonKey::WsConnect => "ws_connect",
            ReasonKey::Rate429 => "rate_429",
            ReasonKey::IpBan418 => "ip_ban_418",
            ReasonKey::Geo451 => "geo_451",
            ReasonKey::Waf403 => "waf_403",
            ReasonKey::TlsIntercept => "tls_intercept",
            ReasonKey::Auth2015 => "auth_2015",
            ReasonKey::Auth2014 => "auth_2014",
            ReasonKey::Clock1021 => "clock_1021",
            ReasonKey::Config => "config",
            ReasonKey::PluginIncompatible => "plugin_incompatible",
            ReasonKey::PluginMissing => "plugin_missing",
            ReasonKey::PluginBlocked => "plugin_blocked",
            ReasonKey::PortTaken => "port_taken",
            ReasonKey::PortReserved => "port_reserved",
            ReasonKey::CrashLoop => "crash_loop",
            ReasonKey::Unknown => "unknown",
        }
    }
}

/// How a stop's close-out ended. `null` in the file: nothing to close.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CloseOut {
    Done {
        closed: u32,
    },
    Failed {
        left: u32,
        #[serde(default)]
        error: Option<String>,
    },
    /// Only some symbols could be checked.
    Partial {
        closed: u32,
    },
    /// The bot closed every position it tracked; the venue has no account
    /// read to confirm it. `venue`: the provider in lower case; `market`:
    /// `linear`, `inverse` or `spot`.
    Unchecked {
        closed: u32,
        venue: String,
        market: String,
    },
    /// A state a newer tradectl wrote. Reads as failed.
    #[serde(other)]
    Unknown,
}

/// The name a person knows `venue` by, with " Spot" for `market` `spot`.
pub fn venue_display(venue: &str, market: &str) -> String {
    let name = match venue {
        "okx" => "OKX",
        "gate" => "Gate",
        "htx" => "HTX",
        "hyperliquid" => "Hyperliquid",
        "bitget" => "Bitget",
        "binance" => "Binance",
        "bybit" => "Bybit",
        other => other,
    };
    if market == "spot" {
        format!("{name} Spot")
    } else {
        name.to_string()
    }
}

/// The run before this one ended without a terminal step (section 2.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrevEnd {
    pub run_id: String,
    /// Its last step: `stopping` = cut off while closing positions.
    pub step: Step,
    /// Its last `updated_ms`.
    pub at_ms: i64,
    /// `crash.log` is newer: the crash was paged already.
    pub crashed: bool,
    /// The run could hold positions on an exchange: `false` for a step that trades
    /// nothing (setup, park, config) or a paper-only bot. `None` from an older writer.
    #[serde(default)]
    pub live: Option<bool>,
}

/// The last time the bot did not run for a while (section 6.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeNote {
    /// Wall-clock times, for the text only.
    pub from_ms: i64,
    pub to_ms: i64,
    /// Of the gap, the time the computer slept.
    pub slept_ms: u64,
    /// The whole gap on the boot clock. `slept_ms >= gap_ms / 2`: the
    /// computer slept; else the bot did not run.
    #[serde(default)]
    pub gap_ms: u64,
    pub resync: ResyncNote,
}

/// The order re-sync after a wake.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ResyncNote {
    Pending,
    Done {
        fills: u32,
        cancels: u32,
    },
    TimedOut,
    /// A paper-only bot: nothing crossed a network.
    NotNeeded,
    /// A state a newer tradectl wrote.
    #[serde(other)]
    Other,
}

/// A strategy plugin as loaded (section 7.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginNote {
    #[serde(rename = "type")]
    pub ty: String,
    pub name: String,
    /// The first 16 hex chars of the file's SHA-256.
    pub sha: String,
    /// The copy the bot loaded.
    pub file: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusDoc {
    pub v: u32,
    pub bot: String,
    /// The canonical bot folder; a copied folder carries the original's file.
    pub dir: String,
    #[serde(default)]
    pub bot_id: Option<String>,
    pub run_id: String,
    pub pid: u32,
    pub version: String,
    pub mode: Mode,
    /// The `TRADECTL_START_NONCE` the `--daemon` parent passed.
    #[serde(default)]
    pub start_nonce: Option<String>,
    pub step: Step,
    /// When `step` began.
    pub since_ms: i64,
    pub updated_ms: i64,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub reason_key: Option<ReasonKey>,
    #[serde(default)]
    pub attempt: Option<u32>,
    #[serde(default)]
    pub retry_at_ms: Option<i64>,
    /// Start of the current network-wait streak.
    #[serde(default)]
    pub waiting_since_ms: Option<i64>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub bind: Option<String>,
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub close_out: Option<CloseOut>,
    /// e.g. `positions_not_closed`.
    #[serde(default)]
    pub alert: Option<String>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// The run before ended without a stop.
    #[serde(default)]
    pub previous: Option<PrevEnd>,
    #[serde(default)]
    pub wake: Option<WakeNote>,
    #[serde(default)]
    pub plugins: Vec<PluginNote>,
}

/// How the holder ends ([`StatusHandle::terminal_blocking`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Terminal {
    /// A clean stop (exit 0); `alert` set when positions were NOT closed.
    Stopped { close_out: Option<CloseOut>, alert: Option<String> },
    /// Ended on its own: crash loop, port error, resource stop, forced exit.
    Exited { code: i32, reason: String, reason_key: Option<ReasonKey> },
}

/// A fresh `TRADECTL_START_NONCE` (128 random bits).
pub fn new_start_nonce() -> String {
    super::random_hex128()
}

/// `s` cut to [`REASON_MAX`] chars.
pub fn clip_reason(s: &str) -> String {
    s.chars().take(REASON_MAX).collect()
}

enum Msg {
    Wake { urgent: bool },
    Flush(mpsc::Sender<()>),
}

struct Shared {
    doc: Mutex<StatusDoc>,
    #[cfg(test)]
    stall: std::sync::atomic::AtomicBool,
}

impl Shared {
    fn doc(&self) -> std::sync::MutexGuard<'_, StatusDoc> {
        self.doc.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Starts the one writer thread of a holder.
pub struct StatusWriter;

impl StatusWriter {
    /// Once, by the lock holder, right after `bot.pid` (step `locked`). No
    /// process global: the CLI hands clones of the handle to the engine;
    /// tests and the e2e harness pass none and write nothing.
    pub fn spawn(
        home: &Home,
        name: &str,
        run_id: &str,
        pid: u32,
        version: &str,
        mode: Mode,
        start_nonce: Option<String>,
    ) -> StatusHandle {
        let dir = home.bot_dir(name);
        let now = super::now_ms();
        let doc = StatusDoc {
            v: STATUS_V,
            bot: name.to_string(),
            dir: std::fs::canonicalize(&dir).unwrap_or(dir).to_string_lossy().into_owned(),
            bot_id: None,
            run_id: run_id.to_string(),
            pid,
            version: version.to_string(),
            mode,
            start_nonce,
            step: Step::Locked,
            since_ms: now,
            updated_ms: now,
            reason: None,
            reason_key: None,
            attempt: None,
            retry_at_ms: None,
            waiting_since_ms: None,
            port: None,
            bind: None,
            notes: Vec::new(),
            close_out: None,
            alert: None,
            exit_code: None,
            previous: None,
            wake: None,
            plugins: Vec::new(),
        };
        let shared = Arc::new(Shared {
            doc: Mutex::new(doc),
            #[cfg(test)]
            stall: std::sync::atomic::AtomicBool::new(false),
        });
        let (tx, rx) = mpsc::channel();
        let path = home.status_path(name);
        let thread_shared = shared.clone();
        let spawned =
            std::thread::Builder::new().name("status-writer".into()).spawn(move || writer(path, thread_shared, rx));
        if let Err(e) = spawned {
            log::warn!("status.json writer did not start: {e}");
        }
        let handle = StatusHandle { shared, tx };
        let _ = handle.tx.send(Msg::Wake { urgent: true });
        handle
    }
}

/// The holder's view of its `status.json`. Every call but
/// [`terminal_blocking`](StatusHandle::terminal_blocking) returns at once.
#[derive(Clone)]
pub struct StatusHandle {
    shared: Arc<Shared>,
    tx: mpsc::Sender<Msg>,
}

impl StatusHandle {
    /// A new step, written at once. A step change also clears `reason_key`,
    /// `attempt` and `retry_at_ms`; set those after, with
    /// [`update`](StatusHandle::update).
    pub fn step(&self, step: Step, reason: Option<String>) {
        self.edit(true, |d| {
            if d.step != step {
                d.step = step;
                d.since_ms = super::now_ms();
                d.reason_key = None;
                d.attempt = None;
                d.retry_at_ms = None;
            }
            d.reason = reason.as_deref().map(clip_reason);
        });
    }

    /// Change fields; written within a second (the last state wins).
    pub fn update(&self, f: impl FnOnce(&mut StatusDoc)) {
        self.edit(false, f);
    }

    /// The final step, written with `sync_all`; waits for the writer at most
    /// 500 ms. Only the exit paths call it.
    pub fn terminal_blocking(&self, t: Terminal) {
        self.edit_doc(|d| {
            match t {
                Terminal::Stopped { close_out, alert } => {
                    d.step = Step::Stopped;
                    d.close_out = close_out;
                    d.alert = alert;
                    d.exit_code = Some(0);
                }
                Terminal::Exited { code, reason, reason_key } => {
                    d.step = Step::Exited;
                    d.exit_code = Some(code);
                    d.reason = Some(clip_reason(&reason));
                    d.reason_key = reason_key;
                }
            }
            d.since_ms = super::now_ms();
        });
        let (ack, done) = mpsc::channel();
        if self.tx.send(Msg::Flush(ack)).is_ok() {
            let _ = done.recv_timeout(TERMINAL_WAIT);
        }
    }

    /// The state as last set (not necessarily written yet).
    pub fn snapshot(&self) -> StatusDoc {
        self.shared.doc().clone()
    }

    fn edit_doc(&self, f: impl FnOnce(&mut StatusDoc)) {
        let mut d = self.shared.doc();
        f(&mut d);
        d.updated_ms = super::now_ms();
    }

    fn edit(&self, urgent: bool, f: impl FnOnce(&mut StatusDoc)) {
        self.edit_doc(f);
        let _ = self.tx.send(Msg::Wake { urgent });
    }

    #[cfg(test)]
    fn stall(&self, on: bool) {
        self.shared.stall.store(on, std::sync::atomic::Ordering::SeqCst);
    }

    /// Tests: wait until the writer has written everything queued.
    #[cfg(test)]
    pub(crate) fn wait_idle(&self) {
        let (ack, done) = mpsc::channel();
        if self.tx.send(Msg::Flush(ack)).is_ok() {
            let _ = done.recv_timeout(Duration::from_secs(10));
        }
    }
}

fn writer(path: PathBuf, shared: Arc<Shared>, rx: mpsc::Receiver<Msg>) {
    let mut last: Option<Instant> = None;
    let mut pending = false;
    let mut warned = false;
    let mut write = |sync: bool, last: &mut Option<Instant>| {
        #[cfg(test)]
        while shared.stall.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let doc = shared.doc().clone();
        let r = serde_json::to_vec_pretty(&doc)
            .map_err(std::io::Error::other)
            .and_then(|bytes| super::fsx::write_atomic(&path, &bytes, sync));
        if let Err(e) = r {
            if !warned {
                log::warn!("cannot write {}: {e}", path.display());
                warned = true;
            }
        }
        *last = Some(Instant::now());
    };
    loop {
        let first = if pending {
            let wait = last.map(|t| (t + COALESCE).saturating_duration_since(Instant::now())).unwrap_or_default();
            match rx.recv_timeout(wait) {
                Ok(m) => Some(m),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    write(false, &mut last);
                    return;
                }
            }
        } else {
            match rx.recv() {
                Ok(m) => Some(m),
                Err(_) => return,
            }
        };
        // One write covers everything queued: the document is shared.
        let (mut urgent, mut acks, mut closed) = (first.is_none(), Vec::new(), false);
        let mut take = |m: Msg| match m {
            Msg::Wake { urgent: u } => urgent |= u,
            Msg::Flush(ack) => acks.push(ack),
        };
        if let Some(m) = first {
            take(m);
        }
        loop {
            match rx.try_recv() {
                Ok(m) => take(m),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    closed = true;
                    break;
                }
            }
        }
        if urgent || !acks.is_empty() || closed || last.is_none_or(|t| t.elapsed() >= COALESCE) {
            write(!acks.is_empty(), &mut last);
            pending = false;
            for ack in acks {
                let _ = ack.send(());
            }
        } else {
            pending = true;
        }
        if closed {
            return;
        }
    }
}

/// The file as it is, whoever wrote it.
pub fn read(home: &Home, name: &str) -> Option<StatusDoc> {
    serde_json::from_str(&std::fs::read_to_string(home.status_path(name)).ok()?).ok()
}

/// The file when it was written for THIS folder (a copied folder carries
/// the original's).
pub fn read_here(home: &Home, name: &str) -> Option<StatusDoc> {
    read(home, name).filter(|d| same_dir(Path::new(&d.dir), &home.bot_dir(name)))
}

/// Lock held: the holder's current state, once it wrote one (another
/// `run_id` = not written yet: show "starting").
pub fn current(home: &Home, holder: &Holder) -> Option<StatusDoc> {
    read_here(home, &holder.name).filter(|d| d.run_id == holder.run_id)
}

fn same_dir(recorded: &Path, dir: &Path) -> bool {
    match std::fs::canonicalize(dir) {
        Ok(c) => c == recorded || std::fs::canonicalize(recorded).is_ok_and(|r| r == c),
        Err(_) => recorded == dir,
    }
}

/// How a bot whose lock is free ended, from `status.json` (section 11.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Last {
    /// No `status.json` for this folder: a v0.2.7 bot, or never started.
    Never,
    Stopped {
        close_out: Option<CloseOut>,
    },
    Exited {
        code: Option<i32>,
        reason: Option<String>,
    },
    /// The last step was not terminal: killed, or crashed. The reason comes
    /// from `crash.log` when it is newer.
    Crashed {
        step: Step,
        reason: Option<String>,
    },
}

/// [`Last`] for bot `name`. Only meaningful while the lock is free.
pub fn last(home: &Home, name: &str) -> Last {
    let Some(doc) = read_here(home, name) else { return Last::Never };
    match doc.step {
        Step::Stopped => Last::Stopped { close_out: doc.close_out },
        Step::Exited => Last::Exited { code: doc.exit_code, reason: doc.reason },
        step => Last::Crashed { step, reason: crash_reason(home, name).or(doc.reason) },
    }
}

/// The last line of `crash.log` when it is newer than `status.json`.
fn crash_reason(home: &Home, name: &str) -> Option<String> {
    let mtime = |p: PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let crash = mtime(home.crash_log_path(name))?;
    if mtime(home.status_path(name)).is_some_and(|s| s > crash) {
        return None;
    }
    let text = std::fs::read_to_string(home.crash_log_path(name)).ok()?;
    text.lines().rev().map(str::trim).find(|l| !l.is_empty()).map(clip_reason)
}

/// What a stop's last record says about the close-out (section 12 step 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseOutVerdict {
    /// Stopped, every position closed.
    Done,
    /// Stopped, nothing to close.
    Nothing,
    /// Stopped, positions NOT closed.
    Failed,
    /// Stopped, only some symbols could be checked.
    Partial,
    /// Stopped, closed what it tracked; the venue has no account read to
    /// confirm it.
    Unchecked,
    /// Ended before it finished closing (aborted, OOM, SIGKILL), or no
    /// record of this run.
    Unknown,
    /// An older bot: it writes no `status.json`.
    Unconfirmed,
}

impl CloseOutVerdict {
    /// 0, or 77 when positions may be open.
    pub fn exit_code(self) -> i32 {
        match self {
            CloseOutVerdict::Done
            | CloseOutVerdict::Nothing
            | CloseOutVerdict::Unchecked
            | CloseOutVerdict::Unconfirmed => 0,
            CloseOutVerdict::Failed | CloseOutVerdict::Partial | CloseOutVerdict::Unknown => {
                super::exit::STOPPED_NOT_CLOSED
            }
        }
    }

    /// `stop-result: stopped close_out=<word>`.
    pub fn word(self) -> &'static str {
        match self {
            CloseOutVerdict::Done => "done",
            CloseOutVerdict::Nothing => "none",
            CloseOutVerdict::Failed => "failed",
            CloseOutVerdict::Partial => "partial",
            CloseOutVerdict::Unchecked => "unchecked",
            CloseOutVerdict::Unknown => "unknown",
            CloseOutVerdict::Unconfirmed => "unconfirmed",
        }
    }

    pub fn message(self) -> Option<&'static str> {
        match self {
            CloseOutVerdict::Done | CloseOutVerdict::Nothing => None,
            CloseOutVerdict::Failed => Some("positions NOT closed; check the exchange"),
            CloseOutVerdict::Partial => Some("only some symbols could be checked; check the exchange"),
            CloseOutVerdict::Unchecked => {
                Some("closed what it tracked; the exchange gives no account read to confirm it")
            }
            CloseOutVerdict::Unknown => Some("the bot ended before it finished closing; check the exchange"),
            CloseOutVerdict::Unconfirmed => Some("older bot: cannot confirm the close-out"),
        }
    }
}

/// After the stopped process ended: what did its record say? `run_id` is
/// the target holder's; `None` for a legacy target.
pub fn close_out_verdict(home: &Home, name: &str, run_id: Option<&str>) -> CloseOutVerdict {
    let Some(run_id) = run_id else { return CloseOutVerdict::Unconfirmed };
    let Some(doc) = stopped_doc(home, name, run_id) else { return CloseOutVerdict::Unknown };
    match (&doc.close_out, &doc.alert) {
        (Some(CloseOut::Partial { .. }), _) => CloseOutVerdict::Partial,
        (Some(CloseOut::Failed { .. } | CloseOut::Unknown), _) | (_, Some(_)) => CloseOutVerdict::Failed,
        (Some(CloseOut::Unchecked { .. }), None) => CloseOutVerdict::Unchecked,
        (Some(CloseOut::Done { .. }), None) => CloseOutVerdict::Done,
        (None, None) => CloseOutVerdict::Nothing,
    }
}

/// The close-out a stopped run recorded, for the words around a verdict
/// (the venue of [`CloseOut::Unchecked`]). `None`: a legacy target, another
/// run, not stopped, or nothing to close.
pub fn close_out_doc(home: &Home, name: &str, run_id: Option<&str>) -> Option<CloseOut> {
    stopped_doc(home, name, run_id?)?.close_out
}

fn stopped_doc(home: &Home, name: &str, run_id: &str) -> Option<StatusDoc> {
    read(home, name).filter(|d| d.run_id == run_id && d.step == Step::Stopped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_bot::testutil::TempHome;

    fn spawn(t: &TempHome, run_id: &str) -> StatusHandle {
        StatusWriter::spawn(&t.home, "paper", run_id, std::process::id(), "0.2.8", Mode::DaemonChild, Some("n1".into()))
    }

    fn wait_step(t: &TempHome, step: Step) -> StatusDoc {
        let t0 = Instant::now();
        loop {
            if let Some(d) = read(&t.home, "paper").filter(|d| d.step == step) {
                return d;
            }
            assert!(t0.elapsed() < Duration::from_secs(5), "never saw {step:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn steps_are_written_in_order_and_the_end_is_synced() {
        let t = TempHome::new();
        t.bot("paper");
        let h = spawn(&t, "r1");
        let d = wait_step(&t, Step::Locked);
        assert_eq!(d.start_nonce.as_deref(), Some("n1"));
        assert_eq!(d.mode, Mode::DaemonChild);
        assert!(same_dir(Path::new(&d.dir), &t.home.bot_dir("paper")));
        h.step(Step::Config, None);
        wait_step(&t, Step::Config);
        h.step(Step::WaitingNetwork, Some("x".repeat(400)));
        h.update(|d| {
            d.reason_key = Some(ReasonKey::NetworkTimeout);
            d.attempt = Some(2);
        });
        let d = wait_step(&t, Step::WaitingNetwork);
        assert_eq!(d.reason.as_ref().map(|r| r.chars().count()), Some(REASON_MAX));
        h.terminal_blocking(Terminal::Stopped { close_out: Some(CloseOut::Done { closed: 3 }), alert: None });
        // Written before terminal_blocking returned.
        let d = read(&t.home, "paper").unwrap();
        assert_eq!(d.step, Step::Stopped);
        assert_eq!(d.close_out, Some(CloseOut::Done { closed: 3 }));
        assert_eq!(d.exit_code, Some(0));
        assert_eq!(d.reason_key, Some(ReasonKey::NetworkTimeout), "terminal keeps the fields");
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(t.home.status_path("paper")).unwrap()).unwrap();
        assert_eq!(raw["step"], "stopped");
        assert_eq!(raw["close_out"]["state"], "done");
        assert_eq!(raw["reason_key"], "network_timeout");
        h.wait_idle();
    }

    #[test]
    fn a_new_step_clears_the_park_fields() {
        let t = TempHome::new();
        t.bot("paper");
        let h = spawn(&t, "r1");
        h.step(Step::IpBanned, Some("banned".into()));
        h.update(|d| {
            d.reason_key = Some(ReasonKey::IpBan418);
            d.retry_at_ms = Some(5);
            d.attempt = Some(1);
        });
        h.step(Step::Running, None);
        let s = h.snapshot();
        assert_eq!((s.reason_key, s.retry_at_ms, s.attempt, s.reason), (None, None, None, None));
        h.wait_idle();
    }

    #[test]
    fn status_handle_never_blocks() {
        let t = TempHome::new();
        t.bot("paper");
        let h = spawn(&t, "r1");
        wait_step(&t, Step::Locked);
        h.stall(true);
        let t0 = Instant::now();
        for _ in 0..1000 {
            h.step(Step::Exchange, None);
            h.update(|d| d.attempt = Some(1));
        }
        assert!(t0.elapsed() < Duration::from_millis(200), "{:?}", t0.elapsed());
        // The terminal write gives up after 500 ms when the disk hangs.
        let t0 = Instant::now();
        h.terminal_blocking(Terminal::Exited { code: 143, reason: "forced".into(), reason_key: None });
        assert!(t0.elapsed() < Duration::from_millis(900));
        h.stall(false);
        let d = wait_step(&t, Step::Exited);
        assert_eq!(d.exit_code, Some(143));
        h.wait_idle();
    }

    #[test]
    fn field_updates_are_coalesced() {
        let t = TempHome::new();
        t.bot("paper");
        let h = spawn(&t, "r1");
        wait_step(&t, Step::Locked);
        for i in 0..50 {
            h.update(|d| d.attempt = Some(i));
        }
        // The last state wins, within about a second.
        let t0 = Instant::now();
        while read(&t.home, "paper").and_then(|d| d.attempt) != Some(49) {
            assert!(t0.elapsed() < Duration::from_secs(3));
            std::thread::sleep(Duration::from_millis(20));
        }
        h.wait_idle();
    }

    #[test]
    fn reader_ignores_another_dir() {
        let t = TempHome::new();
        t.bot("paper");
        let h = spawn(&t, "r1");
        h.terminal_blocking(Terminal::Stopped { close_out: None, alert: None });
        h.wait_idle();
        drop(h);
        assert!(read_here(&t.home, "paper").is_some());
        assert_eq!(last(&t.home, "paper"), Last::Stopped { close_out: None });
        // A copy of the folder carries the original's file.
        let copy = t.bot("copy");
        std::fs::copy(t.home.status_path("paper"), copy.join("status.json")).unwrap();
        assert!(read(&t.home, "copy").is_some());
        assert!(read_here(&t.home, "copy").is_none());
        assert_eq!(last(&t.home, "copy"), Last::Never);
        assert_eq!(last(&t.home, "nobody"), Last::Never);
    }

    #[test]
    fn non_terminal_with_free_lock_is_crashed() {
        let t = TempHome::new();
        t.bot("paper");
        let h = spawn(&t, "r1");
        h.step(Step::Stopping, Some("closing".into()));
        h.wait_idle();
        drop(h);
        let t0 = Instant::now();
        while read(&t.home, "paper").map(|d| d.step) != Some(Step::Stopping) {
            assert!(t0.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(last(&t.home, "paper"), Last::Crashed { step: Step::Stopping, reason: Some("closing".into()) });
        // Through bot_running: the lock is free (never taken here).
        assert_eq!(
            crate::local_bot::bot_running_now(&t.home, "paper"),
            crate::local_bot::Liveness::NotRunning(Last::Crashed {
                step: Step::Stopping,
                reason: Some("closing".into())
            })
        );
        // A newer crash.log gives the reason.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(t.home.crash_log_path("paper"), "old\npanicked at runner.rs:1\n\n").unwrap();
        assert_eq!(
            last(&t.home, "paper"),
            Last::Crashed { step: Step::Stopping, reason: Some("panicked at runner.rs:1".into()) }
        );
    }

    #[test]
    fn close_out_verdict_matrix() {
        use CloseOutVerdict as V;
        let t = TempHome::new();
        t.bot("paper");
        assert_eq!(close_out_verdict(&t.home, "paper", None), V::Unconfirmed);
        assert_eq!(close_out_verdict(&t.home, "paper", Some("r1")), V::Unknown, "no status.json");
        let end = |run_id: &str, term: Option<Terminal>| {
            let h = spawn(&t, run_id);
            match term {
                Some(term) => h.terminal_blocking(term),
                None => {
                    h.step(Step::Stopping, None);
                    h.terminal_blocking(Terminal::Stopped { close_out: None, alert: None });
                    // Overwrite as an abort mid close-out would leave it.
                    let mut d = read(&t.home, "paper").unwrap();
                    d.step = Step::Stopping;
                    std::fs::write(t.home.status_path("paper"), serde_json::to_vec(&d).unwrap()).unwrap();
                }
            }
        };
        end("r1", Some(Terminal::Stopped { close_out: Some(CloseOut::Done { closed: 2 }), alert: None }));
        assert_eq!(close_out_verdict(&t.home, "paper", Some("r1")), V::Done);
        assert_eq!(close_out_verdict(&t.home, "paper", Some("r2")), V::Unknown, "another run_id");
        end("r1", Some(Terminal::Stopped { close_out: None, alert: None }));
        assert_eq!(close_out_verdict(&t.home, "paper", Some("r1")), V::Nothing);
        end("r1", Some(Terminal::Stopped { close_out: Some(CloseOut::Failed { left: 1, error: None }), alert: None }));
        assert_eq!(close_out_verdict(&t.home, "paper", Some("r1")), V::Failed);
        end("r1", Some(Terminal::Stopped { close_out: Some(CloseOut::Partial { closed: 1 }), alert: None }));
        assert_eq!(close_out_verdict(&t.home, "paper", Some("r1")), V::Partial);
        end("r1", Some(Terminal::Stopped { close_out: None, alert: Some("positions_not_closed".into()) }));
        assert_eq!(close_out_verdict(&t.home, "paper", Some("r1")), V::Failed);
        let unchecked = CloseOut::Unchecked { closed: 2, venue: "okx".into(), market: "linear".into() };
        end("r1", Some(Terminal::Stopped { close_out: Some(unchecked.clone()), alert: None }));
        assert_eq!(close_out_verdict(&t.home, "paper", Some("r1")), V::Unchecked);
        assert_eq!(close_out_doc(&t.home, "paper", Some("r1")), Some(unchecked.clone()));
        assert_eq!(close_out_doc(&t.home, "paper", Some("r2")), None, "another run_id");
        assert_eq!(close_out_doc(&t.home, "paper", None), None, "legacy");
        end("r1", Some(Terminal::Stopped { close_out: Some(unchecked), alert: Some("positions_not_closed".into()) }));
        assert_eq!(close_out_verdict(&t.home, "paper", Some("r1")), V::Failed, "an alert wins");
        end("r1", Some(Terminal::Stopped { close_out: Some(CloseOut::Unknown), alert: None }));
        assert_eq!(close_out_verdict(&t.home, "paper", Some("r1")), V::Failed, "a newer state fails safe");
        end("r1", Some(Terminal::Exited { code: 143, reason: "forced".into(), reason_key: None }));
        assert_eq!(close_out_verdict(&t.home, "paper", Some("r1")), V::Unknown);
        end("r1", None);
        assert_eq!(close_out_verdict(&t.home, "paper", Some("r1")), V::Unknown, "aborted while stopping");
        for (v, code, word) in [
            (V::Done, 0, "done"),
            (V::Nothing, 0, "none"),
            (V::Unconfirmed, 0, "unconfirmed"),
            (V::Unchecked, 0, "unchecked"),
            (V::Failed, 77, "failed"),
            (V::Partial, 77, "partial"),
            (V::Unknown, 77, "unknown"),
        ] {
            assert_eq!((v.exit_code(), v.word()), (code, word));
            assert_eq!(v.message().is_none(), matches!(v, V::Done | V::Nothing), "{v:?}");
        }
    }

    #[test]
    fn close_out_states_round_trip() {
        for c in [
            CloseOut::Done { closed: 3 },
            CloseOut::Failed { left: 1, error: Some("no network".into()) },
            CloseOut::Partial { closed: 1 },
            CloseOut::Unchecked { closed: 2, venue: "okx".into(), market: "linear".into() },
            CloseOut::Unknown,
        ] {
            let v = serde_json::to_value(&c).unwrap();
            assert_eq!(serde_json::from_value::<CloseOut>(v).unwrap(), c);
        }
        let v = serde_json::to_value(CloseOut::Unchecked { closed: 2, venue: "gate".into(), market: "spot".into() });
        assert_eq!(v.unwrap(), serde_json::json!({"state": "unchecked", "closed": 2, "venue": "gate", "market": "spot"}));
        let newer: CloseOut = serde_json::from_str(r#"{"state":"half_closed","closed":1,"why":"x"}"#).unwrap();
        assert_eq!(newer, CloseOut::Unknown);
        for r in [
            ResyncNote::Pending,
            ResyncNote::Done { fills: 2, cancels: 1 },
            ResyncNote::TimedOut,
            ResyncNote::NotNeeded,
        ] {
            let v = serde_json::to_value(&r).unwrap();
            assert_eq!(serde_json::from_value::<ResyncNote>(v).unwrap(), r);
        }
        assert_eq!(serde_json::to_value(ResyncNote::TimedOut).unwrap(), serde_json::json!({"state": "timed_out"}));
        assert_eq!(serde_json::from_str::<ResyncNote>(r#"{"state":"later"}"#).unwrap(), ResyncNote::Other);
        assert_eq!(serde_json::to_value(ReasonKey::PluginBlocked).unwrap(), "plugin_blocked");
        assert_eq!(serde_json::from_str::<ReasonKey>("\"plugin_blocked\"").unwrap(), ReasonKey::PluginBlocked);
    }

    #[test]
    fn the_phase_3_fields_round_trip() {
        let t = TempHome::new();
        t.bot("paper");
        let h = spawn(&t, "r2");
        let prev = PrevEnd { run_id: "r1".into(), step: Step::Stopping, at_ms: 42, crashed: false, live: Some(true) };
        let wake = WakeNote {
            from_ms: 1_000,
            to_ms: 2_821_000,
            slept_ms: 2_800_000,
            gap_ms: 2_820_000,
            resync: ResyncNote::Done { fills: 1, cancels: 0 },
        };
        let plugin =
            PluginNote { ty: "shot".into(), name: "shot".into(), sha: "0123456789abcdef".into(), file: "/l/x".into() };
        h.update(|d| {
            d.previous = Some(prev.clone());
            d.wake = Some(wake.clone());
            d.plugins = vec![plugin.clone()];
        });
        h.wait_idle();
        let d = read(&t.home, "paper").unwrap();
        assert_eq!((d.previous, d.wake, d.plugins), (Some(prev), Some(wake), vec![plugin]));
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(t.home.status_path("paper")).unwrap()).unwrap();
        assert_eq!(raw["previous"]["step"], "stopping");
        assert_eq!(raw["wake"]["resync"]["state"], "done");
        assert_eq!(raw["plugins"][0]["type"], "shot");
        // A file from before Phase 3 has none of them.
        let old = r#"{"v":1,"bot":"a","dir":"/x","run_id":"r","pid":1,"version":"0.2.8","mode":"service",
                      "step":"running","since_ms":0,"updated_ms":0}"#;
        let d: StatusDoc = serde_json::from_str(old).unwrap();
        assert_eq!((d.previous, d.wake, d.plugins), (None, None, vec![]));
        // A wake note without gap_ms, and a newer resync state.
        let note: WakeNote =
            serde_json::from_str(r#"{"from_ms":1,"to_ms":2,"slept_ms":3,"resync":{"state":"later"}}"#).unwrap();
        assert_eq!((note.gap_ms, note.resync), (0, ResyncNote::Other));
    }

    #[test]
    fn venue_names() {
        assert_eq!(venue_display("okx", "linear"), "OKX");
        assert_eq!(venue_display("hyperliquid", "inverse"), "Hyperliquid");
        assert_eq!(venue_display("gate", "spot"), "Gate Spot");
        assert_eq!(venue_display("custom", "linear"), "custom");
    }

    #[test]
    fn old_and_new_files_parse() {
        let minimal = r#"{"v":1,"bot":"a","dir":"/x","run_id":"r","pid":1,"version":"0.2.8","mode":"service",
                          "step":"some_future_step","since_ms":0,"updated_ms":0,"reason_key":"brand_new_key","extra":1}"#;
        let d: StatusDoc = serde_json::from_str(minimal).unwrap();
        assert_eq!(d.step, Step::Other);
        assert_eq!(d.reason_key, Some(ReasonKey::Unknown));
        assert!(d.notes.is_empty());
        for k in
            [ReasonKey::Server5xx, ReasonKey::Rate429, ReasonKey::IpBan418, ReasonKey::Clock1021, ReasonKey::Unknown]
        {
            assert_eq!(serde_json::to_value(k).unwrap(), k.as_str());
        }
        assert_eq!(serde_json::to_value(Step::AwaitingSetup).unwrap(), "awaiting_setup");
    }

    #[test]
    fn ready_steps() {
        for s in [Step::AwaitingSetup, Step::Exchange, Step::Strategies, Step::Parked] {
            assert!(s.is_ready(true) && !s.is_ready(false), "{s:?}");
        }
        for s in [Step::Running, Step::WaitingNetwork, Step::IpBanned, Step::Restarting] {
            assert!(s.is_ready(true) && s.is_ready(false), "{s:?}");
        }
        for s in [Step::Locked, Step::Config, Step::Port, Step::Plugin, Step::Stopping, Step::Stopped, Step::Exited] {
            assert!(!s.is_ready(true), "{s:?}");
        }
    }
}
