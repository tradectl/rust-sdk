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
    pub symbol: String,
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

/// Tagged event envelope for JSON serialization.
#[derive(serde::Serialize, Clone)]
#[serde(tag = "type")]
pub enum MonitorEvent {
    Tick(MonitorTick),
    Fill(MonitorFill),
    Shadow(ShadowSummary),
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
