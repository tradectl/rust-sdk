//! Shared bot state — written by the runner, read by MCP and AI plugins.
//!
//! All fields use `RwLock` for concurrent reads from multiple tool calls.
//! The runner writes from symbol tasks; MCP/AI plugins read via `Arc<BotState>`.

use std::collections::{HashMap, HashSet, VecDeque};
use tokio::sync::RwLock;
use serde::Serialize;

use crate::strat_id::StratId;
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

/// Strategy control API — pause/resume entries per symbol.
pub trait StrategyControlApi: Send + Sync {
    fn pause(&self, symbol: &str) -> bool;
    fn resume(&self, symbol: &str) -> bool;
    fn is_paused(&self, symbol: &str) -> bool;
}

/// Ring buffer capacity for recent fills.
const RECENT_FILLS_CAP: usize = 200;

/// Shared bot state, created by the runner and passed to MCP/AI plugins.
pub struct BotState {
    /// Position snapshots keyed by `(StratId, symbol)`, updated on every
    /// fill. More than one strategy instance routinely trades one symbol at
    /// once (a grid config runs several rungs), and each rung needs its own
    /// row: `/v1/status` counts them and `/v1/intent` declares them, so a
    /// collision undercounts open positions and drops declarations.
    ///
    /// The key was `symbol` alone, then `(strategy_name, symbol)`. Both
    /// collided, and each collision cost a position. Names are NOT unique —
    /// several instances under one name is supported config — so the name key
    /// merely narrowed the failure to same-named siblings, where the first to
    /// close removed the survivor's row and with it the declaration of a
    /// virtual stop the bot was still enforcing (prod, 2026-08-17,
    /// BTCUSD_PERP: the watchdog closed it for "no SL"). Only the instance id
    /// is unique; see [`crate::strat_id::StratId`].
    positions: RwLock<HashMap<(String, String), PositionSnapshot>>,
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
    /// Ids of emulator (paper) strategy instances, set once at startup. Used
    /// to stamp `PositionSnapshot::emulated` on write so the watchdog-facing
    /// API can exclude paper positions. A plain `std::sync::RwLock` (not
    /// tokio's): it's read briefly inside `update_position` without crossing
    /// an `.await`.
    ///
    /// Ids, not names: `is_emulator` is a property of a config ENTRY, and two
    /// entries can share a name. Keyed by name, one paper instance would stamp
    /// its live same-named sibling's positions `emulated` — and the watchdog
    /// link drops those from the declaration entirely, leaving a real position
    /// undeclared.
    emulators: std::sync::RwLock<HashSet<String>>,
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
    /// True when `sl_price` is a client-side VIRTUAL stop the runner enforces
    /// per-tick with no resting order on the exchange (opt-in `virtual_sl`),
    /// as opposed to a real exchange-resting stop. The watchdog reads this to
    /// tell an intentional virtual SL apart from a lost/absent one: a healthy
    /// bot's declared virtual SL is enforced on breach rather than force-closed
    /// at the unprotected-time cap.
    #[serde(default)]
    pub virtual_sl: bool,
    /// Id of the resting stop backing `sl_price` (empty for a virtual or
    /// not-yet-placed stop). Forwarded to the watchdog via `/v1/intent` so it
    /// can match the declared stop to a real open order.
    ///
    /// This is the runner's own **client** order id (`<position_id>_sl`), which
    /// it sends as `clientOrderId` — NOT the venue's numeric `orderId`. A
    /// consumer must match it against `Order::client_order_id`, or against
    /// either id; matching `Order::order_id` alone finds nothing. (Prod
    /// 2026-08-03: the watchdog compared only `order_id`, so coverage-summing
    /// silently never fired and a fully-stopped net was force-closed.)
    #[serde(default)]
    pub sl_order_id: String,
    /// Id of the resting take-profit backing `tp_price` (empty when none is
    /// resting). Same client-id caveat as [`Self::sl_order_id`].
    #[serde(default)]
    pub tp_order_id: String,
    pub strategy_name: String,
    /// True when this snapshot belongs to an emulator (paper) strategy — its
    /// orders never reach the real exchange. Stamped by [`BotState`] from the
    /// emulator-name set so the watchdog-facing surface (`/v1/intent`,
    /// `/v1/status` position_count) can exclude paper positions: the watchdog
    /// guards a REAL exchange account and must never match a real net position
    /// against a paper rung's declared (paper-only) protection.
    #[serde(default)]
    pub emulated: bool,
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
    /// The bot's market, as a watchdog-style string (`"binance-coinm"` /
    /// `"binance-usdm"` / `"<provider>-<markettype>"`). Lets a watchdog learn
    /// which account it guards from the bot itself. Empty if unknown.
    pub market: String,
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
            market: String::new(),
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_params_age_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_score_history: Option<Vec<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub staleness_alert: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge_decay_consecutive: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paused: Option<bool>,
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

// ── Computed insight types ────────────────────────────────────

/// Performance summary computed from recent fills.
#[derive(Debug, Clone, Serialize)]
pub struct PerformanceSummary {
    pub total_trades: usize,
    pub wins: usize,
    pub losses: usize,
    pub win_rate_pct: f64,
    pub total_pnl_usd: f64,
    pub avg_win_usd: f64,
    pub avg_loss_usd: f64,
    pub profit_factor: f64,
    pub largest_win_usd: f64,
    pub largest_loss_usd: f64,
    pub current_streak: i32,
    pub by_symbol: Vec<SymbolPerformance>,
}

/// Per-symbol performance breakdown.
#[derive(Debug, Clone, Serialize)]
pub struct SymbolPerformance {
    pub symbol: String,
    pub trades: usize,
    pub wins: usize,
    pub pnl_usd: f64,
    pub win_rate_pct: f64,
}

/// Risk assessment across all open positions.
#[derive(Debug, Clone, Serialize)]
pub struct RiskAssessment {
    pub balance: f64,
    pub total_unrealized_pnl: f64,
    pub total_unrealized_pnl_pct: f64,
    pub open_positions: usize,
    pub positions: Vec<PositionRisk>,
}

/// Risk info for a single position.
#[derive(Debug, Clone, Serialize)]
pub struct PositionRisk {
    pub symbol: String,
    pub side: String,
    pub entry_price: f64,
    pub current_price: f64,
    pub unrealized_pnl: f64,
    pub unrealized_pnl_pct: f64,
    pub distance_to_tp_pct: f64,
    pub distance_to_sl_pct: f64,
    pub strategy_name: String,
}

/// Symbol comparison for side-by-side analysis.
#[derive(Debug, Clone, Serialize)]
pub struct SymbolComparison {
    pub symbol: String,
    pub trades: usize,
    pub wins: usize,
    pub win_rate_pct: f64,
    pub pnl_usd: f64,
    pub avg_pnl_usd: f64,
    pub has_position: bool,
    pub unrealized_pnl: f64,
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
            emulators: std::sync::RwLock::new(HashSet::new()),
        }
    }

    /// Record which strategy instances are emulators (paper). Called once at
    /// startup, before any position write, so every `PositionSnapshot` is
    /// stamped with the correct `emulated` flag as it's written.
    pub fn set_emulators(&self, ids: HashSet<StratId>) {
        *self.emulators.write().unwrap() =
            ids.into_iter().map(|id| id.as_str().to_string()).collect();
    }

    // ── Write methods (called by runner) ───────────────────────

    /// Update position for one strategy instance's symbol. Pass `None` to
    /// remove (position closed).
    ///
    /// Keyed by [`StratId`], never by name. Two instances of one strategy on
    /// one symbol are a supported config, and under a name key they collided
    /// on a single entry: the second overwrote the first, and the first to go
    /// flat **removed the survivor's snapshot**. On a `virtualSl` bot that
    /// snapshot is the only evidence the stop exists — it is not on the
    /// exchange — so the watchdog read a live position as unprotected and
    /// closed it (prod, 2026-08-17, BTCUSD_PERP).
    pub async fn update_position(
        &self,
        strategy_id: &StratId,
        symbol: &str,
        snapshot: Option<PositionSnapshot>,
    ) {
        let mut positions = self.positions.write().await;
        let key = (strategy_id.as_str().to_string(), symbol.to_string());
        match snapshot {
            Some(mut s) => {
                // Authoritative stamp from the emulator-id set — a rung is
                // paper iff its config entry was configured as an emulator, no
                // matter what the caller passed.
                s.emulated = self.emulators.read().unwrap().contains(strategy_id.as_str());
                positions.insert(key, s);
            }
            None => { positions.remove(&key); }
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

    // ── Sync, non-blocking reads (for the sync reader traits) ──

    /// Non-blocking snapshot of open positions, for the sync
    /// [`crate::reader::PositionReader`] trait used by the HTTP API.
    /// Returns an empty `Vec` if a writer currently holds the lock — a
    /// momentary contention that the next poll resolves. Never blocks the
    /// async runtime, so it is safe to call from inside an axum handler.
    pub fn try_get_positions(&self) -> Vec<PositionSnapshot> {
        self.positions
            .try_read()
            .map(|p| p.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Non-blocking clone of bot metadata, for the sync status path.
    /// `None` if a writer holds the lock at this instant.
    pub fn try_get_meta(&self) -> Option<BotMeta> {
        self.meta.try_read().map(|m| m.clone()).ok()
    }

    // ── Read methods (called by MCP/AI) ────────────────────────

    /// Get all open positions, optionally filtered by symbol. A symbol can
    /// have more than one entry here (one per strategy instance/rung
    /// trading it), so this filters by the snapshot's own `symbol` field
    /// rather than doing a keyed lookup.
    pub async fn get_positions(&self, symbol: Option<&str>) -> Vec<PositionSnapshot> {
        let positions = self.positions.read().await;
        match symbol {
            Some(s) => positions.values().filter(|p| p.symbol == s).cloned().collect(),
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

    // ── Computed insight methods ──────────────────────────────────

    /// Compute performance summary from recent fills.
    pub async fn get_performance_summary(&self) -> PerformanceSummary {
        let fills = self.recent_fills.read().await;

        // Only consider closed trades (position_closed=true with profit data)
        let closed: Vec<_> = fills.iter()
            .filter(|f| f.position_closed && f.profit_usd.is_some())
            .collect();

        let total_trades = closed.len();
        let mut wins = 0usize;
        let mut losses = 0usize;
        let mut total_pnl = 0.0f64;
        let mut gross_profit = 0.0f64;
        let mut gross_loss = 0.0f64;
        let mut largest_win = 0.0f64;
        let mut largest_loss = 0.0f64;
        let mut current_streak: i32 = 0;
        let mut by_symbol: HashMap<String, (usize, usize, f64)> = HashMap::new(); // trades, wins, pnl

        for fill in &closed {
            let pnl = fill.profit_usd.unwrap_or(0.0);
            total_pnl += pnl;

            let entry = by_symbol.entry(fill.symbol.clone()).or_insert((0, 0, 0.0));
            entry.0 += 1;
            entry.2 += pnl;

            if pnl >= 0.0 {
                wins += 1;
                entry.1 += 1;
                gross_profit += pnl;
                if pnl > largest_win { largest_win = pnl; }
                if current_streak >= 0 { current_streak += 1; } else { current_streak = 1; }
            } else {
                losses += 1;
                gross_loss += pnl.abs();
                if pnl < largest_loss { largest_loss = pnl; }
                if current_streak <= 0 { current_streak -= 1; } else { current_streak = -1; }
            }
        }

        let win_rate = if total_trades > 0 { wins as f64 / total_trades as f64 * 100.0 } else { 0.0 };
        let avg_win = if wins > 0 { gross_profit / wins as f64 } else { 0.0 };
        let avg_loss = if losses > 0 { gross_loss / losses as f64 } else { 0.0 };
        let profit_factor = if gross_loss > 0.0 { gross_profit / gross_loss } else { f64::INFINITY };

        let mut symbol_perf: Vec<SymbolPerformance> = by_symbol.into_iter()
            .map(|(symbol, (trades, w, pnl))| SymbolPerformance {
                win_rate_pct: if trades > 0 { w as f64 / trades as f64 * 100.0 } else { 0.0 },
                symbol, trades, wins: w, pnl_usd: pnl,
            })
            .collect();
        symbol_perf.sort_by(|a, b| b.pnl_usd.partial_cmp(&a.pnl_usd).unwrap_or(std::cmp::Ordering::Equal));

        PerformanceSummary {
            total_trades, wins, losses, win_rate_pct: win_rate,
            total_pnl_usd: total_pnl, avg_win_usd: avg_win, avg_loss_usd: avg_loss,
            profit_factor, largest_win_usd: largest_win, largest_loss_usd: largest_loss,
            current_streak, by_symbol: symbol_perf,
        }
    }

    /// Compute risk assessment from open positions + current market data.
    pub async fn get_risk_assessment(&self) -> RiskAssessment {
        let positions = self.positions.read().await;
        let tickers = self.latest_tickers.read().await;
        let meta = self.meta.read().await;

        let mut total_unrealized = 0.0f64;
        let mut pos_risks = Vec::new();

        for pos in positions.values() {
            let current_price = tickers.get(&pos.symbol)
                .map(|t| (t.bid_price + t.ask_price) / 2.0)
                .unwrap_or(pos.avg_entry);

            let distance_to_tp = if pos.tp_price > 0.0 && current_price > 0.0 {
                ((pos.tp_price - current_price) / current_price * 100.0).abs()
            } else { 0.0 };

            let distance_to_sl = if pos.sl_price > 0.0 && current_price > 0.0 {
                ((pos.sl_price - current_price) / current_price * 100.0).abs()
            } else { 0.0 };

            total_unrealized += pos.unrealized_pnl;

            pos_risks.push(PositionRisk {
                symbol: pos.symbol.clone(),
                side: pos.side.clone(),
                entry_price: pos.avg_entry,
                current_price,
                unrealized_pnl: pos.unrealized_pnl,
                unrealized_pnl_pct: pos.unrealized_pnl_pct,
                distance_to_tp_pct: distance_to_tp,
                distance_to_sl_pct: distance_to_sl,
                strategy_name: pos.strategy_name.clone(),
            });
        }

        // Sort by worst PnL first
        pos_risks.sort_by(|a, b| a.unrealized_pnl.partial_cmp(&b.unrealized_pnl).unwrap_or(std::cmp::Ordering::Equal));

        let balance = meta.balance;
        let total_pnl_pct = if balance > 0.0 { total_unrealized / balance * 100.0 } else { 0.0 };

        RiskAssessment {
            balance,
            total_unrealized_pnl: total_unrealized,
            total_unrealized_pnl_pct: total_pnl_pct,
            open_positions: pos_risks.len(),
            positions: pos_risks,
        }
    }

    /// Compare all symbols side-by-side: performance + current state.
    pub async fn get_symbol_comparison(&self) -> Vec<SymbolComparison> {
        let fills = self.recent_fills.read().await;
        let positions = self.positions.read().await;

        let mut by_symbol: HashMap<String, (usize, usize, f64)> = HashMap::new();
        for fill in fills.iter().filter(|f| f.position_closed && f.profit_usd.is_some()) {
            let e = by_symbol.entry(fill.symbol.clone()).or_insert((0, 0, 0.0));
            e.0 += 1;
            let pnl = fill.profit_usd.unwrap_or(0.0);
            e.2 += pnl;
            if pnl >= 0.0 { e.1 += 1; }
        }

        // Include symbols that have positions but no closed trades yet. A
        // symbol can have more than one open position (one per strategy
        // instance/rung trading it), so this keys on the snapshot's own
        // `symbol` field rather than the `(strategy_name, symbol)` map key.
        for pos in positions.values() {
            by_symbol.entry(pos.symbol.clone()).or_insert((0, 0, 0.0));
        }

        let mut comparisons: Vec<SymbolComparison> = by_symbol.into_iter()
            .map(|(symbol, (trades, wins, pnl))| {
                // Aggregate across every rung currently open on this symbol.
                let matching: Vec<&PositionSnapshot> =
                    positions.values().filter(|p| p.symbol == symbol).collect();
                let unrealized_pnl: f64 = matching.iter().map(|p| p.unrealized_pnl).sum();
                SymbolComparison {
                    win_rate_pct: if trades > 0 { wins as f64 / trades as f64 * 100.0 } else { 0.0 },
                    avg_pnl_usd: if trades > 0 { pnl / trades as f64 } else { 0.0 },
                    has_position: !matching.is_empty(),
                    unrealized_pnl,
                    symbol, trades, wins, pnl_usd: pnl,
                }
            })
            .collect();

        comparisons.sort_by(|a, b| b.pnl_usd.partial_cmp(&a.pnl_usd).unwrap_or(std::cmp::Ordering::Equal));
        comparisons
    }
}

/// Sync, non-blocking position access for the HTTP API. The impl lives
/// here (not in `tradectl-core`) because both the trait and `BotState` are
/// SDK-owned — the orphan rule forbids implementing it downstream.
impl crate::reader::PositionReader for BotState {
    fn positions(&self) -> Vec<PositionSnapshot> {
        self.try_get_positions()
    }
}

#[cfg(test)]
mod position_tests {
    use super::*;

    fn snap(strategy_name: &str, symbol: &str, qty: f64, sl: f64) -> PositionSnapshot {
        PositionSnapshot {
            symbol: symbol.to_string(),
            side: "SHORT".to_string(),
            avg_entry: 1.0,
            quantity: qty,
            entry_count: 1,
            unrealized_pnl: 0.0,
            unrealized_pnl_pct: 0.0,
            tp_price: 0.0,
            sl_price: sl,
            virtual_sl: false,
            sl_order_id: String::new(),
            tp_order_id: String::new(),
            strategy_name: strategy_name.to_string(),
            emulated: false,
            timestamp_ms: 0,
        }
    }

    /// A `StratId` for a test. Production code cannot do this — the whole
    /// point of the newtype — but a test needs to name instances somehow.
    fn sid(id: &str) -> StratId {
        StratId::from_validated(id)
    }

    /// The regression this module exists to prevent: two strategy
    /// instances ("rungs") trading the same symbol at once must not
    /// clobber each other's position snapshot. Before this fix, keying on
    /// bare symbol meant the second `update_position` call silently
    /// erased the first rung's entry — undercounting `/v1/status`'s
    /// position_count and dropping rows from `/v1/intent`, including any
    /// virtual stop-loss the erased rung had armed.
    #[tokio::test]
    async fn concurrent_rungs_on_one_symbol_do_not_clobber_each_other() {
        let state = BotState::new();
        state.update_position(&sid("225S"), "XANUSDT", Some(snap("225S", "XANUSDT", 715.0, 0.0))).await;
        state.update_position(&sid("30S"), "XANUSDT", Some(snap("30S", "XANUSDT", 990.0, 0.01138))).await;

        let all = state.try_get_positions();
        assert_eq!(all.len(), 2, "both rungs must survive, not just the last-written one");
        let rung_30s = all.iter().find(|p| p.strategy_name == "30S").expect("30S rung missing");
        assert_eq!(rung_30s.sl_price, 0.01138, "the virtual SL price must not be lost");
    }

    /// Removing one rung's position (its own close) must not remove a
    /// different rung still open on the same symbol.
    #[tokio::test]
    async fn removing_one_rung_leaves_the_others_on_the_same_symbol() {
        let state = BotState::new();
        state.update_position(&sid("225S"), "XANUSDT", Some(snap("225S", "XANUSDT", 715.0, 0.0))).await;
        state.update_position(&sid("30S"), "XANUSDT", Some(snap("30S", "XANUSDT", 990.0, 0.01138))).await;

        state.update_position(&sid("225S"), "XANUSDT", None).await;

        let all = state.try_get_positions();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].strategy_name, "30S");
    }

    /// Two instances of ONE strategy on one symbol — same `name`, different
    /// ids. This is the case the previous `(name, symbol)` key could not
    /// represent, and the case every test above misses by using distinct
    /// names.
    ///
    /// Prod, 2026-08-17, BTCUSD_PERP: two same-named instances shared a leg.
    /// The first to close called `update_position(name, sym, None)` and took
    /// the survivor's snapshot with it. The bot was still enforcing a virtual
    /// stop for that position, but a virtual stop rests nowhere on the
    /// exchange — the snapshot IS the declaration — so the watchdog saw a live
    /// position with no stop and closed it after `sl_grace_alive`.
    #[tokio::test]
    async fn same_named_instances_on_one_symbol_are_separate_positions() {
        let state = BotState::new();
        let mut a = snap("shot-emu", "BTCUSD_PERP", 63_500.0, 62_000.0);
        a.virtual_sl = true;
        let mut b = snap("shot-emu", "BTCUSD_PERP", 63_500.0, 61_000.0);
        b.virtual_sl = true;
        state.update_position(&sid("st_aaaa"), "BTCUSD_PERP", Some(a)).await;
        state.update_position(&sid("st_bbbb"), "BTCUSD_PERP", Some(b)).await;
        assert_eq!(state.try_get_positions().len(), 2, "a shared name is not a shared position");

        // One instance goes flat. The other still holds, and still declares.
        state.update_position(&sid("st_aaaa"), "BTCUSD_PERP", None).await;

        let left = state.try_get_positions();
        assert_eq!(left.len(), 1, "closing one same-named instance must not erase its sibling");
        assert!(left[0].virtual_sl, "the survivor's virtual stop must still be declared");
        assert_eq!(left[0].sl_price, 61_000.0, "and at ITS level, not the closed one's");
    }

    /// `is_emulator` is a property of a config ENTRY, so a paper instance must
    /// not stamp a live same-named sibling's positions `emulated` — the
    /// watchdog link drops emulated rows from the declaration entirely, which
    /// would leave a real position undeclared.
    #[tokio::test]
    async fn an_emulator_does_not_stamp_its_same_named_live_sibling() {
        let state = BotState::new();
        state.set_emulators(HashSet::from([sid("st_paper")]));

        state.update_position(&sid("st_paper"), "BTCUSD_PERP",
            Some(snap("shot-emu", "BTCUSD_PERP", 1.0, 0.5))).await;
        state.update_position(&sid("st_live"), "BTCUSD_PERP",
            Some(snap("shot-emu", "BTCUSD_PERP", 1.0, 0.5))).await;

        let all = state.try_get_positions();
        assert_eq!(all.iter().filter(|p| p.emulated).count(), 1, "only the paper entry is paper");
        assert_eq!(all.iter().filter(|p| !p.emulated).count(), 1, "the live one stays declarable");
    }

    /// `get_positions(Some(symbol))` must return every rung open on that
    /// symbol, not the single surviving entry a bare-symbol key would have
    /// forced.
    #[tokio::test]
    async fn get_positions_by_symbol_returns_every_rung() {
        let state = BotState::new();
        state.update_position(&sid("225S"), "XANUSDT", Some(snap("225S", "XANUSDT", 715.0, 0.0))).await;
        state.update_position(&sid("30S"), "XANUSDT", Some(snap("30S", "XANUSDT", 990.0, 0.01138))).await;
        state.update_position(&sid("08S"), "ETHUSDT", Some(snap("08S", "ETHUSDT", 1.0, 0.0))).await;

        let xan = state.get_positions(Some("XANUSDT")).await;
        assert_eq!(xan.len(), 2);
        assert!(xan.iter().all(|p| p.symbol == "XANUSDT"));

        let eth = state.get_positions(Some("ETHUSDT")).await;
        assert_eq!(eth.len(), 1);
    }

    /// `get_symbol_comparison` aggregates unrealized P&L across every rung
    /// on a symbol instead of reflecting only the last-written one.
    #[tokio::test]
    async fn symbol_comparison_aggregates_across_rungs() {
        let state = BotState::new();
        let mut a = snap("225S", "XANUSDT", 715.0, 0.0);
        a.unrealized_pnl = -1.5;
        let mut b = snap("30S", "XANUSDT", 990.0, 0.01138);
        b.unrealized_pnl = 2.0;
        state.update_position(&sid("225S"), "XANUSDT", Some(a)).await;
        state.update_position(&sid("30S"), "XANUSDT", Some(b)).await;

        let comparisons = state.get_symbol_comparison().await;
        let xan = comparisons.iter().find(|c| c.symbol == "XANUSDT").expect("XANUSDT missing");
        assert!(xan.has_position);
        assert!((xan.unrealized_pnl - 0.5).abs() < 1e-9);
    }
}
