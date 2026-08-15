use crate::types::enums::{OrderSide, OrderStatus, OrderType, Side, TimeInForce};

#[derive(Debug, Clone)]
pub struct OrderRequest {
    pub symbol: String,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub quantity: f64,
    pub price: Option<f64>,
    pub stop_price: Option<f64>,
    pub time_in_force: Option<TimeInForce>,
    pub client_order_id: Option<String>,
    pub reduce_only: Option<bool>,
    /// Hedge mode position side. `None` = one-way mode (omit param),
    /// `Some(Long/Short)` = hedge mode (`positionSide=LONG/SHORT`).
    pub position_side: Option<Side>,
}

#[derive(Debug, Clone)]
pub struct Order {
    pub symbol: String,
    pub order_id: String,
    pub client_order_id: Option<String>,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub status: OrderStatus,
    pub price: f64,
    pub execution_price: f64,
    pub exit_price: f64,
    pub quantity: f64,
    /// Cumulative filled quantity (Binance `z` field).
    pub filled_quantity: f64,
    /// Quantity filled in this specific event (Binance `l` field).
    pub last_filled_quantity: f64,
    pub profit: f64,
    pub profit_usd: f64,
    pub take_profit_price: f64,
    pub stop_loss_price: f64,
    pub opened_at: u64,
    pub closed_at: Option<u64>,
    /// Actual commission charged by the exchange on this fill.
    pub commission: f64,
    /// Asset in which commission was charged (e.g. "BNB", "USDT", "SOL").
    pub commission_asset: Option<String>,
    /// Which hedge-mode position this order acts on (Binance `ps`). `None` on a
    /// one-way account, and on any venue that does not report it.
    ///
    /// It is what makes a *close* distinguishable from a sibling strategy's
    /// opposite-direction *entry*: in hedge mode a Sell naming LONG can only
    /// reduce a long, while a Sell naming SHORT opens one. The order-update
    /// callback is filtered by symbol alone, so without this the runner cannot
    /// safely reconcile an unknown order that flattened its position — and a
    /// position closed by a watchdog, by hand, or by ADL stays on its books
    /// forever (2026-08-15, BTCUSD_PERP).
    pub position_side: Option<Side>,
}
