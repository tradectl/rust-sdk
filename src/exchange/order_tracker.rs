use std::collections::HashMap;
use crate::types::Order;

pub type OrderEventCallback = Box<dyn Fn(&Order) + Send + Sync>;

/// In-memory order state manager. Tracks orders across all trading pairs.
/// Exchange-specific fill detection is handled by the exchange implementation,
/// which calls the mutation and emit methods on this store.
pub struct OrderTracker {
    orders: HashMap<String, HashMap<String, Order>>,
    fill_callbacks: Vec<OrderEventCallback>,
    complete_callbacks: Vec<OrderEventCallback>,
    partial_fill_callbacks: Vec<OrderEventCallback>,
}

impl OrderTracker {
    pub fn new() -> Self {
        Self {
            orders: HashMap::new(),
            fill_callbacks: Vec::new(),
            complete_callbacks: Vec::new(),
            partial_fill_callbacks: Vec::new(),
        }
    }

    // ── Query ────────────────────────────────────────────────────

    pub fn get_order(&self, symbol: &str, client_order_id: &str) -> Option<&Order> {
        self.orders.get(symbol)?.get(client_order_id)
    }

    pub fn get_order_mut(&mut self, symbol: &str, client_order_id: &str) -> Option<&mut Order> {
        self.orders.get_mut(symbol)?.get_mut(client_order_id)
    }

    pub fn get_orders_by_symbol(&self, symbol: &str) -> Option<&HashMap<String, Order>> {
        self.orders.get(symbol)
    }

    pub fn get_all_orders(&self) -> &HashMap<String, HashMap<String, Order>> {
        &self.orders
    }

    pub fn has_open_orders(&self, symbol: &str) -> bool {
        self.orders
            .get(symbol)
            .is_some_and(|m| !m.is_empty())
    }

    // ── Mutation ─────────────────────────────────────────────────

    pub fn track_order(&mut self, order: Order) {
        let key = order
            .client_order_id
            .clone()
            .unwrap_or_else(|| order.order_id.clone());
        self.orders
            .entry(order.symbol.clone())
            .or_default()
            .insert(key, order);
    }

    pub fn remove_order(&mut self, symbol: &str, client_order_id: &str) -> bool {
        let Some(symbol_map) = self.orders.get_mut(symbol) else {
            return false;
        };
        let deleted = symbol_map.remove(client_order_id).is_some();
        if symbol_map.is_empty() {
            self.orders.remove(symbol);
        }
        deleted
    }

    pub fn clear(&mut self) {
        self.orders.clear();
    }

    // ── Event Callbacks ──────────────────────────────────────────

    pub fn on_fill(&mut self, cb: OrderEventCallback) {
        self.fill_callbacks.push(cb);
    }

    pub fn on_complete(&mut self, cb: OrderEventCallback) {
        self.complete_callbacks.push(cb);
    }

    pub fn on_partial_fill(&mut self, cb: OrderEventCallback) {
        self.partial_fill_callbacks.push(cb);
    }

    pub fn emit_fill(&self, order: &Order) {
        for cb in &self.fill_callbacks {
            cb(order);
        }
    }

    pub fn emit_complete(&self, order: &Order) {
        for cb in &self.complete_callbacks {
            cb(order);
        }
    }

    pub fn emit_partial_fill(&self, order: &Order) {
        for cb in &self.partial_fill_callbacks {
            cb(order);
        }
    }
}

impl Default for OrderTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{OrderSide, OrderStatus, OrderType};

    fn make_order(symbol: &str, order_id: &str, client_id: Option<&str>) -> Order {
        Order {
            symbol: symbol.to_string(),
            order_id: order_id.to_string(),
            client_order_id: client_id.map(|s| s.to_string()),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            status: OrderStatus::New,
            price: 50000.0,
            execution_price: 0.0,
            exit_price: 0.0,
            quantity: 1.0,
            filled_quantity: 0.0,
            last_filled_quantity: 0.0,
            profit: 0.0,
            profit_usd: 0.0,
            take_profit_price: 0.0,
            stop_loss_price: 0.0,
            opened_at: 1000,
            closed_at: None,
            commission: 0.0,
            commission_asset: None,
        }
    }

    #[test]
    fn track_and_get() {
        let mut tracker = OrderTracker::new();
        let order = make_order("BTCUSDT", "ORD-1", Some("CLIENT-1"));
        tracker.track_order(order);

        assert!(tracker.get_order("BTCUSDT", "CLIENT-1").is_some());
        assert!(tracker.get_order("BTCUSDT", "WRONG").is_none());
    }

    #[test]
    fn track_by_order_id_when_no_client_id() {
        let mut tracker = OrderTracker::new();
        let order = make_order("BTCUSDT", "ORD-1", None);
        tracker.track_order(order);

        assert!(tracker.get_order("BTCUSDT", "ORD-1").is_some());
    }

    #[test]
    fn has_open_orders() {
        let mut tracker = OrderTracker::new();
        assert!(!tracker.has_open_orders("BTCUSDT"));

        tracker.track_order(make_order("BTCUSDT", "ORD-1", Some("C1")));
        assert!(tracker.has_open_orders("BTCUSDT"));
        assert!(!tracker.has_open_orders("ETHUSDT"));
    }

    #[test]
    fn remove_order() {
        let mut tracker = OrderTracker::new();
        tracker.track_order(make_order("BTCUSDT", "ORD-1", Some("C1")));
        tracker.track_order(make_order("BTCUSDT", "ORD-2", Some("C2")));

        assert!(tracker.remove_order("BTCUSDT", "C1"));
        assert!(tracker.has_open_orders("BTCUSDT"));

        assert!(tracker.remove_order("BTCUSDT", "C2"));
        assert!(!tracker.has_open_orders("BTCUSDT"));
    }

    #[test]
    fn clear_all() {
        let mut tracker = OrderTracker::new();
        tracker.track_order(make_order("BTCUSDT", "ORD-1", Some("C1")));
        tracker.track_order(make_order("ETHUSDT", "ORD-2", Some("C2")));
        tracker.clear();
        assert!(!tracker.has_open_orders("BTCUSDT"));
        assert!(!tracker.has_open_orders("ETHUSDT"));
    }

    #[test]
    fn emit_callbacks() {
        use std::sync::{Arc, Mutex};

        let mut tracker = OrderTracker::new();
        let fills = Arc::new(Mutex::new(Vec::new()));
        let fills_clone = fills.clone();

        tracker.on_fill(Box::new(move |order| {
            fills_clone.lock().unwrap().push(order.order_id.clone());
        }));

        let order = make_order("BTCUSDT", "ORD-1", Some("C1"));
        tracker.emit_fill(&order);

        assert_eq!(fills.lock().unwrap().len(), 1);
        assert_eq!(fills.lock().unwrap()[0], "ORD-1");
    }
}
