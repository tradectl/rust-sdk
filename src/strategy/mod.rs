use crate::types::{TickerEvent, TradeEvent, Side, Params, ParamDef};

pub enum Action {
    /// Do nothing.
    Hold,
    /// Open a market position.
    MarketOpen { side: Side, size: Option<f64> },
    /// Place a limit order.
    LimitOpen { side: Side, price: f64, size: Option<f64> },
    /// Close a specific position at market.
    ClosePosition { position_id: u64 },
    /// Close all positions at market.
    CloseAll,
    /// Cancel pending limit order.
    CancelPending,
}

/// Read-only snapshot of an open position, provided by the engine.
pub struct PositionInfo {
    pub id: u64,
    pub side: Side,
    pub entry_price: f64,
    pub quantity: f64,
    pub unrealized_pnl: f64,
    pub entry_time: u64,
}

/// Context provided to the strategy on every event.
pub struct StrategyContext<'a> {
    pub timestamp_ms: u64,
    pub book: Option<&'a TickerEvent>,
    pub positions: &'a [PositionInfo],
    pub balance: f64,
    pub unrealized_pnl: f64,
    pub realized_pnl: f64,
    pub trade_count: usize,
}

/// A price line to display on the monitor chart.
/// Strategies return these from `monitor_snapshot` — the runner passes them
/// through without interpretation.
#[derive(Clone, Debug, serde::Serialize)]
pub struct PriceLine {
    pub label: String,
    pub price: f64,
    pub color: String,
    /// "solid", "dashed", or "dotted"
    pub style: String,
    pub line_width: u8,
    pub axis_label: bool,
}

/// Monitor snapshot returned by a strategy — generic price lines + arbitrary state.
#[derive(Default)]
pub struct MonitorSnapshot {
    /// Price lines to render on the chart (TP, SL, entry, limit, corridor, etc.).
    pub price_lines: Vec<PriceLine>,
    /// Strategy-specific state for the stats/info panel (arbitrary JSON).
    pub state: serde_json::Value,
}

/// Reason a position was closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    TakeProfit,
    StopLoss,
    ForceClose,
}

/// Information about a closed position, passed to `Strategy::on_position_close`.
pub struct CloseInfo {
    pub symbol: String,
    pub side: Side,
    pub entry_price: f64,
    pub close_price: f64,
    pub quantity: f64,
    pub profit_pct: f64,
    pub profit_usd: f64,
    pub reason: CloseReason,
}

/// Core strategy trait. Strategies implement this to participate in backtesting
/// and live trading. The engine calls `on_ticker` / `on_trade` and executes
/// the returned `Action`.
pub trait Strategy: Send {
    /// Called on every ticker event. Default: Hold.
    fn on_ticker(&mut self, ticker: &TickerEvent, ctx: &StrategyContext) -> Action {
        let _ = (ticker, ctx);
        Action::Hold
    }

    /// Called on every trade event. Default: Hold.
    fn on_trade(&mut self, trade: &TradeEvent, ctx: &StrategyContext) -> Action {
        let _ = (trade, ctx);
        Action::Hold
    }

    /// Strategy name (for registry/CLI).
    fn name(&self) -> &str;

    /// Human description.
    fn describe(&self) -> &str {
        ""
    }

    /// Parameter schema for UI/validation/sweep ranges.
    fn params_schema(&self) -> Vec<ParamDef> {
        vec![]
    }

    /// Called when a position is closed (TP, SL, or force-close).
    /// Default: no-op. Override to hook into session management, analytics, etc.
    fn on_position_close(&mut self, close: &CloseInfo, ctx: &StrategyContext) {
        let _ = (close, ctx);
    }

    /// Return a monitor snapshot with price lines and arbitrary state.
    /// The engine passes this through to the monitor UI without interpretation.
    fn monitor_snapshot(&self, _ctx: &StrategyContext, _ticker: &TickerEvent) -> MonitorSnapshot {
        MonitorSnapshot::default()
    }
}

/// Factory function type for creating strategy instances from parameters.
pub type StrategyFactory = fn(&Params) -> Box<dyn Strategy>;
