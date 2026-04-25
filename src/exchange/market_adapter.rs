use std::collections::HashMap;
use async_trait::async_trait;
use crate::types::{
    BookTicker, KlineData, MarketFees, MarketType, Order, OrderBookDepth, OrderRequest,
    OrderSide, PairInfo, ProfitResult, Ticker24hr, TradeData,
};

pub type CallbackId = u64;
pub type BookTickerCallback = Box<dyn Fn(&BookTicker) + Send + Sync>;
pub type KlineCallback = Box<dyn Fn(&KlineData) + Send + Sync>;
pub type TradeCallback = Box<dyn Fn(&TradeData) + Send + Sync>;
pub type OrderUpdateCallback = Box<dyn Fn(&Order) + Send + Sync>;
pub type DepthCallback = Box<dyn Fn(&OrderBookDepth) + Send + Sync>;

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

    // ── L2 Depth (Push — optional, default no-op) ───────────────
    /// Subscribe to L2 order book depth updates. `levels` is the desired
    /// depth (adapter picks closest supported: e.g. Binance 5/10/20).
    fn on_depth(&self, _symbol: &str, _levels: usize, _cb: DepthCallback) -> CallbackId { 0 }
    fn off_depth(&self, _symbol: &str, _id: CallbackId) {}
    /// Get the latest cached depth snapshot. Returns None if not subscribed.
    fn get_depth(&self, _symbol: &str) -> Option<OrderBookDepth> { None }

    // ── Order Operations ─────────────────────────────────────────
    async fn place_order(&self, request: &OrderRequest) -> ExchangeResult<Order>;
    async fn cancel_order(&self, symbol: &str, order_id: &str) -> ExchangeResult<()>;
    async fn edit_order(
        &self,
        symbol: &str,
        order_id: &str,
        side: OrderSide,
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
    /// Maximum leverage allowed for the given symbol on this exchange/
    /// account. Default: `1` for Spot, `125` for futures — adapters
    /// should override to query the exchange's per-symbol brackets so
    /// the UI's leverage slider clamps correctly (BTCUSDT might allow
    /// 125, an alt might cap at 20). Errors degrade gracefully to the
    /// default at the call site.
    async fn get_max_leverage(&self, _symbol: &str) -> ExchangeResult<u32> {
        Ok(if self.market_type() == MarketType::Spot { 1 } else { 125 })
    }
    /// Switch between cross and isolated margin for a futures symbol.
    /// Default is a no-op (returns Ok) so adapters that don't support it
    /// — spot, paper, replay, exchanges without an exposed endpoint —
    /// don't need a stub. Manual-trading server treats Ok as "applied".
    async fn set_margin_mode(&self, _symbol: &str, _isolated: bool) -> ExchangeResult<()> {
        Ok(())
    }
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
