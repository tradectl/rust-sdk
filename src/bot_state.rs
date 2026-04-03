//! Shared bot state — written by the runner, read by MCP and AI plugins.
//!
//! All fields use `RwLock` for concurrent reads from multiple tool calls.
//! The runner writes from symbol tasks; MCP/AI plugins read via `Arc<BotState>`.

use std::collections::{HashMap, VecDeque};
use tokio::sync::RwLock;
use serde::Serialize;

use crate::types::config::{PromotionCandidate, PromotionRecord};

// ── Shared trait APIs (used by both MCP and AI plugins) ────────

/// Promotion store API — trait object to avoid cross-plugin deps.
pub trait PromotionStoreApi: Send + Sync {
    fn pending_list(&self) -> Vec<(String, PromotionCandidate)>;
    fn history_list(&self) -> Vec<PromotionRecord>;
    fn approve(&self, symbol: &str) -> Option<PromotionCandidate>;
    fn reject(&self, symbol: &str) -> bool;
}

/// Session store API — trait object for session state reads.
pub trait SessionStoreApi: Send + Sync {
    fn all(&self) -> HashMap<String, serde_json::Value>;
}

/// Trigger engine API — trait object for trigger state reads.
pub trait TriggerEngineApi: Send + Sync {
    fn snapshot(&self) -> serde_json::Value;
}

/// Ring buffer capacity for recent fills.
const RECENT_FILLS_CAP: usize = 200;

/// Shared bot state, created by the runner and passed to MCP/AI plugins.
pub struct BotState {
    /// Per-symbol position snapshots, updated on every fill.
    positions: RwLock<HashMap<String, PositionSnapshot>>,
    /// Recent fills across all symbols, ring buffer.
    recent_fills: RwLock<VecDeque<FillSnapshot>>,
    /// Per-symbol latest ticker, updated on every tick.
    latest_tickers: RwLock<HashMap<String, TickerSnapshot>>,
    /// Bot-level metadata.
    meta: RwLock<BotMeta>,
    /// Per-symbol active entries/exits (for debugging).
    active_orders: RwLock<HashMap<String, OrdersSnapshot>>,
    /// Latest shadow summaries per strategy/symbol key.
    shadow_summaries: RwLock<HashMap<String, ShadowSummarySnapshot>>,
    /// Per-symbol latest strategy state (MonitorSnapshot data).
    strategy_states: RwLock<HashMap<String, serde_json::Value>>,
    /// Strategy documentation (loaded from STRATEGY.md files).
    strategy_docs: RwLock<HashMap<String, String>>,
}

// ── Snapshot types ─────────────────────────────────────────────

/// Position snapshot for a single symbol.
#[derive(Debug, Clone, Serialize)]
pub struct PositionSnapshot {
    pub symbol: String,
    pub side: String,
    pub avg_entry: f64,
    pub quantity: f64,
    pub entry_count: usize,
    pub unrealized_pnl: f64,
    pub unrealized_pnl_pct: f64,
    pub tp_price: f64,
    pub sl_price: f64,
    pub strategy_name: String,
    pub timestamp_ms: u64,
}

/// Fill event snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct FillSnapshot {
    pub timestamp_ms: u64,
    pub symbol: String,
    pub strategy_name: String,
    pub side: String,
    pub price: f64,
    pub quantity: f64,
    pub fill_type: String,
    pub profit_pct: Option<f64>,
    pub profit_usd: Option<f64>,
    pub position_closed: bool,
}

/// Latest ticker data for a symbol.
#[derive(Debug, Clone, Serialize)]
pub struct TickerSnapshot {
    pub symbol: String,
    pub bid_price: f64,
    pub bid_qty: f64,
    pub ask_price: f64,
    pub ask_qty: f64,
    pub spread: f64,
    pub spread_pct: f64,
    pub timestamp_ms: u64,
}

/// Bot-level metadata.
#[derive(Debug, Clone, Serialize)]
pub struct BotMeta {
    pub strategies: Vec<String>,
    pub symbols: Vec<String>,
    pub mode: String,
    pub provider: String,
    pub balance: f64,
    pub uptime_secs: u64,
    pub started_at_ms: u64,
    pub trade_count: usize,
}

impl Default for BotMeta {
    fn default() -> Self {
        Self {
            strategies: Vec::new(),
            symbols: Vec::new(),
            mode: String::new(),
            provider: String::new(),
            balance: 0.0,
            uptime_secs: 0,
            started_at_ms: 0,
            trade_count: 0,
        }
    }
}

/// Active orders for a symbol.
#[derive(Debug, Clone, Serialize)]
pub struct OrdersSnapshot {
    pub symbol: String,
    pub pending_entries: Vec<OrderSnapshot>,
    pub active_exits: Vec<OrderSnapshot>,
}

/// Single order snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct OrderSnapshot {
    pub order_id: String,
    pub side: String,
    pub price: f64,
    pub quantity: f64,
    pub kind: String,
}

/// Shadow optimization summary snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct ShadowSummarySnapshot {
    pub strategy_name: String,
    pub symbol: String,
    pub window_secs: u64,
    pub timestamp_ms: u64,
    pub results: Vec<ShadowResultSnapshot>,
    pub details: Vec<serde_json::Value>,
}

/// Single shadow variant result.
#[derive(Debug, Clone, Serialize)]
pub struct ShadowResultSnapshot {
    pub variant: String,
    pub trade_count: usize,
    pub pnl: f64,
    pub pnl_pct: f64,
    pub max_drawdown_pct: f64,
    pub score: f64,
    pub eligible: bool,
}

/// API metrics snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct ApiMetricsSnapshot {
    pub ops_by_type: HashMap<String, u64>,
    pub weight_usage_pct: f64,
    pub order_usage_pct: f64,
    pub is_paused: bool,
}

// ── BotState implementation ────────────────────────────────────

impl BotState {
    pub fn new() -> Self {
        Self {
            positions: RwLock::new(HashMap::new()),
            recent_fills: RwLock::new(VecDeque::new()),
            latest_tickers: RwLock::new(HashMap::new()),
            meta: RwLock::new(BotMeta::default()),
            active_orders: RwLock::new(HashMap::new()),
            shadow_summaries: RwLock::new(HashMap::new()),
            strategy_states: RwLock::new(HashMap::new()),
            strategy_docs: RwLock::new(HashMap::new()),
        }
    }

    // ── Write methods (called by runner) ───────────────────────

    /// Update position for a symbol. Pass `None` to remove (position closed).
    pub async fn update_position(&self, symbol: &str, snapshot: Option<PositionSnapshot>) {
        let mut positions = self.positions.write().await;
        match snapshot {
            Some(s) => { positions.insert(symbol.to_string(), s); }
            None => { positions.remove(symbol); }
        }
    }

    /// Record a fill event.
    pub async fn record_fill(&self, fill: FillSnapshot) {
        let mut fills = self.recent_fills.write().await;
        fills.push_back(fill);
        while fills.len() > RECENT_FILLS_CAP {
            fills.pop_front();
        }
    }

    /// Update latest ticker for a symbol.
    pub async fn update_ticker(&self, symbol: &str, ticker: &crate::TickerEvent) {
        let mid = (ticker.bid_price + ticker.ask_price) / 2.0;
        let spread = ticker.ask_price - ticker.bid_price;
        let spread_pct = if mid > 0.0 { spread / mid * 100.0 } else { 0.0 };
        let snapshot = TickerSnapshot {
            symbol: symbol.to_string(),
            bid_price: ticker.bid_price,
            bid_qty: ticker.bid_qty,
            ask_price: ticker.ask_price,
            ask_qty: ticker.ask_qty,
            spread,
            spread_pct,
            timestamp_ms: ticker.timestamp_ms,
        };
        self.latest_tickers.write().await.insert(symbol.to_string(), snapshot);
    }

    /// Update bot metadata.
    pub async fn update_meta(&self, meta: BotMeta) {
        *self.meta.write().await = meta;
    }

    /// Update active orders for a symbol.
    pub async fn update_orders(&self, symbol: &str, snapshot: OrdersSnapshot) {
        self.active_orders.write().await.insert(symbol.to_string(), snapshot);
    }

    /// Update shadow summary for a strategy/symbol key.
    pub async fn update_shadow(&self, key: &str, summary: ShadowSummarySnapshot) {
        self.shadow_summaries.write().await.insert(key.to_string(), summary);
    }

    /// Update strategy state (MonitorSnapshot data).
    pub async fn update_strategy_state(&self, key: &str, state: serde_json::Value) {
        self.strategy_states.write().await.insert(key.to_string(), state);
    }

    /// Set strategy documentation (loaded from STRATEGY.md at startup).
    pub async fn set_strategy_doc(&self, strategy_name: &str, doc: String) {
        self.strategy_docs.write().await.insert(strategy_name.to_string(), doc);
    }

    // ── Read methods (called by MCP/AI) ────────────────────────

    /// Get all open positions, optionally filtered by symbol.
    pub async fn get_positions(&self, symbol: Option<&str>) -> Vec<PositionSnapshot> {
        let positions = self.positions.read().await;
        match symbol {
            Some(s) => positions.get(s).into_iter().cloned().collect(),
            None => positions.values().cloned().collect(),
        }
    }

    /// Get recent fills, optionally filtered by symbol, limited to `limit`.
    pub async fn get_recent_fills(&self, symbol: Option<&str>, limit: usize) -> Vec<FillSnapshot> {
        let fills = self.recent_fills.read().await;
        let iter = fills.iter().rev();
        match symbol {
            Some(s) => iter.filter(|f| f.symbol == s).take(limit).cloned().collect(),
            None => iter.take(limit).cloned().collect(),
        }
    }

    /// Get latest market data, optionally filtered by symbol.
    pub async fn get_market_data(&self, symbol: Option<&str>) -> Vec<TickerSnapshot> {
        let tickers = self.latest_tickers.read().await;
        match symbol {
            Some(s) => tickers.get(s).into_iter().cloned().collect(),
            None => tickers.values().cloned().collect(),
        }
    }

    /// Get bot metadata.
    pub async fn get_bot_meta(&self) -> BotMeta {
        let mut meta = self.meta.read().await.clone();
        // Compute uptime dynamically from started_at_ms
        if meta.started_at_ms > 0 {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            meta.uptime_secs = now_ms.saturating_sub(meta.started_at_ms) / 1000;
        }
        meta
    }

    /// Update balance in bot metadata (called on fill or periodically).
    pub async fn update_balance(&self, balance: f64) {
        self.meta.write().await.balance = balance;
    }

    /// Increment trade count in bot metadata.
    pub async fn increment_trade_count(&self) {
        self.meta.write().await.trade_count += 1;
    }

    /// Get active orders, optionally filtered by symbol.
    pub async fn get_active_orders(&self, symbol: Option<&str>) -> Vec<OrdersSnapshot> {
        let orders = self.active_orders.read().await;
        match symbol {
            Some(s) => orders.get(s).into_iter().cloned().collect(),
            None => orders.values().cloned().collect(),
        }
    }

    /// Get shadow summaries, optionally filtered by symbol.
    pub async fn get_shadow_summaries(&self, symbol: Option<&str>) -> Vec<ShadowSummarySnapshot> {
        let summaries = self.shadow_summaries.read().await;
        match symbol {
            Some(s) => summaries.iter()
                .filter(|(_, v)| v.symbol == s)
                .map(|(_, v)| v.clone())
                .collect(),
            None => summaries.values().cloned().collect(),
        }
    }

    /// Get strategy state, optionally filtered by symbol.
    pub async fn get_strategy_states(&self, symbol: Option<&str>) -> HashMap<String, serde_json::Value> {
        let states = self.strategy_states.read().await;
        match symbol {
            Some(s) => states.iter()
                .filter(|(k, _)| k.contains(s))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            None => states.clone(),
        }
    }

    /// Get strategy documentation by name.
    pub async fn get_strategy_doc(&self, strategy_name: &str) -> Option<String> {
        self.strategy_docs.read().await.get(strategy_name).cloned()
    }

    /// Get all strategy docs.
    pub async fn get_all_strategy_docs(&self) -> HashMap<String, String> {
        self.strategy_docs.read().await.clone()
    }
}
