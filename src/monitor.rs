//! WebSocket monitor server — broadcasts live strategy state to connected clients.
//!
//! The server binds to a configurable host:port and fans out JSON messages via
//! a `tokio::sync::broadcast` channel. Zero overhead when no clients are connected.

use tokio::sync::broadcast;

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
#[derive(serde::Serialize, Clone)]
pub struct RunStateFrame {
    pub id: String,
    pub run_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hold: Option<HoldFrame>,
}

/// Why a running instance is holding its entries.
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
pub struct HoldFrame {
    /// `"bot_halt"` (the whole bot: max-loss cap, account-fatal error,
    /// Telegram `/stop`, resource or disk guard), `"paused"` (a manual or
    /// edge-decay pause of some of its symbols) or `"pairs_halted"` (some of
    /// its pairs stopped on exchange errors while others still trade).
    pub kind: String,
    pub reason: String,
    /// The symbols held: the paused or halted pairs. Empty for `bot_halt`.
    pub symbols: Vec<String>,
    /// Epoch ms the hold began.
    pub since_ms: u64,
}

/// Tagged event envelope for JSON serialization.
#[derive(serde::Serialize, Clone)]
#[serde(tag = "type")]
pub enum MonitorEvent {
    Tick(MonitorTick),
    Fill(MonitorFill),
    Shadow(ShadowSummary),
    Config(ConfigApplied),
    RunState(RunStateFrame),
}

/// Fans monitor events out to subscribers over a `tokio::broadcast` channel.
///
/// Channel-only by design: there is no standalone listener. The bot API
/// (`tradectl-bot-api`) serves these frames to clients over its authed,
/// TLS `wss://…/v1/stream` route — one port for the whole bot. A build with
/// the `monitor` feature but no API server simply has no consumer, and
/// [`broadcast`](Self::broadcast) becomes a no-op (no subscribers).
pub struct MonitorBroadcaster {
    tx: broadcast::Sender<String>,
}

impl Default for MonitorBroadcaster {
    fn default() -> Self {
        Self::new()
    }
}

impl MonitorBroadcaster {
    /// Create a broadcaster backed by a bounded (64) fan-out channel. No
    /// socket is bound; consumers obtain receivers via
    /// [`subscribe`](Self::subscribe).
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel::<String>(64);
        Self { tx }
    }

    /// Subscribe a new receiver. Each connected `/v1/stream` client holds one;
    /// dropping it decrements the count [`has_clients`](Self::has_clients) sees.
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }

    /// Returns `true` if at least one subscriber is connected.
    pub fn has_clients(&self) -> bool {
        self.tx.receiver_count() > 0
    }

    /// Broadcast an event to all connected clients.
    /// No-op if no clients are connected.
    pub fn broadcast(&self, event: &MonitorEvent) {
        if self.tx.receiver_count() == 0 {
            return;
        }
        if let Ok(json) = serde_json::to_string(event) {
            let _ = self.tx.send(json);
        }
    }
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
