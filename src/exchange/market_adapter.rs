use std::collections::HashMap;
use async_trait::async_trait;
use crate::types::{
    BookTicker, KlineData, MarketFees, MarketType, Order, OrderBookDepth, OrderRequest,
    OrderSide, PairInfo, ProfitResult, Ticker24hr, TradeData,
    BracketTier,
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
///
/// # No default method bodies — ever
///
/// **Every method here is required. Do not add a defaulted method.**
///
/// Several types in the live stack are *delegating wrappers* — they hold an
/// inner `MarketAdapter` and must forward every call (`ArcAdapter`,
/// `LoggingAdapter`). A defaulted method turns "this wrapper forgot to
/// forward" from a compile error into a plausible wrong answer at runtime,
/// with no error, no log, and no trace. That has caused four production
/// incidents:
///
/// | Method | Silently fell back to | Outcome |
/// |---|---|---|
/// | `resolved_hedge_mode` | `false` | 2026-07-18 `-4061` position-mode storm |
/// | `get_max_leverage` | `125` | `-2027` clamp compared against a fabricated cap |
/// | `try_auto_adjust_all_leverage` | `Ok(vec![])` | 2026-07-25 subscribe-time sweep dead since it shipped |
/// | `on_depth`/`get_depth` | `0` / `None` | depth subscriptions silently dropped by `LoggingAdapter` |
///
/// The 32 methods that were always required have never caused one. Keeping
/// the trait total is what makes the compiler, rather than reviewer memory,
/// the thing that catches a missing forward.
///
/// An adapter that genuinely lacks a capability still writes the body — an
/// explicit `Ok(())` / `0` / `None` at the impl site is a statement someone
/// can read and challenge in review. Inheriting it invisibly is not.
#[async_trait]
pub trait MarketAdapter: Send + Sync {
    fn market_type(&self) -> MarketType;

    // ── Lifecycle ────────────────────────────────────────────────
    async fn init(&self) -> ExchangeResult<()>;
    async fn stop(&self) -> ExchangeResult<()>;

    /// Ping exchange and return round-trip latency in milliseconds.
    /// Adapters with no network round trip (paper, replay, tests) return `Ok(0)`.
    async fn ping(&self) -> ExchangeResult<u64>;

    /// Re-read the venue's clock now, rather than waiting for the periodic
    /// sync.
    ///
    /// Called when a request comes back `ApiErrorKind::ClockSkew`. That error
    /// fails EVERY signed request, including the ones that only read state,
    /// so waiting out the ordinary sync interval means minutes of a bot that
    /// cannot trade or even see its own orders.
    ///
    /// Adapters with no clock to sync return `Ok(())` — but they must say so
    /// explicitly. A default body here is what lets a delegating wrapper drop
    /// the forward silently, which is how a correctly-configured account spent
    /// 2026-07-18 having every order rejected.
    async fn resync_clock(&self) -> ExchangeResult<()>;

    // ── Pair Management ──────────────────────────────────────────
    fn get_pairs(&self) -> HashMap<String, PairInfo>;
    fn get_pair_info(&self, symbol: &str) -> Option<PairInfo>;
    async fn load_pair(&self, symbol: &str) -> ExchangeResult<PairInfo>;

    /// Re-read every symbol's metadata from the venue, replacing the cache.
    ///
    /// [`load_pair`](Self::load_pair) answers from cache and reaches the venue
    /// only on a MISS, so a venue that changes a live symbol's `tickSize` or
    /// `stepSize` is never observed again for the life of the process. Every
    /// price the runner sends is snapped to the cached `price_step`, so a stale
    /// step puts every order off the venue's grid: Binance answers `-4014`,
    /// which classifies as `InvalidRequest`/`Bug`, and the 3-in-60s breaker
    /// then stops the strategy for what is really a stale cache (2026-08-15,
    /// ONUSDT, two strategies).
    ///
    /// Adapters that hold no cache return `Ok(())` — but they must say so
    /// explicitly, for the reason spelled out on
    /// [`resync_clock`](Self::resync_clock): a default body here is exactly
    /// what lets a delegating wrapper drop the forward without a compile error.
    async fn refresh_pairs(&self) -> ExchangeResult<()>;

    async fn subscribe_pairs(&self, symbols: &[String]) -> ExchangeResult<()>;
    /// Drop the market-data streams for `symbols` — the inverse of
    /// [`subscribe_pairs`](Self::subscribe_pairs).
    ///
    /// Exists because subscriptions otherwise only ever accumulate: the pair
    /// selector's rotation drops leaked every WS stream until restart (217
    /// subscribed vs ~6 traded on prod, 2026-08), and on a venue hosted in
    /// the same cloud region both directions of that dead traffic are billed.
    ///
    /// Venues with no unsubscribe path yet return `Ok(())` explicitly — the
    /// streams keep flowing until reconnect/restart, and the caller must not
    /// treat `Ok` as proof the bytes stopped. Registered `on_*` callbacks are
    /// NOT touched: deregistration stays the `off_*` family's job.
    async fn unsubscribe_pairs(&self, symbols: &[String]) -> ExchangeResult<()>;

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

    // ── L2 Depth (Push) ──────────────────────────────────────────
    /// Subscribe to L2 order book depth updates. `levels` is the desired
    /// depth (adapter picks closest supported: e.g. Binance 5/10/20).
    /// Venues without a depth feed return `0` and never invoke `cb`.
    fn on_depth(&self, symbol: &str, levels: usize, cb: DepthCallback) -> CallbackId;
    fn off_depth(&self, symbol: &str, id: CallbackId);
    /// Get the latest cached depth snapshot. Returns None if not subscribed.
    fn get_depth(&self, symbol: &str) -> Option<OrderBookDepth>;

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
    /// Authoritative current leverage for `symbol`, fetching from the exchange
    /// when the local cache is cold. `get_leverage` is a pure cache read that
    /// returns a `1.0` default for a symbol never seen at init (e.g. one added
    /// later by the pair selector) — which the -2027 auto-reduce path must NOT
    /// mistake for "already at the 1x floor". Adapters that can query the venue
    /// override this to fill the gap and warm the cache; the default returns
    /// the cached read. (2026-07-19 STARUSDT: a pair-selector symbol with no
    /// cache entry read as 1.0, so the reduce path would have stopped instead
    /// of stepping leverage down.) Adapters that cannot query the venue
    /// return `Ok(self.get_leverage(symbol))`.
    async fn current_leverage(&self, symbol: &str) -> ExchangeResult<f64>;
    async fn set_leverage(&self, symbol: &str, leverage: f64) -> ExchangeResult<()>;
    /// Maximum leverage allowed for the given symbol on this exchange/
    /// account — query the venue's per-symbol brackets so the UI's leverage
    /// slider clamps correctly (BTCUSDT might allow 125, an alt might cap
    /// at 20). Where callers must tell "unknown" apart from a real cap, prefer
    /// `Err` over a fabricated ceiling — nothing can distinguish a made-up
    /// `125` from a genuine 125x cap, and the -2027 clamp misfires on exactly
    /// that difference (see `BinanceAdapter`). Adapters that cannot enumerate
    /// brackets currently return the venue ceiling instead; that is a known
    /// wart, not a pattern to copy.
    async fn get_max_leverage(&self, symbol: &str) -> ExchangeResult<u32>;
    /// Force-refresh and return the exchange-max leverage for `symbol`,
    /// bypassing any cache `get_max_leverage` populated. Used on the -2027
    /// error path when the cached value looks like the unknown/125 sentinel
    /// (a fresh listing whose bracket appeared after the subscribe-time
    /// sweep) and we must confirm the real cap before clamping. Adapters
    /// without a separate cache delegate to `get_max_leverage`.
    async fn refresh_max_leverage(&self, symbol: &str) -> ExchangeResult<u32>;
    /// The venue's notional ladder for `symbol`, from the adapter's cache — the
    /// same data `get_max_leverage` reads its first tier from. Empty when the
    /// venue has no such concept, the symbol is unknown, or the ladder has not
    /// been fetched yet; callers treat empty as "no cap known" and never invent
    /// one. Sync and allocation-light: it sits on the entry path.
    fn bracket_ladder(&self, symbol: &str) -> Vec<BracketTier>;
    /// Auto-adjust leverage for newly-subscribed symbols. Re-fetches
    /// bracket caps and lowers any symbol whose current leverage exceeds
    /// the exchange's first-bracket max. Called by the runner right after
    /// `subscribe_pairs` so every traded symbol is checked at the moment
    /// it's added (initial config + dynamic pair-selector adds). Returns
    /// the list of `(symbol, old, new)` triples that were lowered.
    /// Adapters without a leverage concept return `Ok(Vec::new())`.
    async fn try_auto_adjust_all_leverage(
        &self,
        symbols: &[String],
    ) -> ExchangeResult<Vec<(String, f64, u32)>>;
    /// Whether the runner should REACTIVELY reduce a symbol's leverage when
    /// the exchange rejects an order with "max position exceeded at current
    /// leverage" (Binance -2027) instead of halting the strategy. Mirrors the
    /// `api.autoAdjustLeverage` config flag. A -2027 means the intended
    /// position notional overflows the max-notional bracket at the current
    /// leverage; stepping leverage DOWN widens that bracket. Only Binance
    /// returns the configured flag; all other concrete exchanges return
    /// `false` because they don't drive this error path.
    ///
    /// Note `try_auto_adjust_all_leverage` (above) is a *different, weaker*
    /// mechanism: it runs once at subscribe time and only clamps a
    /// stale-too-high leverage down to the exchange's first-bracket ceiling —
    /// it never reduces to fit a notional bracket, so it does not resolve a
    /// live -2027. This flag gates the reactive path that does.
    ///
    /// Delegating wrappers forward `self.inner.auto_adjust_leverage_enabled()`.
    fn auto_adjust_leverage_enabled(&self) -> bool;
    /// Switch between cross and isolated margin for a futures symbol.
    /// Adapters that don't support it — spot, paper, replay, exchanges with
    /// no exposed endpoint — return `Ok(())`, which the manual-trading server
    /// treats as "applied".
    async fn set_margin_mode(&self, symbol: &str, isolated: bool) -> ExchangeResult<()>;
    async fn get_balance(&self) -> ExchangeResult<f64>;

    // ── Profit ───────────────────────────────────────────────────
    fn calculate_profit(&self, order: &Order) -> ProfitResult;

    // ── ID Generation ────────────────────────────────────────────
    fn generate_order_id(&self) -> String;
    fn generate_tp_id(&self, base_order_id: &str) -> String;
    fn generate_sl_id(&self, base_order_id: &str) -> String;

    // ── Logging context ──────────────────────────────────────────
    /// Override the log prefix (e.g. strategy name). Adapters that don't
    /// log return an empty body.
    fn set_log_prefix(&self, prefix: &str);

    // ── Position mode ─────────────────────────────────────────────
    /// The hedge/dual-side-position mode actually in effect after
    /// `init()` — resolved from the exchange when the config left it
    /// unset (`ApiConfig.hedge_mode: None`), or the enforced value when
    /// explicitly configured. Concrete exchanges with a toggleable
    /// account-wide position mode (currently Binance) override this;
    /// others don't use `positionSide` on orders, so the `false` default
    /// is correct for them.
    ///
    /// DELEGATING WRAPPERS (any adapter that wraps an inner `MarketAdapter`
    /// — `ArcAdapter`, `PaperAdapter`, `LoggingAdapter`, …) MUST override
    /// this to forward `self.inner.resolved_hedge_mode()`. If a wrapper
    /// relies on the `false` default, it silently discards the inner
    /// adapter's real value: on a hedge account the runner then omits
    /// `positionSide` and every order is rejected -4061 (2026-07-18 COIN-M
    /// incident — `ArcAdapter` had exactly this gap).
    ///
    /// Intentionally has NO default body: every impl must answer explicitly.
    /// Concrete exchanges without a position mode return `false`; delegating
    /// wrappers forward `self.inner.resolved_hedge_mode()`. A silent `false`
    /// default is what let `ArcAdapter` drop the value — making this required
    /// turns "wrapper forgot to forward it" into a compile error, not a
    /// production `-4061` storm.
    fn resolved_hedge_mode(&self) -> bool;
}

#[cfg(test)]
mod totality {
    /// `MarketAdapter` must have **no default method bodies**.
    ///
    /// The trait is implemented by delegating wrappers (`ArcAdapter`,
    /// `LoggingAdapter`) that have to forward every call. A defaulted method
    /// lets such a wrapper compile while silently answering for the inner
    /// adapter — four production incidents, all invisible until they cost
    /// money (see the trait docs).
    ///
    /// Keeping every method required means the compiler rejects an incomplete
    /// wrapper. This test guards the property itself, so re-adding a default
    /// fails here rather than at 3am on a live account. If you are hitting
    /// this: write the body out at each impl site instead — an explicit
    /// `Ok(())` is reviewable, an inherited one is not.
    #[test]
    fn trait_has_no_default_method_bodies() {
        let src = include_str!("market_adapter.rs");
        let start = src.find("pub trait MarketAdapter").expect("trait present");
        // The trait ends at the first `}` in column 0 after it starts.
        let body = &src[start..];
        let end = body.find("\n}").map(|e| e + 1).unwrap_or(body.len());
        let body = &body[..end];

        let mut defaulted = Vec::new();
        for (idx, _) in body.match_indices("fn ") {
            // Only method signatures at trait-item indentation.
            let line_start = body[..idx].rfind('\n').map_or(0, |p| p + 1);
            let prefix = &body[line_start..idx];
            if !(prefix == "    " || prefix == "    async " || prefix == "    pub ") {
                continue;
            }
            let name: String = body[idx + 3..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            // Walk past the argument list, then see whether the signature is
            // terminated by `;` (required) or `{` (defaulted).
            let Some(open) = body[idx..].find('(') else { continue };
            let mut i = idx + open;
            let mut depth = 0i32;
            for (off, c) in body[i..].char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            i += off + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let terminator = body[i..].chars().find(|c| *c == ';' || *c == '{');
            if terminator == Some('{') {
                defaulted.push(name);
            }
        }

        assert!(
            defaulted.is_empty(),
            "MarketAdapter must have no defaulted methods, found {}: {:?}\n\
             A default lets a delegating wrapper silently skip the forward. \
             Make it required and write the body at each impl site.",
            defaulted.len(),
            defaulted,
        );
    }
}
