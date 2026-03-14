use crate::types::enums::{OrderSide, OrderStatus, OrderType, TimeInForce};

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
    pub filled_quantity: f64,
    pub profit: f64,
    pub profit_usd: f64,
    pub take_profit_price: f64,
    pub stop_loss_price: f64,
    pub opened_at: u64,
    pub closed_at: Option<u64>,
}
