use std::collections::HashMap;
use async_trait::async_trait;
use crate::types::{
    BookTicker, KlineData, MarketFees, MarketType, Order, OrderRequest,
    PairInfo, ProfitResult, Ticker24hr, TradeData,
};

pub type CallbackId = u64;
pub type BookTickerCallback = Box<dyn Fn(&BookTicker) + Send + Sync>;
pub type KlineCallback = Box<dyn Fn(&KlineData) + Send + Sync>;
pub type TradeCallback = Box<dyn Fn(&TradeData) + Send + Sync>;
pub type OrderUpdateCallback = Box<dyn Fn(&Order) + Send + Sync>;

pub type ExchangeError = Box<dyn std::error::Error + Send + Sync>;
pub type ExchangeResult<T> = Result<T, ExchangeError>;

/// Unified exchange interface. Every exchange (Binance, Bybit, Hyperliquid)
/// and every simulation mode (emulator, backtester) implements this trait.
/// Strategies never touch exchange-specific code.
///
/// All methods take `&self` — implementations use interior mutability
/// (RwLock, AtomicU64, etc.) for mutable state. This allows the adapter
/// to be shared via `Arc<dyn MarketAdapter>` without an outer Mutex,
/// enabling parallel API calls across strategies.
#[async_trait]
pub trait MarketAdapter: Send + Sync {
    fn market_type(&self) -> MarketType;

    // ── Lifecycle ────────────────────────────────────────────────
    async fn init(&self) -> ExchangeResult<()>;
    async fn stop(&self) -> ExchangeResult<()>;

    /// Ping exchange and return round-trip latency in milliseconds.
    /// Default returns 0 (paper/test adapters).
    async fn ping(&self) -> ExchangeResult<u64> { Ok(0) }

    // ── Pair Management ──────────────────────────────────────────
    fn get_pairs(&self) -> HashMap<String, PairInfo>;
    fn get_pair_info(&self, symbol: &str) -> Option<PairInfo>;
    async fn load_pair(&self, symbol: &str) -> ExchangeResult<PairInfo>;
    async fn subscribe_pairs(&self, symbols: &[String]) -> ExchangeResult<()>;

    // ── Market Data (Pull) ───────────────────────────────────────
    fn get_book_ticker(&self, symbol: &str) -> Option<BookTicker>;
    async fn fetch_klines(
        &self,
        symbol: &str,
        interval: &str,
        limit: usize,
    ) -> ExchangeResult<Vec<KlineData>>;
    async fn fetch_24hr_stats(
        &self,
        symbols: Option<&[String]>,
    ) -> ExchangeResult<Vec<Ticker24hr>>;

    // ── Market Data (Push) ───────────────────────────────────────
    fn on_book_ticker(&self, symbol: &str, cb: BookTickerCallback) -> CallbackId;
    fn off_book_ticker(&self, symbol: &str, id: CallbackId);
    fn on_kline(&self, symbol: &str, interval: &str, cb: KlineCallback) -> CallbackId;
    fn off_kline(&self, symbol: &str, interval: &str, id: CallbackId);
    fn on_trade(&self, symbol: &str, cb: TradeCallback) -> CallbackId;
    fn off_trade(&self, symbol: &str, id: CallbackId);

    // ── Order Operations ─────────────────────────────────────────
    async fn place_order(&self, request: &OrderRequest) -> ExchangeResult<Order>;
    async fn cancel_order(&self, symbol: &str, order_id: &str) -> ExchangeResult<()>;
    async fn edit_order(
        &self,
        symbol: &str,
        order_id: &str,
        price: f64,
        quantity: Option<f64>,
    ) -> ExchangeResult<Order>;
    async fn fetch_order(
        &self,
        symbol: &str,
        order_id: &str,
    ) -> ExchangeResult<Option<Order>>;
    async fn fetch_open_orders(&self, symbol: &str) -> ExchangeResult<Vec<Order>>;

    // ── Order Tracking (Push) ────────────────────────────────────
    fn on_order_update(&self, cb: OrderUpdateCallback) -> CallbackId;
    fn off_order_update(&self, id: CallbackId);

    // ── Account ──────────────────────────────────────────────────
    fn get_fees(&self) -> MarketFees;
    fn get_leverage(&self, symbol: &str) -> f64;
    async fn set_leverage(&self, symbol: &str, leverage: f64) -> ExchangeResult<()>;
    async fn get_balance(&self) -> ExchangeResult<f64>;

    // ── Profit ───────────────────────────────────────────────────
    fn calculate_profit(&self, order: &Order) -> ProfitResult;

    // ── ID Generation ────────────────────────────────────────────
    fn generate_order_id(&self) -> String;
    fn generate_tp_id(&self, base_order_id: &str) -> String;
    fn generate_sl_id(&self, base_order_id: &str) -> String;

    // ── Logging context ──────────────────────────────────────────
    /// Override the log prefix (e.g. strategy name). Default no-op.
    fn set_log_prefix(&self, _prefix: &str) {}
}
