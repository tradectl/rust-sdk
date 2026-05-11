use std::collections::HashMap;
use crate::types::Order;
use crate::strategy::ExitOrder;

pub type OrderEventCallback = Box<dyn Fn(&Order) + Send + Sync>;

/// Per-entry runner-side metadata kept alongside the raw `Order` in the tracker.
/// This is what was previously fragmented across `entry_exits`, `entry_prices`,
/// `filled_entry_cids`, and `active_entries` in the live runner.
#[derive(Clone, Debug, Default)]
pub struct EntryMetadata {
    /// Multi-slot strategy: which slot id this entry occupies
    /// (`"_"` for single-slot mode).
    pub slot: Option<String>,
    /// Exits (TP/SL) to place once the entry fills.
    pub pending_exits: Vec<ExitOrder>,
    /// Has `on_fill` already been dispatched for this entry?
    /// (Race-fill guard.)
    pub fire_once_fill: bool,
    /// Cumulative filled quantity reported by the exchange; used to derive
    /// per-event fill delta against amendment echoes.
    pub cum_filled_qty: f64,
    /// Entry price recorded at placement time (used by chase-edit logic).
    pub entry_price: f64,
}

/// In-memory order state manager. Tracks orders across all trading pairs.
/// Exchange-specific fill detection is handled by the exchange implementation,
/// which calls the mutation and emit methods on this store.
pub struct OrderTracker {
    orders: HashMap<String, HashMap<String, Order>>,
    entry_metadata: HashMap<String, EntryMetadata>,
    fill_callbacks: Vec<OrderEventCallback>,
    complete_callbacks: Vec<OrderEventCallback>,
    partial_fill_callbacks: Vec<OrderEventCallback>,
}

impl OrderTracker {
    pub fn new() -> Self {
        Self {
            orders: HashMap::new(),
            entry_metadata: HashMap::new(),
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
        self.entry_metadata.clear();
    }

    // ── Entry tracking (runner-side) ─────────────────────────────

    /// Track an entry order with its runner-side metadata.
    pub fn track_entry(&mut self, order: Order, metadata: EntryMetadata) {
        let cid = order
            .client_order_id
            .clone()
            .unwrap_or_else(|| order.order_id.clone());
        self.entry_metadata.insert(cid.clone(), metadata);
        self.orders
            .entry(order.symbol.clone())
            .or_default()
            .insert(cid, order);
    }

    /// Is this cid known as a tracked entry (regardless of fill status)?
    pub fn contains_entry(&self, client_order_id: &str) -> bool {
        self.entry_metadata.contains_key(client_order_id)
    }

    /// Read-only access to entry metadata.
    pub fn get_entry_metadata(&self, client_order_id: &str) -> Option<&EntryMetadata> {
        self.entry_metadata.get(client_order_id)
    }

    /// Mutable access to entry metadata for in-place updates
    /// (e.g. updating `cum_filled_qty`).
    pub fn get_entry_metadata_mut(&mut self, client_order_id: &str) -> Option<&mut EntryMetadata> {
        self.entry_metadata.get_mut(client_order_id)
    }

    /// Mark an entry as having dispatched `on_fill`. Returns `true` if this is
    /// the first call (i.e. the caller should run the fill handler) and
    /// `false` if `on_fill` has already been dispatched for this cid.
    pub fn mark_filled(&mut self, client_order_id: &str) -> bool {
        match self.entry_metadata.get_mut(client_order_id) {
            Some(m) if !m.fire_once_fill => {
                m.fire_once_fill = true;
                true
            }
            _ => false,
        }
    }

    /// Remove an entry and its metadata. Returns `true` when both the order
    /// and the metadata were present (the normal case for a tracked entry);
    /// `false` when neither was present. A return of `true` from only one
    /// side would indicate the two maps had drifted — they are maintained
    /// in lockstep by `track_entry`, so that should not happen in practice.
    pub fn remove_entry(&mut self, symbol: &str, client_order_id: &str) -> bool {
        let order_removed = self.remove_order(symbol, client_order_id);
        let meta_removed = self.entry_metadata.remove(client_order_id).is_some();
        debug_assert_eq!(
            order_removed, meta_removed,
            "OrderTracker entry maps drifted for cid {client_order_id}",
        );
        order_removed && meta_removed
    }

    /// Number of entries currently tracked across all symbols.
    pub fn entry_count(&self) -> usize {
        self.entry_metadata.len()
    }

    /// All tracked entry cids (for iteration / cap-check / shutdown sweeps).
    pub fn entry_cids(&self) -> impl Iterator<Item = &String> {
        self.entry_metadata.keys()
    }

    /// Find the cid of an entry by (symbol, slot). Returns `None` if no entry
    /// is currently tracked for that slot.
    pub fn entry_cid_for_slot(&self, symbol: &str, slot: &str) -> Option<String> {
        self.entry_metadata.iter()
            .find(|(cid, m)| {
                m.slot.as_deref() == Some(slot)
                    && self.orders.get(symbol).is_some_and(|sm| sm.contains_key(*cid))
            })
            .map(|(cid, _)| cid.clone())
    }

    /// All cids currently tracked as entries for a given symbol (regardless of slot).
    pub fn entry_cids_for_symbol(&self, symbol: &str) -> Vec<String> {
        self.entry_metadata.keys()
            .filter(|cid| {
                self.orders.get(symbol).is_some_and(|sm| sm.contains_key(*cid))
            })
            .cloned()
            .collect()
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
    fn track_entry_stores_metadata() {
        let mut tracker = OrderTracker::new();
        let order = make_order("BTCUSDT", "ORD-1", Some("CLIENT-1"));
        let meta = EntryMetadata {
            slot: Some("_".into()),
            pending_exits: vec![],
            fire_once_fill: false,
            cum_filled_qty: 0.0,
            entry_price: 50000.0,
        };
        tracker.track_entry(order, meta);

        assert!(tracker.contains_entry("CLIENT-1"));
        let got = tracker.get_entry_metadata("CLIENT-1").expect("metadata present");
        assert_eq!(got.slot.as_deref(), Some("_"));
        assert_eq!(got.entry_price, 50000.0);
    }

    #[test]
    fn remove_entry_drops_both_order_and_metadata() {
        let mut tracker = OrderTracker::new();
        tracker.track_entry(
            make_order("BTCUSDT", "ORD-1", Some("C1")),
            EntryMetadata::default(),
        );
        assert!(tracker.contains_entry("C1"));

        let removed = tracker.remove_entry("BTCUSDT", "C1");
        assert!(removed);
        assert!(!tracker.contains_entry("C1"));
        assert!(tracker.get_entry_metadata("C1").is_none());
    }

    #[test]
    fn entry_count_reflects_tracked_entries() {
        let mut tracker = OrderTracker::new();
        assert_eq!(tracker.entry_count(), 0);

        tracker.track_entry(make_order("BTCUSDT", "A", Some("CA")), EntryMetadata::default());
        tracker.track_entry(make_order("ETHUSDT", "B", Some("CB")), EntryMetadata::default());
        assert_eq!(tracker.entry_count(), 2);

        tracker.remove_entry("BTCUSDT", "CA");
        assert_eq!(tracker.entry_count(), 1);
    }

    #[test]
    fn mark_filled_sets_fire_once_and_returns_prior_state() {
        let mut tracker = OrderTracker::new();
        tracker.track_entry(
            make_order("BTCUSDT", "A", Some("CA")),
            EntryMetadata::default(),
        );

        let first = tracker.mark_filled("CA");
        assert!(first, "first mark_filled should report 'was not previously filled'");
        let second = tracker.mark_filled("CA");
        assert!(!second, "second mark_filled should report 'already filled'");
    }

    #[test]
    fn clear_drops_entry_metadata() {
        let mut tracker = OrderTracker::new();
        tracker.track_entry(
            make_order("BTCUSDT", "A", Some("CA")),
            EntryMetadata::default(),
        );
        assert_eq!(tracker.entry_count(), 1);

        tracker.clear();
        assert_eq!(tracker.entry_count(), 0);
        assert!(!tracker.contains_entry("CA"));
    }

    #[test]
    fn remove_entry_returns_false_when_absent() {
        let mut tracker = OrderTracker::new();
        assert!(!tracker.remove_entry("BTCUSDT", "nonexistent"));
    }

    #[test]
    fn entry_cid_for_slot_finds_by_symbol_and_slot() {
        let mut tracker = OrderTracker::new();
        let mut meta = EntryMetadata::default();
        meta.slot = Some("_".into());
        tracker.track_entry(make_order("BTCUSDT", "A", Some("CA")), meta);

        assert_eq!(tracker.entry_cid_for_slot("BTCUSDT", "_").as_deref(), Some("CA"));
        assert_eq!(tracker.entry_cid_for_slot("BTCUSDT", "other"), None);
        assert_eq!(tracker.entry_cid_for_slot("ETHUSDT", "_"), None);
    }

    #[test]
    fn entry_cids_for_symbol_returns_all_tracked_cids() {
        let mut tracker = OrderTracker::new();
        tracker.track_entry(make_order("BTCUSDT", "A", Some("CA")), EntryMetadata::default());
        tracker.track_entry(make_order("BTCUSDT", "B", Some("CB")), EntryMetadata::default());
        tracker.track_entry(make_order("ETHUSDT", "C", Some("CC")), EntryMetadata::default());

        let mut got = tracker.entry_cids_for_symbol("BTCUSDT");
        got.sort();
        assert_eq!(got, vec!["CA".to_string(), "CB".to_string()]);

        assert_eq!(tracker.entry_cids_for_symbol("SOLUSDT"), Vec::<String>::new());
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
