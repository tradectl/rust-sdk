use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use async_trait::async_trait;
use crate::types::{
    BookTicker, KlineData, MarketFees, MarketType, Order, OrderRequest, OrderSide, OrderStatus,
    OrderType, PairInfo, ProfitResult, Ticker24hr, TradeData,
    calculate_inverse_profit, calculate_linear_profit, calculate_spot_profit,
    InverseProfitParams, LinearProfitParams, SpotProfitParams,
};
use crate::exchange::market_adapter::{
    BookTickerCallback, CallbackId, ExchangeResult, KlineCallback, MarketAdapter,
    OrderUpdateCallback, TradeCallback,
};

pub struct TestExchangeConfig {
    pub market_type: MarketType,
    pub fees: MarketFees,
    pub leverage: f64,
    pub initial_balance: f64,
}

impl Default for TestExchangeConfig {
    fn default() -> Self {
        Self {
            market_type: MarketType::Linear,
            fees: MarketFees {
                maker_rate: 0.0002,
                taker_rate: 0.0004,
            },
            leverage: 1.0,
            initial_balance: 100_000.0,
        }
    }
}

/// In-memory exchange for testing. Simulates order matching, market data
/// feeds, and balance tracking without network calls.
///
/// All mutable state uses interior mutability so the adapter can be
/// shared via `Arc<dyn MarketAdapter>` without an outer Mutex.
pub struct TestExchange {
    market_type_val: MarketType,
    fees: MarketFees,
    leverage_map: RwLock<HashMap<String, f64>>,
    default_leverage: f64,
    balance: RwLock<f64>,
    pairs: RwLock<HashMap<String, PairInfo>>,
    book_tickers: RwLock<HashMap<String, BookTicker>>,
    open_orders: RwLock<HashMap<String, Order>>,
    next_order_id: AtomicU64,
    next_callback_id: AtomicU64,
    book_ticker_cbs: RwLock<HashMap<String, Vec<(CallbackId, BookTickerCallback)>>>,
    kline_cbs: RwLock<HashMap<String, Vec<(CallbackId, KlineCallback)>>>,
    trade_cbs: RwLock<HashMap<String, Vec<(CallbackId, TradeCallback)>>>,
    order_update_cbs: RwLock<Vec<(CallbackId, OrderUpdateCallback)>>,
}

impl TestExchange {
    pub fn new(config: TestExchangeConfig) -> Self {
        Self {
            market_type_val: config.market_type,
            fees: config.fees,
            leverage_map: RwLock::new(HashMap::new()),
            default_leverage: config.leverage,
            balance: RwLock::new(config.initial_balance),
            pairs: RwLock::new(HashMap::new()),
            book_tickers: RwLock::new(HashMap::new()),
            open_orders: RwLock::new(HashMap::new()),
            next_order_id: AtomicU64::new(1),
            next_callback_id: AtomicU64::new(1),
            book_ticker_cbs: RwLock::new(HashMap::new()),
            kline_cbs: RwLock::new(HashMap::new()),
            trade_cbs: RwLock::new(HashMap::new()),
            order_update_cbs: RwLock::new(Vec::new()),
        }
    }

    fn next_cb_id(&self) -> CallbackId {
        self.next_callback_id.fetch_add(1, Ordering::Relaxed)
    }

    fn notify_order_update(&self, order: &Order) {
        let cbs = self.order_update_cbs.read().unwrap();
        for (_, cb) in cbs.iter() {
            cb(order);
        }
    }

    fn create_default_pair(&self, symbol: &str) -> PairInfo {
        /// Strip trailing USDT/USD/PERP (case-insensitive) to get base symbol.
        fn strip_quote_suffix(s: &str) -> &str {
            let upper = s.to_ascii_uppercase();
            for suffix in &["USDT", "USD", "PERP"] {
                if upper.ends_with(suffix) {
                    return &s[..s.len() - suffix.len()];
                }
            }
            s
        }
        PairInfo {
            symbol: symbol.to_string(),
            display_name: strip_quote_suffix(symbol).to_string(),
            market_type: self.market_type_val,
            price_step: 0.01,
            quantity_step: 0.001,
            price_precision: 2,
            quantity_precision: 3,
            min_quantity: 0.001,
            max_quantity: 1_000_000.0,
            min_notional: 5.0,
            contract_size: if self.market_type_val == MarketType::Inverse {
                100.0
            } else {
                1.0
            },
        }
    }

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    // ── Test Helpers ─────────────────────────────────────────────

    /// Set the current book ticker for a symbol. Notifies subscribers.
    pub fn set_book_ticker(&self, symbol: &str, bid_price: f64, ask_price: f64) {
        let ticker = BookTicker {
            symbol: symbol.to_string(),
            bid_price,
            bid_quantity: 1.0,
            ask_price,
            ask_quantity: 1.0,
            timestamp: Self::now_ms(),
        };
        self.book_tickers.write().unwrap().insert(symbol.to_string(), ticker.clone());
        let cbs = self.book_ticker_cbs.read().unwrap();
        if let Some(cbs) = cbs.get(symbol) {
            for (_, cb) in cbs {
                cb(&ticker);
            }
        }
    }

    /// Emit a kline event for subscribers.
    pub fn emit_kline(&self, kline: &KlineData) {
        let key = format!("{}:{}", kline.symbol, kline.interval);
        let cbs = self.kline_cbs.read().unwrap();
        if let Some(cbs) = cbs.get(&key) {
            for (_, cb) in cbs {
                cb(kline);
            }
        }
    }

    /// Emit a trade event for subscribers.
    pub fn emit_trade(&self, trade: &TradeData) {
        let cbs = self.trade_cbs.read().unwrap();
        if let Some(cbs) = cbs.get(&trade.symbol) {
            for (_, cb) in cbs {
                cb(trade);
            }
        }
    }

    /// Manually fill a pending order at specified or limit price.
    pub fn fill_order(&self, order_id: &str, fill_price: Option<f64>) -> Option<Order> {
        let mut orders = self.open_orders.write().unwrap();
        let mut order = orders.remove(order_id)?;
        let price = fill_price.unwrap_or(order.price);
        order.status = OrderStatus::Filled;
        order.execution_price = price;
        order.filled_quantity = order.quantity;
        drop(orders);
        self.notify_order_update(&order);
        Some(order)
    }

    /// Set balance directly.
    pub fn set_balance(&self, amount: f64) {
        *self.balance.write().unwrap() = amount;
    }

    /// Add or update a pair.
    pub fn set_pair(&self, pair: PairInfo) {
        self.pairs.write().unwrap().insert(pair.symbol.clone(), pair);
    }
}

#[async_trait]
impl MarketAdapter for TestExchange {
    fn market_type(&self) -> MarketType {
        self.market_type_val
    }

    async fn init(&self) -> ExchangeResult<()> {
        Ok(())
    }

    async fn stop(&self) -> ExchangeResult<()> {
        self.open_orders.write().unwrap().clear();
        self.book_ticker_cbs.write().unwrap().clear();
        self.kline_cbs.write().unwrap().clear();
        self.trade_cbs.write().unwrap().clear();
        self.order_update_cbs.write().unwrap().clear();
        Ok(())
    }

    // ── Pair Management ──────────────────────────────────────────

    fn get_pairs(&self) -> HashMap<String, PairInfo> {
        self.pairs.read().unwrap().clone()
    }

    fn get_pair_info(&self, symbol: &str) -> Option<PairInfo> {
        self.pairs.read().unwrap().get(symbol).cloned()
    }

    async fn load_pair(&self, symbol: &str) -> ExchangeResult<PairInfo> {
        if let Some(pair) = self.pairs.read().unwrap().get(symbol) {
            return Ok(pair.clone());
        }
        let pair = self.create_default_pair(symbol);
        self.pairs.write().unwrap().insert(symbol.to_string(), pair.clone());
        Ok(pair)
    }

    async fn subscribe_pairs(&self, symbols: &[String]) -> ExchangeResult<()> {
        for s in symbols {
            self.load_pair(s).await?;
        }
        Ok(())
    }

    // ── Market Data (Pull) ───────────────────────────────────────

    fn get_book_ticker(&self, symbol: &str) -> Option<BookTicker> {
        self.book_tickers.read().unwrap().get(symbol).cloned()
    }

    async fn fetch_klines(
        &self,
        _symbol: &str,
        _interval: &str,
        _limit: usize,
    ) -> ExchangeResult<Vec<KlineData>> {
        Ok(vec![])
    }

    async fn fetch_24hr_stats(
        &self,
        _symbols: Option<&[String]>,
    ) -> ExchangeResult<Vec<Ticker24hr>> {
        Ok(vec![])
    }

    // ── Market Data (Push) ───────────────────────────────────────

    fn on_book_ticker(&self, symbol: &str, cb: BookTickerCallback) -> CallbackId {
        let id = self.next_cb_id();
        self.book_ticker_cbs
            .write().unwrap()
            .entry(symbol.to_string())
            .or_default()
            .push((id, cb));
        id
    }

    fn off_book_ticker(&self, symbol: &str, id: CallbackId) {
        if let Some(cbs) = self.book_ticker_cbs.write().unwrap().get_mut(symbol) {
            cbs.retain(|(cb_id, _)| *cb_id != id);
        }
    }

    fn on_kline(&self, symbol: &str, interval: &str, cb: KlineCallback) -> CallbackId {
        let id = self.next_cb_id();
        let key = format!("{symbol}:{interval}");
        self.kline_cbs.write().unwrap().entry(key).or_default().push((id, cb));
        id
    }

    fn off_kline(&self, symbol: &str, interval: &str, id: CallbackId) {
        let key = format!("{symbol}:{interval}");
        if let Some(cbs) = self.kline_cbs.write().unwrap().get_mut(&key) {
            cbs.retain(|(cb_id, _)| *cb_id != id);
        }
    }

    fn on_trade(&self, symbol: &str, cb: TradeCallback) -> CallbackId {
        let id = self.next_cb_id();
        self.trade_cbs
            .write().unwrap()
            .entry(symbol.to_string())
            .or_default()
            .push((id, cb));
        id
    }

    fn off_trade(&self, symbol: &str, id: CallbackId) {
        if let Some(cbs) = self.trade_cbs.write().unwrap().get_mut(symbol) {
            cbs.retain(|(cb_id, _)| *cb_id != id);
        }
    }

    // ── Order Operations ─────────────────────────────────────────

    async fn place_order(&self, request: &OrderRequest) -> ExchangeResult<Order> {
        let id_num = self.next_order_id.fetch_add(1, Ordering::Relaxed);
        let order_id = format!("TEST-{}", id_num);
        let client_order_id = request.client_order_id.clone().unwrap_or_else(|| order_id.clone());
        let now = Self::now_ms();

        let is_market = request.order_type == OrderType::Market;

        let mut order = Order {
            symbol: request.symbol.clone(),
            order_id: order_id.clone(),
            client_order_id: Some(client_order_id),
            side: request.side,
            order_type: request.order_type,
            status: if is_market {
                OrderStatus::Filled
            } else {
                OrderStatus::New
            },
            price: request.price.unwrap_or(0.0),
            execution_price: 0.0,
            exit_price: 0.0,
            quantity: request.quantity,
            filled_quantity: 0.0,
            last_filled_quantity: 0.0,
            profit: 0.0,
            profit_usd: 0.0,
            take_profit_price: 0.0,
            stop_loss_price: 0.0,
            opened_at: now,
            closed_at: None,
            commission: 0.0,
            commission_asset: None,
        };

        if is_market {
            let fill_price = self
                .book_tickers
                .read().unwrap()
                .get(&request.symbol)
                .map(|t| {
                    if request.side == OrderSide::Buy {
                        t.ask_price
                    } else {
                        t.bid_price
                    }
                })
                .or(request.price);
            let fill_price = match fill_price {
                Some(p) if p > 0.0 => p,
                _ => {
                    return Err(format!(
                        "no fill price for market order on {} — call set_book_ticker first",
                        request.symbol
                    ).into());
                }
            };
            order.execution_price = fill_price;
            order.filled_quantity = request.quantity;
            self.notify_order_update(&order);
        } else {
            self.open_orders.write().unwrap().insert(order_id, order.clone());
        }

        Ok(order)
    }

    async fn cancel_order(&self, symbol: &str, order_id: &str) -> ExchangeResult<()> {
        let mut orders = self.open_orders.write().unwrap();
        if let Some(mut order) = orders.remove(order_id) {
            if order.symbol == symbol {
                order.status = OrderStatus::Canceled;
                order.closed_at = Some(Self::now_ms());
                drop(orders);
                self.notify_order_update(&order);
            }
        }
        Ok(())
    }

    async fn edit_order(
        &self,
        symbol: &str,
        order_id: &str,
        price: f64,
        quantity: Option<f64>,
    ) -> ExchangeResult<Order> {
        let mut orders = self.open_orders.write().unwrap();
        let order = orders.get_mut(order_id).ok_or_else(|| {
            format!("Order {order_id} not found on {symbol}")
        })?;
        if order.symbol != symbol {
            return Err(format!("Order {order_id} not found on {symbol}").into());
        }
        order.price = price;
        if let Some(qty) = quantity {
            order.quantity = qty;
        }
        Ok(order.clone())
    }

    async fn fetch_order(
        &self,
        _symbol: &str,
        order_id: &str,
    ) -> ExchangeResult<Option<Order>> {
        Ok(self.open_orders.read().unwrap().get(order_id).cloned())
    }

    async fn fetch_open_orders(&self, symbol: &str) -> ExchangeResult<Vec<Order>> {
        Ok(self
            .open_orders
            .read().unwrap()
            .values()
            .filter(|o| o.symbol == symbol)
            .cloned()
            .collect())
    }

    // ── Order Tracking (Push) ────────────────────────────────────

    fn on_order_update(&self, cb: OrderUpdateCallback) -> CallbackId {
        let id = self.next_cb_id();
        self.order_update_cbs.write().unwrap().push((id, cb));
        id
    }

    fn off_order_update(&self, id: CallbackId) {
        self.order_update_cbs.write().unwrap().retain(|(cb_id, _)| *cb_id != id);
    }

    // ── Account ──────────────────────────────────────────────────

    fn get_fees(&self) -> MarketFees {
        self.fees
    }

    fn get_leverage(&self, symbol: &str) -> f64 {
        self.leverage_map
            .read().unwrap()
            .get(symbol)
            .copied()
            .unwrap_or(self.default_leverage)
    }

    async fn set_leverage(&self, symbol: &str, leverage: f64) -> ExchangeResult<()> {
        self.leverage_map.write().unwrap().insert(symbol.to_string(), leverage);
        Ok(())
    }

    async fn get_balance(&self) -> ExchangeResult<f64> {
        Ok(*self.balance.read().unwrap())
    }

    // ── Profit ───────────────────────────────────────────────────

    fn calculate_profit(&self, order: &Order) -> ProfitResult {
        match self.market_type_val {
            MarketType::Linear => calculate_linear_profit(&LinearProfitParams {
                side: order.side,
                entry_price: order.execution_price,
                exit_price: order.exit_price,
                quantity: order.filled_quantity,
                leverage: self.get_leverage(&order.symbol),
                fees: self.fees,
                actual_fees: None,
                exit_is_maker: order.order_type == OrderType::Limit,
            }),
            MarketType::Inverse => {
                let contract_size = self
                    .pairs
                    .read().unwrap()
                    .get(&order.symbol)
                    .map(|p| p.contract_size)
                    .unwrap_or(1.0);
                calculate_inverse_profit(&InverseProfitParams {
                    side: order.side,
                    entry_price: order.execution_price,
                    exit_price: order.exit_price,
                    quantity: order.filled_quantity,
                    leverage: self.get_leverage(&order.symbol),
                    contract_size,
                    fees: self.fees,
                    actual_fees_coin: None,
                    exit_is_maker: order.order_type == OrderType::Limit,
                })
            }
            MarketType::Spot => calculate_spot_profit(&SpotProfitParams {
                side: order.side,
                entry_price: order.execution_price,
                exit_price: order.exit_price,
                quantity: order.filled_quantity,
                fees: self.fees,
                actual_fees: None,
                exit_is_maker: order.order_type == OrderType::Limit,
            }),
        }
    }

    // ── ID Generation ────────────────────────────────────────────

    fn generate_order_id(&self) -> String {
        let id = self.next_order_id.fetch_add(1, Ordering::Relaxed);
        format!("TEST-{}", id)
    }

    fn generate_tp_id(&self, base_order_id: &str) -> String {
        format!("{base_order_id}-TP")
    }

    fn generate_sl_id(&self, base_order_id: &str) -> String {
        format!("{base_order_id}-SL")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn exchange() -> TestExchange {
        TestExchange::new(TestExchangeConfig::default())
    }

    #[tokio::test]
    async fn market_order_fills_immediately() {
        let ex = exchange();
        ex.set_book_ticker("BTCUSDT", 49990.0, 50010.0);

        let order = ex
            .place_order(&OrderRequest {
                symbol: "BTCUSDT".into(),
                side: OrderSide::Buy,
                order_type: OrderType::Market,
                quantity: 1.0,
                price: None,
                stop_price: None,
                time_in_force: None,
                client_order_id: None,
                reduce_only: None,
                position_side: None,
            })
            .await
            .unwrap();

        assert_eq!(order.status, OrderStatus::Filled);
        assert_eq!(order.execution_price, 50010.0);
        assert_eq!(order.filled_quantity, 1.0);
    }

    #[tokio::test]
    async fn limit_order_stays_open() {
        let ex = exchange();

        let order = ex
            .place_order(&OrderRequest {
                symbol: "BTCUSDT".into(),
                side: OrderSide::Buy,
                order_type: OrderType::Limit,
                quantity: 1.0,
                price: Some(49000.0),
                stop_price: None,
                time_in_force: None,
                client_order_id: None,
                reduce_only: None,
                position_side: None,
            })
            .await
            .unwrap();

        assert_eq!(order.status, OrderStatus::New);
        assert_eq!(order.execution_price, 0.0);

        let open = ex.fetch_open_orders("BTCUSDT").await.unwrap();
        assert_eq!(open.len(), 1);
    }

    #[tokio::test]
    async fn fill_pending_order() {
        let ex = exchange();

        let order = ex
            .place_order(&OrderRequest {
                symbol: "BTCUSDT".into(),
                side: OrderSide::Buy,
                order_type: OrderType::Limit,
                quantity: 1.0,
                price: Some(49000.0),
                stop_price: None,
                time_in_force: None,
                client_order_id: None,
                reduce_only: None,
                position_side: None,
            })
            .await
            .unwrap();

        let filled = ex.fill_order(&order.order_id, Some(48500.0)).unwrap();
        assert_eq!(filled.status, OrderStatus::Filled);
        assert_eq!(filled.execution_price, 48500.0);
        assert_eq!(filled.filled_quantity, 1.0);

        let open = ex.fetch_open_orders("BTCUSDT").await.unwrap();
        assert!(open.is_empty());
    }

    #[tokio::test]
    async fn cancel_order_test() {
        let ex = exchange();

        let order = ex
            .place_order(&OrderRequest {
                symbol: "BTCUSDT".into(),
                side: OrderSide::Sell,
                order_type: OrderType::Limit,
                quantity: 0.5,
                price: Some(55000.0),
                stop_price: None,
                time_in_force: None,
                client_order_id: None,
                reduce_only: None,
                position_side: None,
            })
            .await
            .unwrap();

        ex.cancel_order("BTCUSDT", &order.order_id).await.unwrap();
        let open = ex.fetch_open_orders("BTCUSDT").await.unwrap();
        assert!(open.is_empty());
    }

    #[tokio::test]
    async fn book_ticker_callback() {
        let ex = exchange();
        let received = Arc::new(Mutex::new(None::<f64>));
        let received_clone = received.clone();

        ex.on_book_ticker(
            "BTCUSDT",
            Box::new(move |ticker| {
                *received_clone.lock().unwrap() = Some(ticker.bid_price);
            }),
        );

        ex.set_book_ticker("BTCUSDT", 50000.0, 50010.0);
        assert_eq!(*received.lock().unwrap(), Some(50000.0));
    }

    #[tokio::test]
    async fn off_book_ticker_removes_callback() {
        let ex = exchange();
        let count = Arc::new(Mutex::new(0u32));
        let count_clone = count.clone();

        let cb_id = ex.on_book_ticker(
            "BTCUSDT",
            Box::new(move |_| {
                *count_clone.lock().unwrap() += 1;
            }),
        );

        ex.set_book_ticker("BTCUSDT", 50000.0, 50010.0);
        assert_eq!(*count.lock().unwrap(), 1);

        ex.off_book_ticker("BTCUSDT", cb_id);
        ex.set_book_ticker("BTCUSDT", 51000.0, 51010.0);
        assert_eq!(*count.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn order_update_callback() {
        let ex = exchange();
        ex.set_book_ticker("BTCUSDT", 49990.0, 50010.0);

        let updates = Arc::new(Mutex::new(Vec::new()));
        let updates_clone = updates.clone();

        ex.on_order_update(Box::new(move |order| {
            updates_clone
                .lock()
                .unwrap()
                .push(order.order_id.clone());
        }));

        ex.place_order(&OrderRequest {
            symbol: "BTCUSDT".into(),
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity: 1.0,
            price: None,
            stop_price: None,
            time_in_force: None,
            client_order_id: None,
            reduce_only: None,
            position_side: None,
        })
        .await
        .unwrap();

        assert_eq!(updates.lock().unwrap().len(), 1);
    }

    #[test]
    fn generate_ids() {
        let ex = exchange();
        let id1 = ex.generate_order_id();
        let id2 = ex.generate_order_id();
        assert_ne!(id1, id2);
        assert!(id1.starts_with("TEST-"));

        assert_eq!(ex.generate_tp_id("ORD-1"), "ORD-1-TP");
        assert_eq!(ex.generate_sl_id("ORD-1"), "ORD-1-SL");
    }

    #[tokio::test]
    async fn leverage_and_fees() {
        let ex = exchange();
        assert_eq!(ex.get_leverage("BTCUSDT"), 1.0);

        ex.set_leverage("BTCUSDT", 20.0).await.unwrap();
        assert_eq!(ex.get_leverage("BTCUSDT"), 20.0);
        assert_eq!(ex.get_leverage("ETHUSDT"), 1.0);

        let fees = ex.get_fees();
        assert_eq!(fees.maker_rate, 0.0002);
        assert_eq!(fees.taker_rate, 0.0004);
    }

    #[tokio::test]
    async fn balance() {
        let ex = exchange();
        assert_eq!(ex.get_balance().await.unwrap(), 100_000.0);

        ex.set_balance(50_000.0);
        assert_eq!(ex.get_balance().await.unwrap(), 50_000.0);
    }

    #[tokio::test]
    async fn load_pair_creates_default() {
        let ex = exchange();
        let pair = ex.load_pair("BTCUSDT").await.unwrap();
        assert_eq!(pair.symbol, "BTCUSDT");
        assert_eq!(pair.display_name, "BTC");
        assert_eq!(pair.market_type, MarketType::Linear);

        let pair2 = ex.load_pair("BTCUSDT").await.unwrap();
        assert_eq!(pair2.symbol, pair.symbol);
    }

    #[tokio::test]
    async fn market_order_without_book_ticker_errors() {
        let ex = exchange();
        // No set_book_ticker — market order should fail
        let result = ex
            .place_order(&OrderRequest {
                symbol: "ETHUSDT".into(),
                side: OrderSide::Buy,
                order_type: OrderType::Market,
                quantity: 1.0,
                price: None,
                stop_price: None,
                time_in_force: None,
                client_order_id: None,
                reduce_only: None,
                position_side: None,
            })
            .await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("no fill price"), "error: {msg}");
    }
}
