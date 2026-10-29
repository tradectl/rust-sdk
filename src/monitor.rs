//! Monitor frames and the broadcaster that fans them out to `/v1/stream`.
//!
//! v1 clients read one ordered channel of JSON strings, as before v0.2.8. v2
//! clients read two channels of [`WireFrame`]s (lossless events, lossy ticks)
//! and get a `State` built from the registries here on connect (design:
//! `docs/versions/v0.2.8-s9-design.md` section 2). Nothing is serialized for a
//! reader that is not connected.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::{broadcast, watch};

pub use crate::types::config::MonitorConfig;

/// Full strategy state snapshot, broadcast on every tick.
#[derive(serde::Serialize, Clone)]
pub struct MonitorTick {
    pub timestamp_ms: u64,
    pub strategy_name: String,
    pub mode: String,
    /// Market type the symbol trades on: `"linear" | "inverse" | "spot"`.
    /// Lets consumers (native lab) pick the right public data feed instead of
    /// guessing from the symbol shape (BNBUSDT is valid on spot *and* linear).
    pub market: String,
    pub symbol: String,
    /// Stable id of the strategy instance (matches `StrategyConfigDto::id`).
    /// `strategy_name` is not unique, so this is what tells same-named
    /// instances apart. Absent from older bots.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy_id: Option<String>,
    pub bid_price: f64,
    pub ask_price: f64,
    pub balance: f64,
    pub trade_count: usize,
    /// Price lines to render on the chart (provided by the strategy).
    pub price_lines: Vec<crate::strategy::PriceLine>,
    /// Strategy-specific state for the info panel.
    pub strategy_state: serde_json::Value,
}

/// Discrete order fill event.
#[derive(serde::Serialize, Clone)]
pub struct MonitorFill {
    pub timestamp_ms: u64,
    pub strategy_name: String,
    pub symbol: String,
    /// Stable id of the strategy instance — see [`MonitorTick::strategy_id`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy_id: Option<String>,
    pub side: String,
    pub price: f64,
    pub quantity: f64,
    pub fill_type: String,
    pub profit_pct: Option<f64>,
    pub profit_usd: Option<f64>,
    /// Which ExitOrder.id triggered this fill (for exit fills).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_id: Option<String>,
    /// Whether this was a partial fill.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_partial: bool,
    /// Whether this fill closed the position (net qty reached 0).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub position_closed: bool,
}

/// Shadow optimization summary, broadcast periodically.
#[derive(serde::Serialize, Clone)]
pub struct ShadowSummary {
    pub timestamp_ms: u64,
    pub strategy_name: String,
    pub symbol: String,
    pub window_secs: u64,
    pub results: Vec<ShadowTrialResult>,
    /// Detailed state for top-N variants (sorted by score descending).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<ShadowVariantDetail>,
}

/// Metrics for a single shadow variant.
#[derive(serde::Serialize, Clone)]
pub struct ShadowTrialResult {
    pub variant: String,
    pub trade_count: usize,
    pub pnl: f64,
    pub pnl_pct: f64,
    pub max_drawdown_pct: f64,
    pub score: f64,
    pub eligible: bool,
}

/// Detailed state for a single shadow variant.
#[derive(serde::Serialize, Clone)]
pub struct ShadowVariantDetail {
    pub variant: String,
    pub position: Option<ShadowPosition>,
    pub active_exits: Vec<ShadowExit>,
    pub pending_entry: Option<ShadowPendingEntry>,
    pub balance: f64,
    pub win_count: usize,
    pub loss_count: usize,
    pub avg_win_pct: f64,
    pub avg_loss_pct: f64,
    pub recent_trades: Vec<ShadowTrade>,
}

#[derive(serde::Serialize, Clone)]
pub struct ShadowPosition {
    pub side: String,
    pub avg_entry: f64,
    pub quantity: f64,
    pub entry_count: usize,
}

#[derive(serde::Serialize, Clone)]
pub struct ShadowExit {
    pub id: String,
    pub price: f64,
    pub kind: String,
}

#[derive(serde::Serialize, Clone)]
pub struct ShadowPendingEntry {
    pub side: String,
    pub price: f64,
    pub size: f64,
}

#[derive(serde::Serialize, Clone)]
pub struct ShadowTrade {
    pub entry_price: f64,
    pub exit_price: f64,
    pub pnl_pct: f64,
    pub side: String,
    pub exit_time: u64,
}

/// Settings-apply progress: a queued when-flat change landed on (or was
/// skipped for) one symbol. The Lab flips its per-symbol pending chips off
/// these frames instead of polling `GET /v1/config`.
#[derive(serde::Serialize, Clone)]
pub struct ConfigApplied {
    pub strategy: String,
    pub symbol: String,
    /// Config epoch at emit time (matches `GET /v1/config`).
    pub epoch: u64,
    /// The keys that changed on this symbol.
    pub keys: Vec<String>,
    /// `"lab"` | `"revert"`.
    pub origin: String,
}

/// A strategy instance's live run-state transition, pushed the instant the
/// runner records it (`ConfigAdmin::set_run_state` / `fail_if_running`, and on
/// every hold change) so the
/// Lab's strategy editor repaints its status the moment it changes, instead of
/// waiting for the next `GET /v1/config` poll. `id` is the stable strategy id
/// (matches `StrategyConfigDto::id`); `run_state` is the same lower-case wire
/// string as the config DTO (`starting` / `running` / `stopped` / `failed`),
/// and `start_error` carries the reason when `run_state == "failed"`.
///
/// `hold` says the instance is running but not opening anything new, and why.
/// It is a separate field, not a `run_state` string: the instance still
/// manages its positions, and a Lab that does not know the field keeps
/// showing what it showed before. Absent when nothing holds the instance, so
/// a frame without it clears a hold the previous frame carried.
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
pub struct RunStateFrame {
    pub id: String,
    pub run_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hold: Option<HoldFrame>,
}

/// Why a running instance is holding its entries.
///
/// One reason per frame, by precedence `bot_halt` > `pairs_halted` >
/// `paused`. A pause under a halt is not lost: `GET /v1/config`
/// `paused_symbols` always lists every pause, and a frame with the pause
/// follows when the halt lifts.
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
pub struct HoldFrame {
    /// `"bot_halt"` (the whole bot: max-loss cap, account-fatal error,
    /// Telegram `/stop`, resource or disk guard), `"paused"` (a manual or
    /// edge-decay pause of some of its symbols) or `"pairs_halted"` (some of
    /// its pairs stopped on exchange errors while others still trade).
    pub kind: String,
    pub reason: String,
    /// The symbols held: the paused or halted pairs. Empty for `bot_halt`,
    /// and for a whole-strategy pause while the strategy has no pair yet
    /// (`kind == "paused"` then means the whole strategy).
    pub symbols: Vec<String>,
    /// Epoch ms the hold began.
    pub since_ms: u64,
}

/// One symbol task's resting orders and open position: the `orders` and
/// `positions` a Tick carries in `strategy_state`, without the market data
/// and the strategy's own state. Sent as an `Orders` frame when they change,
/// and as part of an `OrdersSnapshot` and a `State`. Empty `orders` and
/// `positions` mean the task holds nothing now.
///
/// The fields after `positions` are additive (v0.2.8): a v1 Lab ignores them.
#[derive(serde::Serialize, Clone, Debug, Default, PartialEq)]
pub struct TaskOrders {
    /// The task's slot, set by the broadcaster. A kept slot keeps the number
    /// of the task that left it.
    #[serde(skip_serializing_if = "is_zero")]
    pub slot: u64,
    pub timestamp_ms: u64,
    pub strategy_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy_id: Option<String>,
    pub mode: String,
    pub market: String,
    pub symbol: String,
    /// Strategy plugin type, as `strategy_state.strat_type` in a Tick.
    pub strat_type: String,
    pub orders: Vec<serde_json::Value>,
    pub positions: Vec<serde_json::Value>,
    /// Broadcaster-wide counter, set by the broadcaster on every `Orders`
    /// frame and `OrderMove`. A Lab ignores a frame of a slot with a lower one.
    #[serde(skip_serializing_if = "is_zero")]
    pub rev: u64,
    /// `Some(false)` on a kept slot: no task manages these rows any more.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub managed: Option<bool>,
    /// Kept slots only: why and when the task ended.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended: Option<TaskEnd>,
    /// Kept slots only: `Some(false)` while carried rows wait for their venue
    /// check.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    /// The task's last book mid, while it holds something.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mid: Option<f64>,
    /// The venue time of `mid`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub book_ms: Option<u64>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl TaskOrders {
    pub fn is_empty(&self) -> bool {
        self.orders.is_empty() && self.positions.is_empty()
    }

    /// A live task's rows; `false` for a kept slot.
    pub fn is_managed(&self) -> bool {
        self.managed != Some(false)
    }

    /// The key a v1 Lab files these rows under.
    fn v1_key(&self) -> (&str, &str, Option<&str>) {
        (&self.strategy_name, &self.symbol, self.strategy_id.as_deref())
    }
}

/// Why and when a kept slot's task ended. `reason`: `stopped`, `removed`,
/// `restarted`, `failed`, `close_failed`, `previous_run`.
#[derive(serde::Serialize, Clone, Debug, Default, PartialEq)]
pub struct TaskEnd {
    pub reason: String,
    pub at_ms: u64,
}

/// What a task leaves behind when it ends holding something. `orders` are the
/// rows the Lab shows; the ids beside them are what the engine matches venue
/// updates against and are never sent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeptRecord {
    pub orders: TaskOrders,
    pub end: TaskEnd,
    /// Venue client ids of the kept exits.
    pub exit_ids: Vec<String>,
    /// Venue client ids of the kept entries.
    pub entry_ids: Vec<String>,
    /// Binance algo ids, where a kept exit is an algo order.
    pub algo_ids: Vec<String>,
}

/// Every task's [`TaskOrders`] that holds anything. Complete: a series not
/// listed holds no orders and no position. v1 only: one entry per key.
#[derive(serde::Serialize, Clone, Debug)]
pub struct OrdersSnapshot {
    pub timestamp_ms: u64,
    pub tasks: Vec<TaskOrders>,
}

/// The newest stream protocol this bot serves.
pub const STREAM_PROTO: u32 = 2;
/// The oldest stream protocol this bot still serves to a Lab that asks.
pub const STREAM_MIN_PROTO: u32 = 2;

/// What a v0.2.8 bot sends, as `Hello.caps`. A Lab ignores words it does not
/// know.
pub const STREAM_CAPS: &[&str] = &[
    "state", "bot", "series", "order_move", "slot", "watch", "px", "managed", "kept_check", "held", "mid",
    "changed_ms", "rev", "resync", "logs_level", "log_seq", "beat", "entries_gated", "spawn_epoch",
    "reason_args",
];

/// First frame of every v2 connection.
#[derive(serde::Serialize, Clone, Debug, Default, PartialEq)]
pub struct HelloFrame {
    pub proto: u32,
    pub min_proto: u32,
    /// `bot` or `watchdog`.
    pub role: String,
    /// `run`, or `gap` for the CLI's gap server between two runs.
    pub serving: String,
    pub run_id: String,
    /// 1 for the first run of this process, +1 per respawn.
    pub pipeline: u32,
    pub bot: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot_id: Option<String>,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub started_at_ms: u64,
    pub now_ms: u64,
    pub venue: String,
    pub market: String,
    /// `live`, `demo` or `testnet`.
    pub env: String,
    pub caps: Vec<String>,
}

impl HelloFrame {
    /// [`STREAM_CAPS`] as owned words.
    pub fn bot_caps() -> Vec<String> {
        STREAM_CAPS.iter().map(|c| c.to_string()).collect()
    }
}

/// A bot-side loss on one connection. `lost`: `events` or `logs`.
#[derive(serde::Serialize, Clone, Debug, Default, PartialEq)]
pub struct ResyncFrame {
    pub lost: String,
    /// `warn` when the pump lowered this connection's log level.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logs: Option<String>,
    /// `lost: logs`: the last `log_seq` sent before the loss.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<u64>,
}

/// The `book` counts of a `Bot` frame: live positions only (never paper).
#[derive(serde::Serialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BookCounts {
    /// Live positions the bot shows, managed and kept.
    pub positions: u32,
    /// Live positions in kept slots.
    pub unmanaged: u32,
    /// Live positions with `protection: "none"`.
    pub no_stop: u32,
    /// Live positions whose only stop is a virtual SL.
    pub virtual_sl: u32,
    /// Live positions from before this process that no kept row covers.
    pub kept_before: u32,
}

impl BookCounts {
    /// A count that is sent at once rather than coalesced went up.
    pub fn rose_from(&self, before: &BookCounts) -> bool {
        self.no_stop > before.no_stop
            || self.unmanaged > before.unmanaged
            || self.virtual_sl > before.virtual_sl
            || self.kept_before > before.kept_before
    }

    fn of(rows: &TaskOrders) -> BookCounts {
        if rows.mode == "paper" {
            return BookCounts::default();
        }
        let mut c = BookCounts::default();
        for p in &rows.positions {
            c.positions += 1;
            match p.get("protection").and_then(|v| v.as_str()) {
                Some("none") => c.no_stop += 1,
                Some("virtual_sl") => c.virtual_sl += 1,
                _ => {}
            }
        }
        if !rows.is_managed() {
            c.unmanaged = c.positions;
        }
        c
    }

    fn add(&mut self, o: &BookCounts) {
        self.positions += o.positions;
        self.unmanaged += o.unmanaged;
        self.no_stop += o.no_stop;
        self.virtual_sl += o.virtual_sl;
    }

    fn sub(&mut self, o: &BookCounts) {
        self.positions = self.positions.saturating_sub(o.positions);
        self.unmanaged = self.unmanaged.saturating_sub(o.unmanaged);
        self.no_stop = self.no_stop.saturating_sub(o.no_stop);
        self.virtual_sl = self.virtual_sl.saturating_sub(o.virtual_sl);
    }
}

/// One strategy instance in `State.runs`: the `RunState` frame body.
pub type RunRow = RunStateFrame;

/// What a task's series is, for `Series` frames and `State.series`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SeriesInfo {
    pub strategy_id: String,
    pub name: String,
    pub symbol: String,
    pub market: String,
    pub mode: String,
    pub strat_type: String,
}

/// `State.series`: one strategy instance with the symbols it runs on.
#[derive(serde::Serialize, Clone, Debug, Default, PartialEq)]
pub struct SeriesGroup {
    pub strategy_id: String,
    pub name: String,
    pub market: String,
    pub mode: String,
    pub strat_type: String,
    pub symbols: Vec<String>,
}

/// A task started (`op: start`) or ended (`op: end`, with `reason`).
#[derive(serde::Serialize, Clone, Debug, Default, PartialEq)]
pub struct SeriesFrame {
    pub op: String,
    pub slot: u64,
    pub strategy_id: String,
    pub name: String,
    pub symbol: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub market: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub mode: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub strat_type: String,
    /// `end` only: `stopped`, `removed`, `restarted`, `retired`, `failed`,
    /// `shutdown`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub at_ms: u64,
}

/// A chase edit the venue acked with the same client id: one row's new price
/// and size. v2 Full only.
#[derive(serde::Serialize, Clone, Debug, Default, PartialEq)]
pub struct OrderMove {
    pub slot: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy_id: Option<String>,
    pub strategy_name: String,
    pub symbol: String,
    pub id: String,
    pub price: f64,
    pub qty: f64,
    pub changed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mid: Option<f64>,
    /// Set by the broadcaster.
    pub rev: u64,
}

/// The wanted price of a chase the entry seam holds.
#[derive(serde::Serialize, Clone, Debug, Default, PartialEq)]
pub struct HeldChase {
    pub id: String,
    pub wanted: f64,
    pub since_ms: u64,
}

/// The v2 Tick: no orders, positions, balance or trade count (those come in
/// `Orders`, `OrderMove` and `State`). Sent as `{"type":"Tick",...}`.
#[derive(serde::Serialize, Clone)]
pub struct LeanTick {
    pub slot: u64,
    pub timestamp_ms: u64,
    pub strategy_name: String,
    pub mode: String,
    pub market: String,
    pub symbol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy_id: Option<String>,
    pub bid_price: f64,
    pub ask_price: f64,
    pub price_lines: Vec<crate::strategy::PriceLine>,
    pub strategy_state: serde_json::Value,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<HeldChase>,
}

/// A price only, for a symbol the Lab needs a price for but does not chart.
#[derive(serde::Serialize, Clone, Debug, Default, PartialEq)]
pub struct PxFrame {
    pub symbol: String,
    pub bid: f64,
    pub ask: f64,
    pub book_ms: u64,
}

/// Runner liveness, every 5 s, to v2 connections at both levels.
#[derive(serde::Serialize, Clone, Debug, Default, PartialEq)]
pub struct Beat {
    pub now_ms: u64,
    pub tasks: u32,
    /// Tasks whose loop has not passed for 10 s.
    pub late: u32,
    /// Tasks holding a live virtual SL with no book for 40 s.
    pub no_feed: u32,
    /// Unrealized PnL per quote or settle asset.
    pub upnl: std::collections::BTreeMap<String, f64>,
}

/// Tagged event envelope for JSON serialization.
///
/// `State` and `Bot` are not variants: the broadcaster joins them from JSON
/// it already holds ([`MonitorBroadcaster::state_snapshot`],
/// [`MonitorBroadcaster::publish_bot`]).
#[derive(serde::Serialize, Clone)]
#[serde(tag = "type")]
pub enum MonitorEvent {
    Tick(MonitorTick),
    Fill(MonitorFill),
    Shadow(ShadowSummary),
    Config(ConfigApplied),
    RunState(RunStateFrame),
    Orders(TaskOrders),
    OrdersSnapshot(OrdersSnapshot),
    Hello(HelloFrame),
    Resync(ResyncFrame),
    Series(SeriesFrame),
    OrderMove(OrderMove),
    #[serde(rename = "Tick")]
    LeanTick(LeanTick),
    Px(PxFrame),
    Beat(Beat),
}

/// Which v2 queue a frame goes to in the pump.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    /// In order, lossless (`Series`, `RunState`, `Config`, `Fill`, `Bot`).
    Fifo,
    /// Newest per key wins (`Orders` by slot, `OrderMove` by slot and id).
    Keyed,
    /// Newest per key wins, lossy (`Tick`, `Shadow`, `Px`, `Beat`).
    Mailbox,
}

/// Which v2 connections get a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Audience {
    /// Full and Events.
    All,
    /// Full only.
    Full,
}

/// One frame on a v2 channel, serialized once for every connection.
#[derive(Clone, Debug)]
pub struct WireFrame {
    pub lane: Lane,
    pub audience: Audience,
    /// The keyed or mailbox key (0 for `Fifo`).
    pub key: u64,
    /// The task slot the frame belongs to, 0 when none. The pump drops the
    /// waiting moves of a slot on its `Orders`, and a mailbox Tick older than
    /// the last order frame it sent for the slot.
    pub slot: u64,
    /// Broadcaster-wide publish number: the order frames were published in.
    pub pub_no: u64,
    /// For the `watch` and `prices` filters.
    pub symbol: Option<Arc<str>>,
    pub json: Arc<str>,
}

/// A v2 connection's receivers: `events` (Fifo and Keyed) and `ticks`
/// (Mailbox).
pub struct MonitorRx {
    pub events: broadcast::Receiver<WireFrame>,
    pub ticks: broadcast::Receiver<WireFrame>,
}

/// What one connection asks for. The pump keeps its current one and hands
/// the old and the new to [`MonitorBroadcaster::set_demand`], and the
/// default on drop.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Demand {
    /// A v1 connection at Full.
    pub legacy_full: bool,
    /// A v2 connection at Full.
    pub v2_full: bool,
    /// v2: the symbols it charts.
    pub watch: Vec<Arc<str>>,
    /// v2: the symbols it needs a price for.
    pub prices: Vec<Arc<str>>,
}

/// What a task builds for its symbol.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TickDemand {
    /// The full v1 Tick: a v1 Full client exists.
    pub legacy: bool,
    /// The lean v2 Tick: a v2 client watches the symbol.
    pub lean: bool,
    /// `Px`: a v2 client needs the symbol's price.
    pub px: bool,
}

/// Which parts a `State` carries. An absent part means "unchanged".
#[derive(Clone, Debug, Default)]
pub struct StateParts {
    /// `connect`, `lag`, `request`, `detail`, `gap`.
    pub reason: String,
    pub run_id: Option<String>,
    pub pipeline: Option<u32>,
    pub now_ms: u64,
    pub log_seq: Option<u64>,
    pub bot: bool,
    pub runs: bool,
    pub series: bool,
    pub tasks: bool,
}

impl StateParts {
    /// Every part: on connect, after a loss and on request.
    pub fn full(reason: &str, run_id: Option<String>, pipeline: Option<u32>, now_ms: u64, log_seq: Option<u64>) -> Self {
        StateParts {
            reason: reason.into(),
            run_id,
            pipeline,
            now_ms,
            log_seq,
            bot: true,
            runs: true,
            series: true,
            tasks: true,
        }
    }
}

/// Counts of what the broadcaster sent, for tests (`testhook`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MonitorStats {
    pub pub_no: u64,
    pub legacy_frames: u64,
    pub events_frames: u64,
    pub ticks_frames: u64,
    pub moves: u64,
    pub states: u64,
}

#[derive(Default)]
struct Stats {
    legacy_frames: AtomicU64,
    events_frames: AtomicU64,
    ticks_frames: AtomicU64,
    moves: AtomicU64,
    states: AtomicU64,
}

/// One slot in the task registry.
struct SlotEntry {
    rows: Arc<TaskOrders>,
    /// `rows` serialized, made when a frame is sent; `None` until then.
    body: Option<Arc<str>>,
    counts: BookCounts,
    /// Kept slots: the record's ids.
    kept: Option<KeptIds>,
}

#[derive(Clone)]
struct KeptIds {
    end: TaskEnd,
    exit_ids: Vec<String>,
    entry_ids: Vec<String>,
    algo_ids: Vec<String>,
}

#[derive(Default)]
struct Tasks {
    by_slot: HashMap<u64, SlotEntry>,
    totals: BookCounts,
}

impl Tasks {
    /// What a v1 Lab should show for `key`: a live task's rows, else a kept
    /// slot's (live wins), leaving out `skip`.
    fn v1_view(&self, key: (&str, &str, Option<&str>), skip: u64) -> Option<Arc<TaskOrders>> {
        let mut kept = None;
        for (slot, e) in &self.by_slot {
            if *slot == skip || e.rows.v1_key() != key {
                continue;
            }
            if e.rows.is_managed() {
                return Some(e.rows.clone());
            }
            kept.get_or_insert_with(|| e.rows.clone());
        }
        kept
    }

    fn set(&mut self, slot: u64, entry: Option<SlotEntry>) {
        if let Some(old) = self.by_slot.remove(&slot) {
            self.totals.sub(&old.counts);
        }
        if let Some(e) = entry {
            self.totals.add(&e.counts);
            self.by_slot.insert(slot, e);
        }
    }
}

#[derive(Default)]
struct DemandCounts {
    watch: HashMap<Arc<str>, u32>,
    prices: HashMap<Arc<str>, u32>,
}

/// Fans monitor events out to subscribers and keeps what a new connection is
/// sent first.
///
/// Channel-only by design: there is no standalone listener. The bot API
/// (`tradectl-bot-api`) serves these frames over its authed TLS
/// `wss://…/v1/stream` route.
///
/// Three channels. `legacy` is the v1 stream, unchanged, fed only while a v1
/// client holds a receiver. `events` (lossless frames) and `ticks` (lossy
/// frames) carry [`WireFrame`]s to v2 connections. Registries keep the tasks
/// by slot (rows, cached JSON, kept slots, book counts), the series, the
/// strategy runs and the bot level, so a `State` is built without asking the
/// runner.
pub struct MonitorBroadcaster {
    tx: broadcast::Sender<String>,
    events: broadcast::Sender<WireFrame>,
    ticks: broadcast::Sender<WireFrame>,
    tasks: Mutex<Tasks>,
    series: Mutex<HashMap<u64, SeriesInfo>>,
    runs: Mutex<BTreeMap<String, RunStateFrame>>,
    bot: Mutex<Option<Arc<str>>>,
    book: watch::Sender<BookCounts>,
    next_slot: AtomicU64,
    rev: AtomicU64,
    pub_no: AtomicU64,
    legacy_full: AtomicU32,
    v2_full: AtomicU32,
    demand: RwLock<DemandCounts>,
    demand_gen: AtomicU64,
    stats: Stats,
}

impl Default for MonitorBroadcaster {
    fn default() -> Self {
        Self::new()
    }
}

/// The v1 channel's depth. A client that falls further behind gets an
/// `OrdersSnapshot` after the lag.
pub const MONITOR_CHANNEL_CAPACITY: usize = 1024;
/// The v2 `events` channel's depth.
pub const EVENTS_CHANNEL_CAPACITY: usize = 1024;
/// The v2 `ticks` channel's depth.
pub const TICKS_CHANNEL_CAPACITY: usize = 256;

fn key_of(parts: impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    parts.hash(&mut h);
    h.finish()
}

/// `{"type":"<ty>",` + the rest of `body`, an object with at least one field.
fn splice(ty: &str, body: &str) -> Arc<str> {
    format!("{{\"type\":\"{ty}\",{}", &body[1..]).into()
}

struct Route {
    lane: Lane,
    audience: Audience,
    key: u64,
    slot: u64,
    symbol: Option<Arc<str>>,
}

impl Route {
    fn fifo() -> Route {
        Route { lane: Lane::Fifo, audience: Audience::All, key: 0, slot: 0, symbol: None }
    }

    fn orders(slot: u64, symbol: &str) -> Route {
        Route { lane: Lane::Keyed, audience: Audience::Full, key: key_of(("Orders", slot)), slot, symbol: Some(symbol.into()) }
    }
}

impl MonitorBroadcaster {
    /// Create a broadcaster backed by bounded fan-out channels. No socket is
    /// bound; consumers obtain receivers via [`subscribe`](Self::subscribe)
    /// (v1) and [`subscribe_v2`](Self::subscribe_v2).
    pub fn new() -> Self {
        Self::with_capacities(EVENTS_CHANNEL_CAPACITY, TICKS_CHANNEL_CAPACITY)
    }

    /// A broadcaster with other v2 channel depths, so a test can make a lane
    /// overflow.
    #[cfg(any(test, feature = "testhook"))]
    pub fn with_lane_capacities(events: usize, ticks: usize) -> Self {
        Self::with_capacities(events, ticks)
    }

    fn with_capacities(events: usize, ticks: usize) -> Self {
        let (tx, _) = broadcast::channel::<String>(MONITOR_CHANNEL_CAPACITY);
        let (events, _) = broadcast::channel::<WireFrame>(events);
        let (ticks, _) = broadcast::channel::<WireFrame>(ticks);
        let (book, _) = watch::channel(BookCounts::default());
        Self {
            tx,
            events,
            ticks,
            tasks: Default::default(),
            series: Default::default(),
            runs: Default::default(),
            bot: Default::default(),
            book,
            next_slot: AtomicU64::new(1),
            rev: AtomicU64::new(0),
            pub_no: AtomicU64::new(0),
            legacy_full: AtomicU32::new(0),
            v2_full: AtomicU32::new(0),
            demand: Default::default(),
            demand_gen: AtomicU64::new(0),
            stats: Stats::default(),
        }
    }

    /// A key for one task's orders, unique for this broadcaster.
    pub fn orders_slot(&self) -> u64 {
        self.next_slot.fetch_add(1, Ordering::Relaxed)
    }

    fn next_rev(&self) -> u64 {
        self.rev.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn v1_on(&self) -> bool {
        self.tx.receiver_count() > 0
    }

    fn lane_tx(&self, lane: Lane) -> &broadcast::Sender<WireFrame> {
        match lane {
            Lane::Mailbox => &self.ticks,
            Lane::Fifo | Lane::Keyed => &self.events,
        }
    }

    fn v2_on(&self, lane: Lane) -> bool {
        self.lane_tx(lane).receiver_count() > 0
    }

    /// Send one frame: `legacy` to v1 clients, `v2` to the v2 lane it names.
    /// One publish number for both.
    fn emit(&self, legacy: Option<&str>, v2: Option<(Route, Arc<str>)>) {
        if legacy.is_none() && v2.is_none() {
            return;
        }
        let pub_no = self.pub_no.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(json) = legacy {
            if self.tx.send(json.to_string()).is_ok() {
                self.stats.legacy_frames.fetch_add(1, Ordering::Relaxed);
            }
        }
        if let Some((r, json)) = v2 {
            let lane = r.lane;
            let frame = WireFrame { lane, audience: r.audience, key: r.key, slot: r.slot, pub_no, symbol: r.symbol, json };
            if self.lane_tx(lane).send(frame).is_ok() {
                let n = if lane == Lane::Mailbox { &self.stats.ticks_frames } else { &self.stats.events_frames };
                n.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Record a live task's current orders and position, and send them as an
    /// `Orders` frame. An empty one removes the task from the registry; a v1
    /// client then gets a kept slot of the same key, if one is left.
    pub fn publish_orders(&self, slot: u64, mut orders: TaskOrders) {
        orders.slot = slot;
        orders.rev = self.next_rev();
        orders.managed = None;
        orders.ended = None;
        let rows = Arc::new(orders);
        let (v1_on, v2_on) = (self.v1_on(), self.v2_on(Lane::Keyed));
        let body: Option<Arc<str>> =
            (v1_on || v2_on).then(|| serde_json::to_string(&*rows).unwrap_or_default().into());
        let v1_rows = {
            let mut t = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
            if rows.is_empty() {
                t.set(slot, None);
            } else {
                let counts = BookCounts::of(&rows);
                t.set(slot, Some(SlotEntry { rows: rows.clone(), body: body.clone(), counts, kept: None }));
            }
            self.book_changed(&t);
            (v1_on && rows.is_empty()).then(|| t.v1_view(rows.v1_key(), slot)).flatten()
        };
        let Some(body) = body else { return };
        let frame = splice("Orders", &body);
        let legacy = v1_on.then(|| match &v1_rows {
            Some(r) => frame_of(r),
            None => frame.clone(),
        });
        self.emit(legacy.as_deref(), v2_on.then(|| (Route::orders(slot, &rows.symbol), frame)));
    }

    /// The task ended holding nothing on the venue: drop its slot and send an
    /// empty `Orders` frame for it. A v1 client gets the rows of a live task
    /// of the same key (a respawned one) or a kept slot of it instead. No-op
    /// if the task held nothing.
    pub fn clear_orders(&self, slot: u64, timestamp_ms: u64) {
        let (empty, v1_rows) = {
            let mut t = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
            let Some(old) = t.by_slot.get(&slot).map(|e| e.rows.clone()) else { return };
            t.set(slot, None);
            self.book_changed(&t);
            let empty = TaskOrders {
                slot,
                timestamp_ms,
                strategy_name: old.strategy_name.clone(),
                strategy_id: old.strategy_id.clone(),
                mode: old.mode.clone(),
                market: old.market.clone(),
                symbol: old.symbol.clone(),
                strat_type: old.strat_type.clone(),
                rev: self.next_rev(),
                ..Default::default()
            };
            let v1 = t.v1_view(old.v1_key(), slot);
            (empty, v1)
        };
        let (v1_on, v2_on) = (self.v1_on(), self.v2_on(Lane::Keyed));
        if !v1_on && !v2_on {
            return;
        }
        let frame = frame_of(&empty);
        let legacy = v1_on.then(|| v1_rows.as_ref().map_or_else(|| frame.clone(), |r| frame_of(r)));
        self.emit(legacy.as_deref(), v2_on.then(|| (Route::orders(slot, &empty.symbol), frame)));
    }

    /// The task ended leaving something on the venue: its slot stays, with
    /// `managed: false` and the end reason, until the engine clears it. A
    /// record with no rows is [`clear_orders`](Self::clear_orders). Called
    /// again with a changed record (a kept exit filled or was cancelled), it
    /// replaces the slot's rows.
    ///
    /// A v1 client gets the kept rows only while no live task has the key.
    pub fn keep_orders(&self, slot: u64, record: KeptRecord) {
        if record.orders.is_empty() {
            self.clear_orders(slot, record.end.at_ms);
            return;
        }
        let KeptRecord { mut orders, end, exit_ids, entry_ids, algo_ids } = record;
        orders.slot = slot;
        orders.rev = self.next_rev();
        orders.managed = Some(false);
        orders.ended = Some(end.clone());
        let rows = Arc::new(orders);
        let (v1_on, v2_on) = (self.v1_on(), self.v2_on(Lane::Keyed));
        let body: Option<Arc<str>> =
            (v1_on || v2_on).then(|| serde_json::to_string(&*rows).unwrap_or_default().into());
        let live = {
            let mut t = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
            let counts = BookCounts::of(&rows);
            let kept = KeptIds { end, exit_ids, entry_ids, algo_ids };
            t.set(slot, Some(SlotEntry { rows: rows.clone(), body: body.clone(), counts, kept: Some(kept) }));
            self.book_changed(&t);
            t.v1_view(rows.v1_key(), slot).filter(|r| r.is_managed())
        };
        let Some(body) = body else { return };
        let frame = splice("Orders", &body);
        let legacy = v1_on.then(|| live.as_ref().map_or_else(|| frame.clone(), |r| frame_of(r)));
        self.emit(legacy.as_deref(), v2_on.then(|| (Route::orders(slot, &rows.symbol), frame)));
    }

    /// Every kept slot as its record, for the CLI to carry over a respawn.
    pub fn kept_slots(&self) -> Vec<KeptRecord> {
        let t = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<KeptRecord> = t
            .by_slot
            .values()
            .filter_map(|e| {
                let k = e.kept.as_ref()?;
                Some(KeptRecord {
                    orders: (*e.rows).clone(),
                    end: k.end.clone(),
                    exit_ids: k.exit_ids.clone(),
                    entry_ids: k.entry_ids.clone(),
                    algo_ids: k.algo_ids.clone(),
                })
            })
            .collect();
        out.sort_by_key(|r| r.orders.slot);
        out
    }

    /// A chase edit acked with the same client id: update that row in the
    /// registry (with or without clients), then send an `OrderMove` to v2
    /// Full clients. `false` when the slot or the row is not there; the caller
    /// then sends the rows as an `Orders` frame.
    pub fn publish_move(&self, slot: u64, mut mv: OrderMove) -> bool {
        {
            let mut t = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
            let Some(e) = t.by_slot.get_mut(&slot) else { return false };
            let Some(i) = e.rows.orders.iter().position(|o| o.get("id").and_then(|v| v.as_str()) == Some(mv.id.as_str()))
            else {
                return false;
            };
            let mut rows = (*e.rows).clone();
            let row = &mut rows.orders[i];
            row["price"] = serde_json::json!(mv.price);
            row["qty"] = serde_json::json!(mv.qty);
            row["changed_ms"] = serde_json::json!(mv.changed_ms);
            if mv.mid.is_some() {
                rows.mid = mv.mid;
            }
            mv.rev = self.next_rev();
            rows.rev = mv.rev;
            e.rows = Arc::new(rows);
            e.body = None;
        }
        mv.slot = slot;
        if self.v2_full.load(Ordering::Relaxed) > 0 && self.v2_on(Lane::Keyed) {
            let route = Route {
                lane: Lane::Keyed,
                audience: Audience::Full,
                key: key_of(("OrderMove", slot, mv.id.as_str())),
                slot,
                symbol: Some(mv.symbol.as_str().into()),
            };
            if let Ok(json) = serde_json::to_string(&MonitorEvent::OrderMove(mv)) {
                self.emit(None, Some((route, json.into())));
                self.stats.moves.fetch_add(1, Ordering::Relaxed);
            }
        }
        true
    }

    /// The registry's book counts changed: let the bot-level publisher know.
    /// Under the tasks lock, so two changes never land out of order.
    fn book_changed(&self, t: &Tasks) {
        let now = t.totals;
        self.book.send_if_modified(|b| {
            let changed = b.positions != now.positions
                || b.unmanaged != now.unmanaged
                || b.no_stop != now.no_stop
                || b.virtual_sl != now.virtual_sl;
            if changed {
                let kept_before = b.kept_before;
                *b = BookCounts { kept_before, ..now };
            }
            changed
        });
    }

    /// The five totals, kept by diff.
    pub fn book_counts(&self) -> BookCounts {
        *self.book.borrow()
    }

    /// Waits on the book counts (the bot-level publisher). Send at once when
    /// [`BookCounts::rose_from`] says so, else coalesce.
    pub fn subscribe_book(&self) -> watch::Receiver<BookCounts> {
        self.book.subscribe()
    }

    /// Live positions from before this process that no kept row covers.
    pub fn set_kept_before(&self, n: u32) {
        self.book.send_if_modified(|b| {
            let changed = b.kept_before != n;
            b.kept_before = n;
            changed
        });
    }

    /// A task started: record its series and send `Series` `start`.
    pub fn series_start(&self, slot: u64, info: SeriesInfo, at_ms: u64) {
        let frame = SeriesFrame {
            op: "start".into(),
            slot,
            strategy_id: info.strategy_id.clone(),
            name: info.name.clone(),
            symbol: info.symbol.clone(),
            market: info.market.clone(),
            mode: info.mode.clone(),
            strat_type: info.strat_type.clone(),
            reason: None,
            at_ms,
        };
        self.series.lock().unwrap_or_else(|e| e.into_inner()).insert(slot, info);
        self.send_fifo(&MonitorEvent::Series(frame));
    }

    /// A task ended: drop its series and send `Series` `end` with `reason`.
    /// No-op for a slot that never started.
    pub fn series_end(&self, slot: u64, reason: &str, at_ms: u64) {
        let Some(info) = self.series.lock().unwrap_or_else(|e| e.into_inner()).remove(&slot) else { return };
        self.send_fifo(&MonitorEvent::Series(SeriesFrame {
            op: "end".into(),
            slot,
            strategy_id: info.strategy_id,
            name: info.name,
            symbol: info.symbol,
            reason: Some(reason.into()),
            at_ms,
            ..Default::default()
        }));
    }

    /// Every live task, one row per strategy instance with its symbols.
    pub fn series_groups(&self) -> Vec<SeriesGroup> {
        let series = self.series.lock().unwrap_or_else(|e| e.into_inner());
        let mut groups: BTreeMap<(&str, &str), SeriesGroup> = BTreeMap::new();
        for s in series.values() {
            let g = groups.entry((s.strategy_id.as_str(), s.name.as_str())).or_insert_with(|| SeriesGroup {
                strategy_id: s.strategy_id.clone(),
                name: s.name.clone(),
                market: s.market.clone(),
                mode: s.mode.clone(),
                strat_type: s.strat_type.clone(),
                symbols: Vec::new(),
            });
            g.symbols.push(s.symbol.clone());
        }
        groups
            .into_values()
            .map(|mut g| {
                g.symbols.sort();
                g
            })
            .collect()
    }

    /// Record an instance's run state and send it as a `RunState` frame.
    /// [`broadcast`](Self::broadcast) of a `RunState` does the same.
    pub fn set_run(&self, frame: RunStateFrame) {
        self.broadcast(&MonitorEvent::RunState(frame));
    }

    /// The instance was removed from the config.
    pub fn remove_run(&self, id: &str) {
        self.runs.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
    }

    /// Send the bot level (a `BotLevelView` serialized by the engine) as a
    /// `Bot` frame to v2 clients. Dropped when equal to the last one; `true`
    /// when it changed.
    pub fn publish_bot(&self, json: Arc<str>) -> bool {
        {
            let mut bot = self.bot.lock().unwrap_or_else(|e| e.into_inner());
            if bot.as_deref() == Some(&*json) {
                return false;
            }
            *bot = Some(json.clone());
        }
        if self.v2_on(Lane::Fifo) && json.len() > 2 {
            self.emit(None, Some((Route::fifo(), splice("Bot", &json))));
        }
        true
    }

    /// The last `Bot` body, for `/v1/status` and the contract test.
    pub fn bot_level(&self) -> Option<Arc<str>> {
        self.bot.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// A `State` frame (without `seq`; the pump splices it in). The registry
    /// `Arc`s are copied under the locks; the JSON is built outside them, so
    /// no publishing task waits on a `State`.
    pub fn state_snapshot(&self, parts: &StateParts) -> String {
        let bot = if parts.bot { self.bot_level() } else { None };
        let runs: Option<Vec<RunStateFrame>> =
            parts.runs.then(|| self.runs.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect());
        let series = parts.series.then(|| self.series_groups());
        let tasks: Option<Vec<(Arc<TaskOrders>, Option<Arc<str>>)>> = parts.tasks.then(|| {
            let t = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
            let mut v: Vec<_> = t.by_slot.iter().map(|(s, e)| (*s, e.rows.clone(), e.body.clone())).collect();
            drop(t);
            v.sort_by_key(|(s, _, _)| *s);
            v.into_iter().map(|(_, r, b)| (r, b)).collect()
        });

        fn js<T: serde::Serialize + ?Sized>(v: &T) -> String {
            serde_json::to_string(v).unwrap_or_default()
        }
        let mut out = String::with_capacity(256 + tasks.as_ref().map_or(0, |t| t.len() * 512));
        out.push_str("{\"type\":\"State\",\"reason\":");
        out.push_str(&js(&parts.reason));
        if let Some(run_id) = &parts.run_id {
            out.push_str(",\"run_id\":");
            out.push_str(&js(run_id));
        }
        if let Some(p) = parts.pipeline {
            out.push_str(&format!(",\"pipeline\":{p}"));
        }
        out.push_str(&format!(",\"now_ms\":{}", parts.now_ms));
        if let Some(bot) = bot.filter(|b| !b.is_empty()) {
            out.push_str(",\"bot\":");
            out.push_str(&bot);
        }
        if let Some(runs) = runs {
            out.push_str(",\"runs\":");
            out.push_str(&js(&runs));
        }
        if let Some(series) = series {
            out.push_str(",\"series\":");
            out.push_str(&js(&series));
        }
        if let Some(tasks) = tasks {
            out.push_str(",\"tasks\":[");
            for (i, (rows, body)) in tasks.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                match body {
                    Some(b) => out.push_str(b),
                    None => out.push_str(&serde_json::to_string(&**rows).unwrap_or_default()),
                }
            }
            out.push(']');
        }
        if let Some(n) = parts.log_seq {
            out.push_str(&format!(",\"log_seq\":{n}"));
        }
        out.push('}');
        self.stats.states.fetch_add(1, Ordering::Relaxed);
        out
    }

    /// Every task's latest orders and position, as a v1 `OrdersSnapshot`
    /// frame: one entry per key, a live task before a kept slot. Subscribe
    /// first, then send this: a change made in between reaches the client
    /// twice rather than not at all.
    pub fn orders_snapshot(&self, timestamp_ms: u64) -> String {
        let rows: Vec<Arc<TaskOrders>> = {
            let t = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
            let mut by_key: HashMap<(&str, &str, Option<&str>), &Arc<TaskOrders>> = HashMap::new();
            let mut slots: Vec<_> = t.by_slot.iter().collect();
            slots.sort_by_key(|(s, _)| **s);
            for (_, e) in slots {
                let r = &e.rows;
                by_key
                    .entry(r.v1_key())
                    .and_modify(|cur| {
                        if !cur.is_managed() && r.is_managed() {
                            *cur = r;
                        }
                    })
                    .or_insert(r);
            }
            by_key.into_values().cloned().collect()
        };
        let mut tasks: Vec<TaskOrders> = rows.iter().map(|r| (**r).clone()).collect();
        tasks.sort_by(|a, b| (&a.strategy_name, &a.symbol).cmp(&(&b.strategy_name, &b.symbol)));
        let event = MonitorEvent::OrdersSnapshot(OrdersSnapshot { timestamp_ms, tasks });
        serde_json::to_string(&event).unwrap_or_default()
    }

    /// Subscribe a v1 receiver (today's stream). Each connected v1 client
    /// holds one; the legacy channel is fed only while one exists.
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }

    /// Subscribe a v2 connection's two receivers.
    pub fn subscribe_v2(&self) -> MonitorRx {
        MonitorRx { events: self.events.subscribe(), ticks: self.ticks.subscribe() }
    }

    /// Returns `true` if at least one subscriber, v1 or v2, is connected.
    pub fn has_clients(&self) -> bool {
        self.tx.receiver_count() > 0 || self.events.receiver_count() > 0 || self.ticks.receiver_count() > 0
    }

    /// A v1 or v2 connection at Full exists: order frames are owed.
    pub fn full_clients(&self) -> bool {
        self.legacy_full.load(Ordering::Relaxed) + self.v2_full.load(Ordering::Relaxed) > 0
    }

    /// v2 connections at Full.
    pub fn v2_full_clients(&self) -> u32 {
        self.v2_full.load(Ordering::Relaxed)
    }

    /// Bumped on every demand change. A task reads [`tick_demand`]
    /// (Self::tick_demand) again only when this moved.
    pub fn demand_gen(&self) -> u64 {
        self.demand_gen.load(Ordering::Acquire)
    }

    /// One connection's demand went from `old` to `new` (to the default when
    /// it ends).
    pub fn set_demand(&self, old: &Demand, new: &Demand) {
        if old == new {
            return;
        }
        let step = |n: &AtomicU32, was: bool, is: bool| match (was, is) {
            (false, true) => {
                n.fetch_add(1, Ordering::Relaxed);
            }
            (true, false) => {
                let _ = n.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| Some(v.saturating_sub(1)));
            }
            _ => {}
        };
        step(&self.legacy_full, old.legacy_full, new.legacy_full);
        step(&self.v2_full, old.v2_full, new.v2_full);
        {
            let mut d = self.demand.write().unwrap_or_else(|e| e.into_inner());
            let shift = |map: &mut HashMap<Arc<str>, u32>, gone: &[Arc<str>], came: &[Arc<str>]| {
                for s in gone {
                    if let Some(n) = map.get_mut(s) {
                        *n -= 1;
                        if *n == 0 {
                            map.remove(s);
                        }
                    }
                }
                for s in came {
                    *map.entry(s.clone()).or_insert(0) += 1;
                }
            };
            shift(&mut d.watch, &old.watch, &new.watch);
            shift(&mut d.prices, &old.prices, &new.prices);
        }
        self.demand_gen.fetch_add(1, Ordering::Release);
    }

    /// What a task on `symbol` builds: the full v1 Tick, the lean v2 Tick, a
    /// `Px`. All `false`: no Tick, no fingerprint, no build.
    pub fn tick_demand(&self, symbol: &str) -> TickDemand {
        let d = self.demand.read().unwrap_or_else(|e| e.into_inner());
        TickDemand {
            legacy: self.legacy_full.load(Ordering::Relaxed) > 0,
            lean: d.watch.contains_key(symbol),
            px: d.prices.contains_key(symbol),
        }
    }

    /// The newest publish number.
    pub fn last_pub_no(&self) -> u64 {
        self.pub_no.load(Ordering::Relaxed)
    }

    /// What was sent so far.
    #[cfg(any(test, feature = "testhook"))]
    pub fn stats(&self) -> MonitorStats {
        MonitorStats {
            pub_no: self.pub_no.load(Ordering::Relaxed),
            legacy_frames: self.stats.legacy_frames.load(Ordering::Relaxed),
            events_frames: self.stats.events_frames.load(Ordering::Relaxed),
            ticks_frames: self.stats.ticks_frames.load(Ordering::Relaxed),
            moves: self.stats.moves.load(Ordering::Relaxed),
            states: self.stats.states.load(Ordering::Relaxed),
        }
    }

    fn send_fifo(&self, event: &MonitorEvent) {
        if !self.v2_on(Lane::Fifo) {
            return;
        }
        if let Ok(json) = serde_json::to_string(event) {
            self.emit(None, Some((Route::fifo(), json.into())));
        }
    }

    /// Broadcast an event to the clients that read it: the full Tick and
    /// `OrdersSnapshot` to v1 only; the lean Tick, `Px`, `Beat`, `Series` and
    /// `OrderMove` to v2 only; the rest to both. A `RunState` is also recorded
    /// for `State.runs`, and an `Orders` frame goes by its `slot` (use
    /// [`publish_orders`](Self::publish_orders) to keep the registry). No-op
    /// for a reader that is not connected.
    pub fn broadcast(&self, event: &MonitorEvent) {
        if let MonitorEvent::RunState(f) = event {
            self.runs.lock().unwrap_or_else(|e| e.into_inner()).insert(f.id.clone(), f.clone());
        }
        let mailbox = |key: u64, slot: u64, symbol: &str, audience: Audience| Route {
            lane: Lane::Mailbox,
            audience,
            key,
            slot,
            symbol: Some(symbol.into()),
        };
        let (legacy, route) = match event {
            MonitorEvent::Tick(_) | MonitorEvent::OrdersSnapshot(_) => (true, None),
            MonitorEvent::Fill(_) | MonitorEvent::Config(_) | MonitorEvent::RunState(_) => (true, Some(Route::fifo())),
            MonitorEvent::Shadow(s) => (
                true,
                Some(mailbox(key_of(("Shadow", &s.strategy_name, &s.symbol)), 0, &s.symbol, Audience::Full)),
            ),
            MonitorEvent::Orders(o) => (true, Some(Route::orders(o.slot, &o.symbol))),
            // Per connection: the pump sends them itself.
            MonitorEvent::Hello(_) | MonitorEvent::Resync(_) => return,
            MonitorEvent::Series(_) => (false, Some(Route::fifo())),
            MonitorEvent::OrderMove(m) => (
                false,
                Some(Route {
                    lane: Lane::Keyed,
                    audience: Audience::Full,
                    key: key_of(("OrderMove", m.slot, m.id.as_str())),
                    slot: m.slot,
                    symbol: Some(m.symbol.as_str().into()),
                }),
            ),
            MonitorEvent::LeanTick(t) => {
                (false, Some(mailbox(key_of(("Tick", t.slot)), t.slot, &t.symbol, Audience::Full)))
            }
            MonitorEvent::Px(p) => (false, Some(mailbox(key_of(("Px", p.symbol.as_str())), 0, &p.symbol, Audience::Full))),
            MonitorEvent::Beat(_) => (
                false,
                Some(Route { lane: Lane::Mailbox, audience: Audience::All, key: key_of("Beat"), slot: 0, symbol: None }),
            ),
        };
        let legacy = legacy && self.v1_on();
        let route = route.filter(|r| self.v2_on(r.lane));
        if !legacy && route.is_none() {
            return;
        }
        let Ok(json) = serde_json::to_string(event) else { return };
        let json: Arc<str> = json.into();
        self.emit(legacy.then_some(&*json), route.map(|r| (r, json.clone())));
    }
}

/// An `Orders` frame of `rows`.
fn frame_of(rows: &TaskOrders) -> Arc<str> {
    splice("Orders", &serde_json::to_string(rows).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(strategy_id: Option<String>) -> MonitorFill {
        MonitorFill {
            timestamp_ms: 1,
            strategy_name: "shot".into(),
            symbol: "BTCUSDT".into(),
            strategy_id,
            side: "BUY".into(),
            price: 1.0,
            quantity: 1.0,
            fill_type: "entry".into(),
            profit_pct: None,
            profit_usd: None,
            exit_id: None,
            is_partial: false,
            position_closed: false,
        }
    }

    /// An older Lab decodes these frames with no `strategy_id` field, so the
    /// key is left out rather than sent as `null` when there is no id.
    #[test]
    fn strategy_id_is_sent_only_when_known() {
        let v = serde_json::to_value(MonitorEvent::Fill(fill(Some("st_a".into())))).unwrap();
        assert_eq!(v["strategy_id"], "st_a");
        let v = serde_json::to_value(MonitorEvent::Fill(fill(None))).unwrap();
        assert!(v.get("strategy_id").is_none());
    }

    fn task(symbol: &str, orders: usize) -> TaskOrders {
        TaskOrders {
            timestamp_ms: 1,
            strategy_name: "shot".into(),
            strategy_id: Some("st_a".into()),
            mode: "paper".into(),
            market: "linear".into(),
            symbol: symbol.into(),
            strat_type: "shot".into(),
            orders: (0..orders).map(|i| serde_json::json!({ "id": format!("o{i}"), "price": 1.0, "qty": 1.0 })).collect(),
            ..Default::default()
        }
    }

    /// A live task on `symbol` holding one position with `protection`.
    fn holding(symbol: &str, protection: &str) -> TaskOrders {
        TaskOrders {
            mode: "live".into(),
            positions: vec![serde_json::json!({ "side": "Long", "quantity": 1.0, "protection": protection })],
            ..task(symbol, 1)
        }
    }

    fn kept(orders: TaskOrders, reason: &str) -> KeptRecord {
        KeptRecord {
            orders,
            end: TaskEnd { reason: reason.into(), at_ms: 5 },
            exit_ids: vec!["x-tp".into()],
            ..Default::default()
        }
    }

    fn snapshot_tasks(m: &MonitorBroadcaster) -> Vec<serde_json::Value> {
        let v: serde_json::Value = serde_json::from_str(&m.orders_snapshot(9)).unwrap();
        assert_eq!(v["type"], "OrdersSnapshot");
        assert_eq!(v["timestamp_ms"], 9);
        v["tasks"].as_array().unwrap().clone()
    }

    fn json(s: &str) -> serde_json::Value {
        serde_json::from_str(s).unwrap()
    }

    fn state(m: &MonitorBroadcaster) -> serde_json::Value {
        json(&m.state_snapshot(&StateParts::full("connect", Some("r1".into()), Some(1), 7, Some(3))))
    }

    fn full_v2() -> Demand {
        Demand { v2_full: true, ..Default::default() }
    }

    /// The snapshot holds each task's latest orders, and only tasks that hold
    /// something. It is kept with no client connected.
    #[test]
    fn the_snapshot_keeps_each_tasks_latest_orders() {
        let m = MonitorBroadcaster::new();
        let (a, b) = (m.orders_slot(), m.orders_slot());
        assert_ne!(a, b);
        m.publish_orders(a, task("BTCUSDT", 1));
        m.publish_orders(b, task("ETHUSDT", 2));
        m.publish_orders(a, task("BTCUSDT", 3));
        let tasks = snapshot_tasks(&m);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0]["symbol"], "BTCUSDT");
        assert_eq!(tasks[0]["orders"].as_array().unwrap().len(), 3);
        assert_eq!(tasks[1]["orders"].as_array().unwrap().len(), 2);

        m.publish_orders(b, task("ETHUSDT", 0));
        assert_eq!(snapshot_tasks(&m).len(), 1, "an empty book leaves the snapshot");
        m.clear_orders(a, 2);
        assert!(snapshot_tasks(&m).is_empty(), "an ended task leaves the snapshot");
    }

    /// A client sees every change as an `Orders` frame, an ended task with
    /// nothing left as an empty one, and a task that ends with a position as
    /// a kept slot of its own number.
    #[test]
    fn changes_and_ends_are_sent_as_orders_frames() {
        let m = MonitorBroadcaster::new();
        let mut rx = m.subscribe();
        let slot = m.orders_slot();
        m.publish_orders(slot, task("BTCUSDT", 1));
        let v = json(&rx.try_recv().unwrap());
        assert_eq!(v["type"], "Orders");
        assert_eq!(v["strategy_id"], "st_a");
        assert_eq!(v["orders"][0]["id"], "o0");
        assert_eq!(v["slot"], slot);
        assert!(v.get("managed").is_none(), "a live task's frame has no managed key");
        m.publish_orders(slot, task("BTCUSDT", 0));
        let v = json(&rx.try_recv().unwrap());
        assert!(v["orders"].as_array().unwrap().is_empty(), "an emptied book is sent");
        m.publish_orders(slot, task("BTCUSDT", 1));
        let _ = rx.try_recv().unwrap();
        m.clear_orders(slot, 7);
        let v = json(&rx.try_recv().unwrap());
        assert_eq!(v["type"], "Orders");
        assert_eq!(v["strategy_id"], "st_a", "same key, so the client finds the rows");
        assert_eq!(v["timestamp_ms"], 7);
        assert!(v["orders"].as_array().unwrap().is_empty() && v["positions"].as_array().unwrap().is_empty());
        assert!(snapshot_tasks(&m).is_empty());
        m.clear_orders(slot, 8);
        assert!(rx.try_recv().is_err(), "a task that held nothing sends nothing");

        // A task that ends with a position keeps its slot.
        let slot = m.orders_slot();
        m.publish_orders(slot, holding("ETHUSDT", "virtual_sl"));
        let _ = rx.try_recv().unwrap();
        let mut rows = holding("ETHUSDT", "none");
        rows.positions[0]["was_virtual"] = serde_json::json!(true);
        m.keep_orders(slot, kept(rows, "stopped"));
        let v = json(&rx.try_recv().unwrap());
        assert_eq!(v["slot"], slot, "the kept slot keeps the task's number");
        assert_eq!(v["managed"], false);
        assert_eq!(v["ended"]["reason"], "stopped");
        assert_eq!(v["positions"][0]["was_virtual"], true);
        let tasks = snapshot_tasks(&m);
        assert_eq!(tasks.len(), 1, "a kept slot stays in the snapshot");
        assert_eq!(tasks[0]["managed"], false);
        let kept = m.kept_slots();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].exit_ids, vec!["x-tp".to_string()], "the ids stay beside the rows");
    }

    /// A respawned task already reports the same strategy and symbol: the old
    /// one ending must not wipe its rows for a v1 client.
    #[test]
    fn an_ended_task_does_not_wipe_its_successor() {
        let m = MonitorBroadcaster::new();
        let (old, new) = (m.orders_slot(), m.orders_slot());
        m.publish_orders(old, task("BTCUSDT", 1));
        m.publish_orders(new, task("BTCUSDT", 2));
        let mut rx = m.subscribe();
        m.clear_orders(old, 9);
        let v = json(&rx.try_recv().unwrap());
        assert_eq!(v["orders"].as_array().unwrap().len(), 2, "the successor's rows are sent");
        assert_eq!(snapshot_tasks(&m).len(), 1);
    }

    /// v1 sees a kept slot only while no live task has its key; v2 sees both,
    /// each in its own slot.
    #[test]
    fn a_kept_slot_goes_to_v1_only_while_no_live_task_has_its_key() {
        let m = MonitorBroadcaster::new();
        let mut v1 = m.subscribe();
        let mut v2 = m.subscribe_v2();
        let (old, new) = (m.orders_slot(), m.orders_slot());
        m.publish_orders(new, holding("BTCUSDT", "venue_sl"));
        let _ = (v1.try_recv().unwrap(), v2.events.try_recv().unwrap());

        m.keep_orders(old, kept(holding("BTCUSDT", "none"), "restarted"));
        let legacy = json(&v1.try_recv().unwrap());
        assert_eq!(legacy["slot"], new, "v1 gets the live task's rows");
        let frame = v2.events.try_recv().unwrap();
        assert_eq!((frame.lane, frame.audience, frame.slot), (Lane::Keyed, Audience::Full, old));
        assert_eq!(json(&frame.json)["managed"], false);
        let tasks = snapshot_tasks(&m);
        assert_eq!(tasks.len(), 1, "one entry per key");
        assert_eq!(tasks[0]["slot"], new, "the live task wins");
        assert_eq!(state(&m)["tasks"].as_array().unwrap().len(), 2, "State lists both slots");

        // The live task empties: v1 falls back to the kept rows.
        m.publish_orders(new, task("BTCUSDT", 0));
        let legacy = json(&v1.try_recv().unwrap());
        assert_eq!((legacy["slot"].as_u64(), legacy["managed"].as_bool()), (Some(old), Some(false)));
        let v2_frame = json(&v2.events.try_recv().unwrap().json);
        assert!(v2_frame["orders"].as_array().unwrap().is_empty(), "v2 gets the empty frame of its slot");
        assert_eq!(snapshot_tasks(&m)[0]["slot"], old);
    }

    /// Frames go to their lane and audience, and the legacy channel is fed
    /// only while a v1 client is connected.
    #[test]
    fn lanes_audiences_and_the_legacy_channel() {
        let m = MonitorBroadcaster::new();
        let mut v2 = m.subscribe_v2();
        m.set_demand(&Demand::default(), &full_v2());
        let slot = m.orders_slot();
        m.broadcast(&MonitorEvent::Fill(fill(None)));
        m.broadcast(&MonitorEvent::Tick(tick()));
        m.broadcast(&MonitorEvent::LeanTick(lean(slot)));
        m.broadcast(&MonitorEvent::Px(PxFrame { symbol: "ETHUSDT".into(), bid: 1.0, ask: 2.0, book_ms: 3 }));
        m.broadcast(&MonitorEvent::Beat(Beat { tasks: 1, ..Default::default() }));
        m.series_start(slot, series("BTCUSDT"), 4);
        m.publish_orders(slot, task("BTCUSDT", 1));
        assert!(m.publish_move(slot, mv("o0", 2.0)));
        assert_eq!(m.stats().legacy_frames, 0, "no v1 client, nothing on the legacy channel");

        let events: Vec<WireFrame> = std::iter::from_fn(|| v2.events.try_recv().ok()).collect();
        let kinds: Vec<(String, Lane, Audience)> =
            events.iter().map(|f| (json(&f.json)["type"].as_str().unwrap().to_string(), f.lane, f.audience)).collect();
        assert_eq!(
            kinds,
            vec![
                ("Fill".into(), Lane::Fifo, Audience::All),
                ("Series".into(), Lane::Fifo, Audience::All),
                ("Orders".into(), Lane::Keyed, Audience::Full),
                ("OrderMove".into(), Lane::Keyed, Audience::Full),
            ],
            "the full Tick never goes to v2"
        );
        let ticks: Vec<WireFrame> = std::iter::from_fn(|| v2.ticks.try_recv().ok()).collect();
        let kinds: Vec<(String, Audience, Option<&str>)> = ticks
            .iter()
            .map(|f| (json(&f.json)["type"].as_str().unwrap().to_string(), f.audience, f.symbol.as_deref()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("Tick".into(), Audience::Full, Some("BTCUSDT")),
                ("Px".into(), Audience::Full, Some("ETHUSDT")),
                ("Beat".into(), Audience::All, None),
            ]
        );
        assert!(json(&ticks[0].json)["strategy_state"].get("orders").is_none(), "the lean Tick");
        assert_eq!(ticks[0].slot, slot);

        // pub_no orders frames across both lanes.
        let mut all: Vec<u64> = events.iter().chain(ticks.iter()).map(|f| f.pub_no).collect();
        let n = all.len();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), n, "one number per frame");
        assert!(events.windows(2).all(|w| w[0].pub_no < w[1].pub_no), "a lane is in publish order");
        assert!(events[0].pub_no < ticks[0].pub_no && ticks[0].pub_no < events[1].pub_no);

        // A v1 client: the full Tick and the rest, no lean Tick, Px or Beat.
        let mut v1 = m.subscribe();
        m.broadcast(&MonitorEvent::Tick(tick()));
        m.broadcast(&MonitorEvent::LeanTick(lean(slot)));
        m.broadcast(&MonitorEvent::Beat(Beat::default()));
        m.series_end(slot, "stopped", 5);
        let got: Vec<String> = std::iter::from_fn(|| v1.try_recv().ok()).map(|s| json(&s)["type"].as_str().unwrap().into()).collect();
        assert_eq!(got, vec!["Tick".to_string()]);
    }

    /// `rev` rises with every order frame and move, across slots.
    #[test]
    fn rev_rises_on_every_order_frame_and_move() {
        let m = MonitorBroadcaster::new();
        let mut rx = m.subscribe();
        let (a, b) = (m.orders_slot(), m.orders_slot());
        m.publish_orders(a, task("BTCUSDT", 1));
        m.publish_orders(b, task("ETHUSDT", 1));
        assert!(m.publish_move(a, mv("o0", 2.0)));
        m.publish_orders(a, task("BTCUSDT", 2));
        let revs: Vec<u64> = std::iter::from_fn(|| rx.try_recv().ok()).map(|s| json(&s)["rev"].as_u64().unwrap()).collect();
        assert_eq!(revs, vec![1, 2, 4], "the move took rev 3");
    }

    /// A move updates the registry with no client connected, so a later
    /// snapshot is never older than the move; an unknown row is refused.
    #[test]
    fn an_order_move_updates_the_registry() {
        let m = MonitorBroadcaster::new();
        let slot = m.orders_slot();
        m.publish_orders(slot, task("BTCUSDT", 2));
        assert!(m.publish_move(slot, OrderMove { mid: Some(9.5), ..mv("o1", 3.5) }));
        let tasks = snapshot_tasks(&m);
        assert_eq!(tasks[0]["orders"][1]["price"], 3.5);
        assert_eq!(tasks[0]["orders"][1]["changed_ms"], 11);
        assert_eq!(tasks[0]["orders"][0]["price"], 1.0, "only the moved row");
        assert_eq!(tasks[0]["mid"], 9.5);
        assert_eq!(state(&m)["tasks"][0]["orders"][1]["price"], 3.5, "State has it too");
        assert!(!m.publish_move(slot, mv("gone", 1.0)));
        assert!(!m.publish_move(slot + 1, mv("o0", 1.0)));
        assert_eq!(m.stats().moves, 0, "no v2 Full client, no frame");
    }

    /// Book counts move by diff; paper positions never count; the rises that
    /// go at once are the ones the Lab must not wait for.
    #[test]
    fn book_counts_by_diff() {
        let m = MonitorBroadcaster::new();
        let book = m.subscribe_book();
        let (a, b, p) = (m.orders_slot(), m.orders_slot(), m.orders_slot());
        m.publish_orders(a, holding("BTCUSDT", "virtual_sl"));
        m.publish_orders(b, holding("ETHUSDT", "venue_sl"));
        m.publish_orders(p, TaskOrders { mode: "paper".into(), ..holding("SOLUSDT", "none") });
        let c = m.book_counts();
        assert_eq!((c.positions, c.virtual_sl, c.no_stop, c.unmanaged), (2, 1, 0, 0));
        assert!(book.has_changed().unwrap());

        let before = m.book_counts();
        m.keep_orders(a, kept(holding("BTCUSDT", "none"), "stopped"));
        let c = m.book_counts();
        assert_eq!((c.positions, c.virtual_sl, c.no_stop, c.unmanaged), (2, 0, 1, 1));
        assert!(c.rose_from(&before), "a stop lost and a kept position go at once");

        let before = c;
        m.publish_orders(b, task("ETHUSDT", 0));
        let c = m.book_counts();
        assert_eq!(c.positions, 1);
        assert!(!c.rose_from(&before), "a closed position is coalesced");
        m.set_kept_before(2);
        assert!(m.book_counts().rose_from(&c));
        m.clear_orders(a, 9);
        assert_eq!(m.book_counts(), BookCounts { kept_before: 2, ..Default::default() });
    }

    /// The demand count answers per symbol and moves `demand_gen`.
    #[test]
    fn tick_demand_follows_the_connections() {
        let m = MonitorBroadcaster::new();
        assert_eq!(m.tick_demand("BTCUSDT"), TickDemand::default());
        let gen0 = m.demand_gen();
        let a = Demand { v2_full: true, watch: vec!["BTCUSDT".into()], prices: vec!["ETHUSDT".into()], ..Default::default() };
        m.set_demand(&Demand::default(), &a);
        assert!(m.demand_gen() > gen0);
        assert_eq!(m.tick_demand("BTCUSDT"), TickDemand { lean: true, ..Default::default() });
        assert_eq!(m.tick_demand("ETHUSDT"), TickDemand { px: true, ..Default::default() });
        assert!(m.full_clients());
        let b = Demand { watch: vec!["BTCUSDT".into()], ..Default::default() };
        m.set_demand(&Demand::default(), &b);
        let v1 = Demand { legacy_full: true, ..Default::default() };
        m.set_demand(&Demand::default(), &v1);
        assert!(m.tick_demand("XRPUSDT").legacy, "a v1 Full client wants every task's Tick");

        m.set_demand(&a, &Demand::default());
        assert!(m.tick_demand("BTCUSDT").lean, "the other watcher stays");
        assert!(!m.tick_demand("ETHUSDT").px);
        m.set_demand(&b, &Demand::default());
        m.set_demand(&v1, &Demand::default());
        assert_eq!(m.tick_demand("BTCUSDT"), TickDemand::default());
        assert!(!m.full_clients());
        let gen = m.demand_gen();
        m.set_demand(&Demand::default(), &Demand::default());
        assert_eq!(m.demand_gen(), gen, "no change, no bump");
    }

    /// `State` carries the parts asked for; an absent part means unchanged.
    #[test]
    fn state_snapshot_parts() {
        let m = MonitorBroadcaster::new();
        let slot = m.orders_slot();
        m.series_start(slot, series("BTCUSDT"), 1);
        m.series_start(m.orders_slot(), series("ETHUSDT"), 1);
        m.publish_orders(slot, holding("BTCUSDT", "venue_sl"));
        m.set_run(RunStateFrame { id: "st_a".into(), run_state: "running".into(), start_error: None, hold: None });
        let v = state(&m);
        assert_eq!(v["type"], "State");
        assert_eq!((v["reason"].as_str(), v["run_id"].as_str(), v["pipeline"].as_u64()), (Some("connect"), Some("r1"), Some(1)));
        assert!(v.get("bot").is_none(), "no bot level published yet");
        assert_eq!(v["runs"][0]["run_state"], "running");
        assert_eq!(v["series"][0]["symbols"], serde_json::json!(["BTCUSDT", "ETHUSDT"]));
        assert_eq!(v["tasks"][0]["slot"], slot);
        assert_eq!(v["log_seq"], 3);

        assert!(m.publish_bot(r#"{"trading":"running"}"#.into()));
        assert!(!m.publish_bot(r#"{"trading":"running"}"#.into()), "the same body is dropped");
        assert_eq!(state(&m)["bot"]["trading"], "running");

        let detail = StateParts { reason: "detail".into(), now_ms: 8, tasks: true, ..Default::default() };
        let v = json(&m.state_snapshot(&detail));
        let mut keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, vec!["now_ms", "reason", "tasks", "type"], "a detail State has only the tasks");
        assert_eq!(m.stats().states, 3);
    }

    /// `Bot` goes to v2 only, at both levels.
    #[test]
    fn the_bot_level_goes_to_v2_only() {
        let m = MonitorBroadcaster::new();
        let mut v1 = m.subscribe();
        let mut v2 = m.subscribe_v2();
        assert!(m.publish_bot(r#"{"trading":"halted"}"#.into()));
        assert!(v1.try_recv().is_err());
        let f = v2.events.try_recv().unwrap();
        assert_eq!((f.lane, f.audience), (Lane::Fifo, Audience::All));
        assert_eq!(f.json.as_ref(), r#"{"type":"Bot","trading":"halted"}"#);
    }

    fn tick() -> MonitorTick {
        MonitorTick {
            timestamp_ms: 1,
            strategy_name: "shot".into(),
            mode: "live".into(),
            market: "linear".into(),
            symbol: "BTCUSDT".into(),
            strategy_id: None,
            bid_price: 1.0,
            ask_price: 2.0,
            balance: 0.0,
            trade_count: 0,
            price_lines: Vec::new(),
            strategy_state: serde_json::json!({ "strat_type": "shot", "orders": [] }),
        }
    }

    fn lean(slot: u64) -> LeanTick {
        LeanTick {
            slot,
            timestamp_ms: 1,
            strategy_name: "shot".into(),
            mode: "live".into(),
            market: "linear".into(),
            symbol: "BTCUSDT".into(),
            strategy_id: Some("st_a".into()),
            bid_price: 1.0,
            ask_price: 2.0,
            price_lines: Vec::new(),
            strategy_state: serde_json::json!({ "strat_type": "shot" }),
            held: Vec::new(),
        }
    }

    fn series(symbol: &str) -> SeriesInfo {
        SeriesInfo {
            strategy_id: "st_a".into(),
            name: "shot".into(),
            symbol: symbol.into(),
            market: "linear".into(),
            mode: "live".into(),
            strat_type: "shot".into(),
        }
    }

    fn mv(id: &str, price: f64) -> OrderMove {
        OrderMove {
            strategy_id: Some("st_a".into()),
            strategy_name: "shot".into(),
            symbol: "BTCUSDT".into(),
            id: id.into(),
            price,
            qty: 1.0,
            changed_ms: 11,
            ..Default::default()
        }
    }

    /// A frame with no `hold` is how a hold is cleared, so the key must be
    /// absent then, and carry every field when set.
    #[test]
    fn run_state_frame_carries_the_hold_only_when_held() {
        let mut f = RunStateFrame {
            id: "st_a".into(),
            run_state: "running".into(),
            start_error: None,
            hold: None,
        };
        let v = serde_json::to_value(MonitorEvent::RunState(f.clone())).unwrap();
        assert_eq!(v["type"], "RunState");
        assert!(v.get("hold").is_none());
        f.hold = Some(HoldFrame {
            kind: "paused".into(),
            reason: "paused from the Lab".into(),
            symbols: vec!["BTCUSDT".into()],
            since_ms: 42,
        });
        let v = serde_json::to_value(MonitorEvent::RunState(f)).unwrap();
        assert_eq!(v["run_state"], "running");
        assert_eq!(v["hold"]["kind"], "paused");
        assert_eq!(v["hold"]["symbols"][0], "BTCUSDT");
        assert_eq!(v["hold"]["since_ms"], 42);
    }
}
