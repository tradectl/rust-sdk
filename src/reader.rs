//! Read-only abstractions over a running bot's historical and live state.
//!
//! `TradeReader` and `PositionReader` decouple any read-only consumer (the
//! `tradectl-bot-api` HTTP crate, MCP tools, future exporters) from the
//! concrete data sources that live inside `tradectl-live`
//! (`TradeDBReader` over rusqlite, `BotState` over `RwLock`). A consumer
//! depending on these traits never touches `rusqlite` or any exchange
//! adapter.
//!
//! ## Type-movement note (v0.1.14)
//!
//! The concrete trade types (`TradeRow`, `TradeFilter`, `TradePage`,
//! `CloseReason`) currently live in `tradectl-live::trade_db`, right next
//! to the rusqlite reader that produces them. Hoisting them into the core
//! SDK would drag a SQLite-shaped API into a crate that has no business
//! knowing about SQLite, and would churn every existing consumer of those
//! types in `tradectl-live`. That is the higher-risk option.
//!
//! So instead this module defines **wire-shape mirror structs** with fields
//! identical to the `tradectl-live` originals. `tradectl-live` owns the
//! `From` conversions at the boundary (see `trade_db.rs`). The mirrors
//! derive `serde` so an HTTP layer can serialize them straight onto the
//! wire as the REST response body without inventing a third parallel type.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Locked enum for UI display of why a trade closed. Mirrors
/// `tradectl_live::trade_db::CloseReason`. The raw exchange/bot string is
/// preserved separately in [`TradeRow::close_reason_raw`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    Tp,
    Sl,
    Manual,
    ForceClose,
    Liquidated,
    Canceled,
    Unknown,
}

impl CloseReason {
    /// Map a raw `close_reason` string onto the locked enum, case-insensitively.
    /// Mirrors `tradectl_live::trade_db::CloseReason::from_raw` exactly so the
    /// HTTP `reason=` query filter and the SQLite reader agree.
    pub fn from_raw(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "tp" => Self::Tp,
            "sl" => Self::Sl,
            "manual" => Self::Manual,
            "forceclose" | "force_close" | "force-close" => Self::ForceClose,
            "liquidated" | "liquidation" => Self::Liquidated,
            "canceled" | "cancelled" => Self::Canceled,
            _ => Self::Unknown,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Tp => "TP",
            Self::Sl => "SL",
            Self::Manual => "Manual",
            Self::ForceClose => "ForceClose",
            Self::Liquidated => "Liquidated",
            Self::Canceled => "Canceled",
            Self::Unknown => "Unknown",
        }
    }
}

/// One closed-trade row. Field-for-field mirror of
/// `tradectl_live::trade_db::TradeRow`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeRow {
    pub id: i64,
    pub strategy: String,
    pub symbol: String,
    pub side: String,
    pub entry_price: f64,
    pub exit_price: f64,
    pub quantity: f64,
    pub profit_usd: f64,
    pub profit_pct: f64,
    pub profit_pct_raw: f64,
    pub close_reason_raw: String,
    pub reason: CloseReason,
    pub opened_at: i64,
    pub closed_at: i64,
    pub entry_order_id: String,
    pub exit_order_id: String,
    pub mode: String,
    /// `"auto"` for strategy-driven trades, `"manual"` for human-placed.
    pub source: String,
    /// Lowercased market type: "linear" | "inverse" | "spot". `""` for
    /// legacy rows / bots predating the column (serde-default keeps old
    /// bots' JSON decodable).
    #[serde(default)]
    pub market_type: String,
    /// Lowercased exchange identity: "binance" | "bybit" | … `""` for
    /// legacy rows / older bots.
    #[serde(default)]
    pub exchange: String,
}

/// Optional filters for [`TradeReader::list_trades`]. Empty-string / `None`
/// means "no filter". Mirror of `tradectl_live::trade_db::TradeFilter`.
#[derive(Debug, Default, Clone)]
pub struct TradeFilter {
    pub strategy: Option<String>,
    pub symbol: Option<String>,
    pub mode: Option<String>,
    pub side: Option<String>,
    pub reason: Option<CloseReason>,
    /// Bot config name (analytics cell identity).
    pub config_name: Option<String>,
    /// Inclusive lower bound on `closed_at` (epoch millis).
    pub from_ms: Option<i64>,
    /// Exclusive upper bound on `closed_at` (epoch millis).
    pub to_ms: Option<i64>,
}

/// One page of rows from [`TradeReader::list_trades`]. Mirror of
/// `tradectl_live::trade_db::TradePage`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradePage {
    pub rows: Vec<TradeRow>,
    /// When `Some`, pass this back as the `cursor` arg to fetch the next page.
    /// Encoded as `closed_at_ms:id` so ties on `closed_at` break deterministically.
    pub next_cursor: Option<String>,
    /// Total rows matching the filter before pagination (for "showing X of Y").
    pub total: i64,
}

/// Error returned by [`TradeReader`] methods. Deliberately opaque — it
/// carries a human-readable message produced by the concrete backend
/// (e.g. a stringified `rusqlite::Error`) so this crate stays free of any
/// storage-engine dependency.
#[derive(Debug, Clone)]
pub struct TradeReaderError {
    message: String,
}

impl TradeReaderError {
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for TradeReaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "trade reader error: {}", self.message)
    }
}

impl std::error::Error for TradeReaderError {}

/// Read-only access to a bot's persisted, closed-trade history.
///
/// Implemented in `tradectl-live` for `TradeDBReader` (rusqlite, WAL-mode
/// read connection). The implementation converts its native row/filter/page
/// types into the wire mirrors defined above at the call boundary.
pub trait TradeReader: Send + Sync {
    /// Paginate closed trades matching `filter`, newest-first. `limit` is
    /// clamped by the implementation. `cursor`, when set, is a previous
    /// call's `TradePage::next_cursor`.
    fn list_trades(
        &self,
        filter: &TradeFilter,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<TradePage, TradeReaderError>;

    /// Fetch a single trade by id. `Ok(None)` if no row matches.
    fn get_trade(&self, id: i64) -> Result<Option<TradeRow>, TradeReaderError>;
}

/// Read-only access to a bot's current open positions.
///
/// Implemented for [`crate::bot_state::BotState`] (see `bot_state.rs`), so a
/// runner can pass its `Arc<BotState>` straight in. Returns the SDK's
/// existing [`crate::bot_state::PositionSnapshot`] so no new position type
/// is invented here; HTTP consumers map it onto their own wire DTO.
pub trait PositionReader: Send + Sync {
    fn positions(&self) -> Vec<crate::bot_state::PositionSnapshot>;
}

/// Lightweight, allocation-cheap status fields for a `/status`-style
/// endpoint. Deliberately decoupled from any wire crate: an HTTP layer
/// maps this onto its own response DTO (e.g.
/// `tradectl_control_proto::StatusResponse`). Identity fields the core
/// state doesn't track (`name`, `version`) are supplied by whoever
/// constructs the reader — typically the runner, which knows both.
#[derive(Debug, Clone, Default)]
pub struct StatusInfo {
    pub name: String,
    pub version: String,
    pub mode: String,
    pub exchange: String,
    pub uptime_secs: u64,
    pub symbol_count: u32,
    pub position_count: u32,
}

/// Read-only access to a bot's identity + uptime + counts.
///
/// Not implemented for `BotState` directly, because `name`/`version` are
/// owned by the runner rather than the shared state. `tradectl-live`
/// provides a thin wrapper that pairs an `Arc<BotState>` with those two
/// strings.
pub trait StatusReader: Send + Sync {
    fn status_info(&self) -> StatusInfo;
}

/// Overall summary for a stats window. `win_rate` is a percentage in
/// `0.0..=100.0`, computed from `wins / (wins + losses)` and guarded
/// against division by zero (yields `0.0` when there are no decided trades).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsSummary {
    pub trade_count: usize,
    pub net_pnl_usd: f64,
    pub wins: usize,
    pub losses: usize,
    pub win_rate: f64,
}

/// One day's aggregated P&L within a stats window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyStat {
    /// `YYYY-MM-DD` (UTC) as produced by SQLite `date()`.
    pub date: String,
    pub pnl_usd: f64,
    pub trade_count: usize,
    pub wins: usize,
}

/// Per-symbol aggregated P&L within a stats window (daily rows rolled up to
/// per-symbol totals for the whole window).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoinStat {
    pub symbol: String,
    pub pnl_usd: f64,
    pub trade_count: usize,
    pub wins: usize,
}

/// Server-side, time-windowed trade statistics. The body of `GET /v1/stats`.
/// Computed entirely in SQLite by the concrete backend — no row pulling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsResponse {
    pub summary: StatsSummary,
    pub daily: Vec<DailyStat>,
    pub by_coin: Vec<CoinStat>,
}

impl Default for StatsResponse {
    fn default() -> Self {
        Self {
            summary: StatsSummary {
                trade_count: 0,
                net_pnl_usd: 0.0,
                wins: 0,
                losses: 0,
                win_rate: 0.0,
            },
            daily: Vec::new(),
            by_coin: Vec::new(),
        }
    }
}

/// Read-only access to server-side aggregated trade statistics over a time
/// window. Implemented in `tradectl-live` over the same `TradeDBReader` that
/// backs [`TradeReader`]; the aggregation runs in SQLite. `mode` filters by
/// `paper`/`live`; `None` aggregates across all modes (matching `/v1/trades`).
pub trait StatsReader: Send + Sync {
    fn stats(
        &self,
        from_ms: i64,
        until_ms: i64,
        mode: Option<&str>,
    ) -> Result<StatsResponse, TradeReaderError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_reason_from_raw_matches_live_semantics() {
        assert_eq!(CloseReason::from_raw("tp"), CloseReason::Tp);
        assert_eq!(CloseReason::from_raw("SL"), CloseReason::Sl);
        assert_eq!(CloseReason::from_raw("force_close"), CloseReason::ForceClose);
        assert_eq!(CloseReason::from_raw("force-close"), CloseReason::ForceClose);
        assert_eq!(CloseReason::from_raw("liquidation"), CloseReason::Liquidated);
        assert_eq!(CloseReason::from_raw("cancelled"), CloseReason::Canceled);
        assert_eq!(CloseReason::from_raw("???"), CloseReason::Unknown);
    }

    #[test]
    fn close_reason_serializes_snake_case() {
        let j = serde_json::to_string(&CloseReason::ForceClose).unwrap();
        assert_eq!(j, "\"force_close\"");
        let back: CloseReason = serde_json::from_str("\"tp\"").unwrap();
        assert_eq!(back, CloseReason::Tp);
    }

    #[test]
    fn trade_page_round_trips_json() {
        let page = TradePage {
            rows: vec![TradeRow {
                id: 7,
                strategy: "shot".into(),
                symbol: "BTCUSDT".into(),
                side: "LONG".into(),
                entry_price: 100.0,
                exit_price: 101.0,
                quantity: 1.0,
                profit_usd: 1.0,
                profit_pct: 1.0,
                profit_pct_raw: 0.01,
                close_reason_raw: "tp".into(),
                reason: CloseReason::Tp,
                opened_at: 1,
                closed_at: 2,
                entry_order_id: "e".into(),
                exit_order_id: "x".into(),
                mode: "live".into(),
                source: "auto".into(),
                market_type: "linear".into(),
                exchange: "binance".into(),
            }],
            next_cursor: Some("2:7".into()),
            total: 1,
        };
        let s = serde_json::to_string(&page).unwrap();
        let back: TradePage = serde_json::from_str(&s).unwrap();
        assert_eq!(back.rows.len(), 1);
        assert_eq!(back.rows[0].id, 7);
        assert_eq!(back.next_cursor.as_deref(), Some("2:7"));
        assert_eq!(back.total, 1);
    }
}
