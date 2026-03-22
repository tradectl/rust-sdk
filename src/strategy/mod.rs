use crate::types::{TickerEvent, TradeEvent, Side, Params, ParamDef};

// ---------------------------------------------------------------------------
// Order / Exit types
// ---------------------------------------------------------------------------

/// Whether an entry order is market or limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderKind {
    Market,
    Limit,
}

/// How an exit order executes on the exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitType {
    /// Limit order — fills at `price` or better. Used for take-profit.
    Limit,
    /// Stop-market order — triggers at `price`, fills at market. Used for stop-loss.
    Stop,
}

/// A single exit order declared by the strategy.
///
/// The runner tracks exits by `id` (strategy-assigned). On `SetExits`, exits
/// with the same `id` are diffed: unchanged → skip, changed → edit, missing →
/// cancel, new → place. Already-filled IDs are never re-placed.
#[derive(Debug, Clone)]
pub struct ExitOrder {
    /// Strategy-assigned identifier, e.g. "tp1", "sl".
    pub id: String,
    /// Target price (limit price for Limit, trigger price for Stop).
    pub price: f64,
    /// Quantity to exit.
    pub size: f64,
    /// Execution type (limit or stop-market).
    pub kind: ExitType,
    /// Delay in milliseconds before the runner places this order.
    /// 0 = immediate. Typical SL delay: 3000.
    pub delay_ms: u64,
}

// ---------------------------------------------------------------------------
// Action enum
// ---------------------------------------------------------------------------

/// Action returned by a strategy to the engine.
///
/// The engine (live runner or backtest) executes the action and reports fills
/// back through `Strategy::on_fill`.
pub enum Action {
    /// Do nothing.
    Hold,

    // -- Entries --

    /// Place an entry order (market or limit) with optional exit orders.
    /// Exits are placed after the entry fills (via `on_fill` response or
    /// directly if declared here).
    PlaceEntry {
        side: Side,
        /// `None` for market orders, `Some(price)` for limit orders.
        price: Option<f64>,
        size: f64,
        kind: OrderKind,
        /// Exit orders to place after this entry fills.
        exits: Vec<ExitOrder>,
    },
    /// Edit a pending entry order (change price and/or size).
    EditEntry {
        order_id: String,
        price: Option<f64>,
        size: Option<f64>,
    },
    /// Cancel specific entry orders by ID.
    CancelEntry { order_ids: Vec<String> },

    // -- Exits (declarative + imperative) --

    /// Declarative: replace all exit orders with this set.
    /// The runner diffs by `ExitOrder.id` — unchanged exits are not touched,
    /// changed exits are edited, missing exits are cancelled, new exits are
    /// placed. Already-filled exit IDs are skipped.
    SetExits { exits: Vec<ExitOrder> },
    /// Imperative: add a single exit order.
    AddExit { exit: ExitOrder },
    /// Imperative: update an existing exit order (matched by `exit.id`).
    UpdateExit { exit: ExitOrder },
    /// Imperative: remove an exit order by ID.
    RemoveExit { id: String },

    // -- Position --

    /// Market-close the full position and cancel all exit orders.
    CloseAll,
}

// ---------------------------------------------------------------------------
// Position / context
// ---------------------------------------------------------------------------

/// Read-only snapshot of the current position on a symbol, provided by the engine.
///
/// Follows the Binance position model: one accumulated position per symbol with
/// weighted-average entry price.
pub struct PositionInfo {
    pub side: Side,
    /// Weighted-average entry price across all fills.
    pub avg_entry: f64,
    /// Current remaining quantity (decreases on exit fills).
    pub quantity: f64,
    /// Sum of all entry fill quantities (does not decrease on exits).
    pub total_entered: f64,
    /// Number of entry fills in this position cycle.
    pub entry_count: usize,
    /// Price of the most recent entry fill.
    pub last_entry_price: f64,
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
    /// Configured trading direction for this strategy instance.
    pub direction: Side,
}

// ---------------------------------------------------------------------------
// Fill event / response
// ---------------------------------------------------------------------------

/// Information about a filled order, passed to `Strategy::on_fill`.
///
/// Covers entry fills, partial exit fills, and full position closes.
pub struct FillEvent {
    /// Exchange order ID of the filled order.
    pub order_id: String,
    /// Symbol that was filled (e.g. "BTCUSDT").
    pub symbol: String,
    /// Fill price.
    pub price: f64,
    /// Fill quantity.
    pub quantity: f64,
    /// `true` if this fill adds to the position (entry).
    pub is_entry: bool,
    /// `true` if the order was only partially filled (more quantity pending).
    pub is_partial: bool,
    /// For exit fills: which `ExitOrder.id` triggered this fill.
    pub exit_id: Option<String>,
    /// `true` if this fill brought the net position quantity to zero.
    pub position_closed: bool,
}

/// Response from `Strategy::on_fill`.
///
/// Contains follow-up actions (e.g. `SetExits` after an entry fill) and
/// whether the runner should send a Telegram notification for this fill.
pub struct FillResponse {
    /// Follow-up actions to execute (e.g. set exits, cancel orders).
    pub actions: Vec<Action>,
    /// If `true`, the runner sends a default-formatted Telegram notification.
    pub notify: bool,
}

// ---------------------------------------------------------------------------
// Monitor
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Strategy trait
// ---------------------------------------------------------------------------

/// Core strategy trait. Strategies implement this to participate in backtesting
/// and live trading.
///
/// The engine calls `on_ticker` / `on_trade` on every market event and executes
/// the returned `Action`. When an order fills, the engine calls `on_fill` and
/// processes the returned `FillResponse` actions.
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

    /// Called when an order fills (entry or exit, partial or full).
    ///
    /// Return follow-up actions (e.g. `SetExits` after entry) and whether
    /// the runner should send a Telegram notification.
    fn on_fill(&mut self, fill: &FillEvent, ctx: &StrategyContext) -> FillResponse {
        let _ = (fill, ctx);
        FillResponse { actions: vec![], notify: true }
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

    /// Return a monitor snapshot with price lines and arbitrary state.
    /// The engine passes this through to the monitor UI without interpretation.
    fn monitor_snapshot(&self, _ctx: &StrategyContext, _ticker: &TickerEvent) -> MonitorSnapshot {
        MonitorSnapshot::default()
    }
}

// ---------------------------------------------------------------------------
// Plugin ABI
// ---------------------------------------------------------------------------

/// Factory function type for creating strategy instances from parameters.
pub type StrategyFactory = fn(&Params) -> Box<dyn Strategy>;

/// ABI-stable struct returned by strategy dylibs.
///
/// The CLI loads a `.so`/`.dylib`, calls the exported `tradectl_strategy()`
/// function, and gets this struct containing the strategy name and factory.
#[repr(C)]
pub struct StrategyPlugin {
    /// ABI version — bump on breaking changes to the Strategy trait.
    pub abi_version: u32,
    /// Pointer to the strategy name (UTF-8 bytes).
    pub name: *const u8,
    /// Length of the strategy name in bytes.
    pub name_len: usize,
    /// Factory function that creates strategy instances from parameters.
    pub factory: StrategyFactory,
}

/// Current ABI version for strategy plugins.
pub const STRATEGY_ABI_VERSION: u32 = 2;

// Safety: StrategyPlugin is constructed at load time and used from a single thread.
unsafe impl Send for StrategyPlugin {}
unsafe impl Sync for StrategyPlugin {}

/// Declare this crate as a tradectl strategy plugin.
///
/// Call once in your `lib.rs`:
/// ```rust,ignore
/// tradectl_sdk::declare_strategy!("bounce-back", BounceBack::new);
/// ```
///
/// This exports a C-compatible entry point that the `tradectl` CLI loads at runtime.
#[macro_export]
macro_rules! declare_strategy {
    ($name:expr, $factory:expr) => {
        #[no_mangle]
        pub extern "C" fn tradectl_strategy() -> $crate::strategy::StrategyPlugin {
            const NAME: &[u8] = $name.as_bytes();
            $crate::strategy::StrategyPlugin {
                abi_version: $crate::strategy::STRATEGY_ABI_VERSION,
                name: NAME.as_ptr(),
                name_len: NAME.len(),
                factory: |params| ::std::boxed::Box::new($factory(params)),
            }
        }
    };
}
