# tradectl-sdk

Core SDK crate (v0.1.8) — types, traits, ABI, exchange abstractions, profit calculation, and monitor broadcasting. Every other Rust crate in the workspace depends on this.

## Crate Structure

| Module | Files | Purpose |
|--------|-------|---------|
| `strategy/` | `mod.rs`, `batch.rs` | Strategy trait, Action enum, FillEvent, StrategyPlugin ABI, BatchStrategy |
| `types/` | `enums.rs`, `types.rs`, `config.rs`, `profit.rs`, `order.rs` | All data types, enums, config structs, profit functions |
| `exchange/` | `market_adapter.rs`, `errors.rs`, `test_exchange.rs`, `callbacks.rs` | MarketAdapter trait, error classification, test exchange |
| `bot_state.rs` | — | Shared state for MCP/AI (RwLock-based) |
| `monitor.rs` | — | WebSocket broadcast server (feature-gated) |
| `run_cli.rs` | — | CLI dispatch helpers (feature-gated) |

## Features

| Feature | Adds | Used By |
|---------|------|---------|
| `monitor` | tokio-tungstenite, futures-util, tokio/net+rt+macros | plugins/live (default) |
| `runner` | clap | CLI dispatch |

Default: none.

## Strategy Trait

```rust
pub trait Strategy: Send {
    fn on_ticker(&mut self, ticker: &TickerEvent, ctx: &StrategyContext) -> Action { Action::Hold }
    fn on_trade(&mut self, trade: &TradeEvent, ctx: &StrategyContext) -> Action { Action::Hold }
    fn on_fill(&mut self, fill: &FillEvent, ctx: &StrategyContext) -> FillResponse {
        FillResponse { actions: vec![], notify: true }
    }
    fn name(&self) -> &str;                                    // required
    fn describe(&self) -> &str { "" }
    fn params_schema(&self) -> Vec<ParamDef> { vec![] }
    fn monitor_snapshot(&self, ctx: &StrategyContext, ticker: &TickerEvent) -> MonitorSnapshot { default }
    fn session_state(&mut self) -> Option<serde_json::Value> { None }
    fn session_reset(&mut self, _symbol: &str) {}
}
```

Only `name()` is required. All others have defaults. Strategy is `Send` but **not** `Sync` — it runs in a single-threaded per-symbol event loop.

## Action Enum

```rust
pub enum Action {
    Hold,
    PlaceEntry { side: Side, price: Option<f64>, size: f64, kind: OrderKind, exits: Vec<ExitOrder>, entry_id: Option<String> },
    EditEntry { order_id: String, price: Option<f64>, size: Option<f64> },
    CancelEntry { order_ids: Vec<String>, entry_ids: Vec<String> },
    SetExits { exits: Vec<ExitOrder> },
    AddExit { exit: ExitOrder },
    UpdateExit { exit: ExitOrder },
    RemoveExit { id: String },
    CloseAll,
}
```

Key semantics:
- **PlaceEntry**: `entry_id: None` = single-entry mode (edit-in-place if pending exists). `Some(id)` = multi-slot (each entry_id tracked independently).
- **SetExits**: Diffs by `ExitOrder.id` — adds missing, updates changed, removes absent.
- **CloseAll**: Market-closes all positions + cancels all pending orders.
- **CancelEntry**: Can cancel by `order_ids` (exchange IDs) or `entry_ids` (strategy-assigned IDs).

## Key Types

### StrategyContext
```rust
pub struct StrategyContext<'a> {
    pub timestamp_ms: u64,
    pub book: Option<&'a TickerEvent>,       // latest book ticker (None before first tick)
    pub positions: &'a [PositionInfo],        // open positions
    pub balance: f64,                         // available balance
    pub unrealized_pnl: f64,
    pub realized_pnl: f64,
    pub trade_count: usize,                   // closed trades count
    pub direction: Side,                      // Long or Short (from config)
    pub max_orders_reached: bool,             // exchange order limit hit
}
```

### FillEvent / FillResponse
```rust
pub struct FillEvent {
    pub order_id: String,
    pub symbol: String,
    pub price: f64,
    pub quantity: f64,
    pub is_entry: bool,          // true = entry fill, false = exit fill
    pub is_partial: bool,        // partial fill (position still open)
    pub exit_id: Option<String>, // which ExitOrder triggered (for exits)
    pub position_closed: bool,   // true when position fully closed
}

pub struct FillResponse {
    pub actions: Vec<Action>,    // actions to execute after fill (e.g., SetExits on entry)
    pub notify: bool,            // send Telegram notification
}
```

### PositionInfo
```rust
pub struct PositionInfo {
    pub side: Side,
    pub avg_entry: f64,          // weighted average entry price
    pub quantity: f64,           // current position size
    pub total_entered: f64,      // cumulative entered quantity
    pub entry_count: usize,      // number of entries
    pub last_entry_price: f64,
}
```

### ExitOrder
```rust
pub struct ExitOrder {
    pub id: String,              // strategy-assigned ID (matched in SetExits diff)
    pub price: f64,
    pub size: f64,
    pub kind: ExitType,          // Limit (TP) or Stop (SL)
    pub delay_ms: u64,           // delay before placing (typically 3000ms for SL)
}
```

### MonitorSnapshot / PriceLine
```rust
pub struct MonitorSnapshot {
    pub price_lines: Vec<PriceLine>,  // chart overlays
    pub state: serde_json::Value,     // arbitrary JSON for strategy state panel
}

pub struct PriceLine {
    pub label: String,
    pub price: f64,
    pub color: String,       // hex or CSS color
    pub style: String,       // "solid" | "dashed" | "dotted"
    pub line_width: u8,
    pub axis_label: bool,
}
```

### Market Data (repr(C))
```rust
pub struct TickerEvent { pub bid_price: f64, pub ask_price: f64, pub bid_qty: f64, pub ask_qty: f64, pub timestamp_ms: u64 }
pub struct TradeEvent { pub price: f64, pub quantity: f64, pub timestamp_ms: u64, pub is_buyer_maker: bool }
pub enum MarketEvent { Ticker(TickerEvent), Trade(TradeEvent) }
```

### Enums
- **Side**: `Long`, `Short` (serde: "LONG"/"SHORT")
- **MarketType**: `Linear`, `Inverse`, `Spot`
- **OrderKind**: `Market`, `Limit`
- **ExitType**: `Limit` (TP), `Stop` (SL)
- **OrderType**: `Market`, `Limit`, `StopMarket`, `StopLimit`, `TrailingStopMarket`, `Liquidation`
- **OrderStatus**: `New`, `Filled`, `PartiallyFilled`, `Canceled`, `Closed`, `Rejected`
- **OrderSide**: `Buy`, `Sell`
- **TimeInForce**: `Gtc`, `Ioc`, `Fok`, `Gtx`

## Plugin ABI

```rust
pub const STRATEGY_ABI_VERSION: u32 = 3;

#[repr(C)]
pub struct StrategyPlugin {
    pub abi_version: u32,
    pub name: *const u8,
    pub name_len: usize,
    pub factory: StrategyFactory,                    // fn(&Params) -> Box<dyn Strategy>
    pub batch_factory: Option<BatchFactory>,          // fn(&[Params], &BatchConfig, usize) -> Box<dyn BatchStrategy>
}
```

**Macros:**
```rust
declare_strategy!("my-strategy", MyStrategy::new);
declare_batch_strategy!("my-strategy", MyStrategy::new, MyStrategyBatch::new);
```

Both generate `#[no_mangle] pub extern "C" fn tradectl_strategy() -> StrategyPlugin`.

## BatchStrategy Trait

Structure-of-Arrays execution for ~1000x throughput in sweep/shadow.

```rust
pub trait BatchStrategy: Send {
    fn exchange(&self) -> &BatchExchange;
    fn exchange_mut(&mut self) -> &mut BatchExchange;
    fn process_ticker(&mut self, ticker: &TickerEvent);      // required — strategy logic
    fn check_trade(&mut self, trade: &TradeEvent) { ... }    // delegates to exchange
    fn force_close_all(&mut self, bid_price: f64) { ... }
    fn results(&self) -> Vec<BatchResult> { ... }
    fn trial_count(&self) -> usize { ... }
    fn reset(&mut self) { ... }
    fn diagnostics(&self) -> BatchDiagnostics { ... }
    fn estimated_ram_bytes(&self) -> usize { ... }
}
```

**BatchConfig**: `initial_balance`, `fees` (taker/maker), `slippage`, `leverage`, `latency_ms`, `jitter_ms`, `sl_delay_ms`, `market_type`, `contract_size`.

**compute_score**: `pnl% * min(trade_count / min_trades, 1.0) / (1 + max_dd%)` — rewards return, penalizes drawdown, gates on minimum trade count.

## MarketAdapter Trait

Unified async interface for all exchanges and simulation. Uses `&self` (interior mutability via Mutex/RwLock internally).

```rust
#[async_trait]
pub trait MarketAdapter: Send + Sync {
    // Lifecycle
    fn market_type(&self) -> MarketType;
    async fn init(&self) -> ExchangeResult<()>;
    async fn stop(&self) -> ExchangeResult<()>;
    async fn ping(&self) -> ExchangeResult<u64>;

    // Pair Management
    fn get_pairs(&self) -> HashMap<String, PairInfo>;
    fn get_pair_info(&self, symbol: &str) -> Option<PairInfo>;
    async fn load_pair(&self, symbol: &str) -> ExchangeResult<PairInfo>;
    async fn subscribe_pairs(&self, symbols: &[String]) -> ExchangeResult<()>;

    // Market Data (Pull)
    fn get_book_ticker(&self, symbol: &str) -> Option<BookTicker>;
    async fn fetch_klines(&self, symbol: &str, interval: &str, limit: usize) -> ExchangeResult<Vec<KlineData>>;
    async fn fetch_24hr_stats(&self, symbols: Option<&[String]>) -> ExchangeResult<Vec<Ticker24hr>>;

    // Market Data (Push — callbacks)
    fn on_book_ticker(&self, symbol: &str, cb: BookTickerCallback) -> CallbackId;
    fn off_book_ticker(&self, symbol: &str, id: CallbackId);
    fn on_kline(&self, symbol: &str, interval: &str, cb: KlineCallback) -> CallbackId;
    fn off_kline(&self, symbol: &str, interval: &str, id: CallbackId);
    fn on_trade(&self, symbol: &str, cb: TradeCallback) -> CallbackId;
    fn off_trade(&self, symbol: &str, id: CallbackId);

    // Orders
    async fn place_order(&self, request: &OrderRequest) -> ExchangeResult<Order>;
    async fn cancel_order(&self, symbol: &str, order_id: &str) -> ExchangeResult<()>;
    async fn edit_order(&self, symbol: &str, order_id: &str, price: f64, quantity: Option<f64>) -> ExchangeResult<Order>;
    async fn fetch_order(&self, symbol: &str, order_id: &str) -> ExchangeResult<Option<Order>>;
    async fn fetch_open_orders(&self, symbol: &str) -> ExchangeResult<Vec<Order>>;

    // Order Tracking
    fn on_order_update(&self, cb: OrderUpdateCallback) -> CallbackId;
    fn off_order_update(&self, id: CallbackId);

    // Account
    fn get_fees(&self) -> MarketFees;
    fn get_leverage(&self, symbol: &str) -> f64;
    async fn set_leverage(&self, symbol: &str, leverage: f64) -> ExchangeResult<()>;
    async fn get_balance(&self) -> ExchangeResult<f64>;

    // Profit & IDs
    fn calculate_profit(&self, order: &Order) -> ProfitResult;
    fn generate_order_id(&self) -> String;
    fn generate_tp_id(&self, base_order_id: &str) -> String;
    fn generate_sl_id(&self, base_order_id: &str) -> String;
    fn set_log_prefix(&self, _prefix: &str) {}
}
```

## Profit Functions

Three standalone pure functions (not on a struct):

| Function | Use Case | Key Params |
|----------|----------|------------|
| `calculate_linear_profit` | USDT-margined futures | entry/exit price, qty, leverage, taker/maker fees |
| `calculate_inverse_profit` | Coin-margined futures | + contract_size |
| `calculate_spot_profit` | Spot trades | entry/exit price, qty, fees (no leverage) |

**ProfitResult**: `{ profit: f64, profit_raw: f64, profit_usd: f64, fees: f64 }` — `profit` is leveraged ROI%, `profit_raw` is unleveraged %, `profit_usd` is absolute P&L, `fees` is total fee cost.

## Error System

```rust
pub enum ApiErrorKind {
    OrderNotFound, SlTriggerPrice, ReduceOnlyRejected, SamePrice,
    InsufficientMargin, Unauthorized, SymbolNotTrading, TooManyOrders,
    QuantityExceeded, MaxPositionExceeded, MinNotional, DuplicateOrderId,
    RateLimited, IpBanned, Network, ParseError, Unknown,
}
```

| Method | Returns true for | Runner behavior |
|--------|-----------------|-----------------|
| `is_fatal()` | Unauthorized, SymbolNotTrading | Stop all trading |
| `is_persistent()` | InsufficientMargin, QuantityExceeded, MinNotional | Stop strategy |
| `is_retryable()` | Network, RateLimited | Retry with backoff |
| `is_silent()` | OrderNotFound, SamePrice, ReduceOnlyRejected, SlTriggerPrice, DuplicateOrderId, TooManyOrders, IpBanned, MaxPositionExceeded | No Telegram alert |
| `is_margin()` | InsufficientMargin | Specific margin handling |

`ExchangeApiError::from_response(status, body, endpoint)` parses exchange-specific JSON error responses.

## Config Types

**BotConfig** (top-level, serde `camelCase`):
```rust
pub struct BotConfig {
    pub telegram: Option<TelegramConfig>,      // bot_token, chat_id, send_interval
    pub api: ApiConfig,                        // provider, key, secret, ws, wallet_address, private_key, passphrase
    pub limits: Option<LimitsConfig>,          // max_loss_limit
    pub db: Option<DbConfig>,                  // path
    pub log: Option<LogConfig>,                // path, mode, level
    pub monitor: Option<MonitorConfig>,        // host (0.0.0.0), port (9100)
    pub paper: Option<PaperSettings>,          // latency_ms, jitter_ms
    pub strats: Vec<StratEntry>,               // name, type, marketType, isEmulator, pairs, params, direction, shadow, promotion, trigger, pairSelector
    pub auto_adjust_leverage: bool,
    pub mcp: Option<McpConfig>,                // enabled, host (127.0.0.1), port (9101)
    pub ai: Option<AiConfig>,                  // provider (anthropic/openai/ollama), model, api_key_env, telegram_agent
    pub strategy_docs: HashMap<String, String>,// loaded from STRATEGY.md files (skip serialization)
}
```

**ApiConfig.provider** values: `Binance`, `Bybit`, `OKX`, `Hyperliquid`, `HTX`, `Gate`, `Bitget`.

## BotState

Shared state for MCP/AI access to live runner data. RwLock-based for concurrent reads.

- **Snapshots**: PositionSnapshot, FillSnapshot, TickerSnapshot, BotMeta, OrdersSnapshot, ShadowSummarySnapshot
- **Trait APIs**: PromotionStoreApi, SessionStoreApi, StrategyControlApi
- **Computed insights**: get_performance_summary, get_risk_assessment, get_symbol_comparison

## Build & Test

```bash
cargo build                           # default (no features)
cargo build --features monitor        # with WebSocket server
cargo build --features runner         # with CLI dispatch
cargo build --features "monitor,runner"
cargo test                            # ~36 tests (profit, errors, types)
```

## Behavioral Edge Cases

### Profit Calculation

| When X happens... | Y happens | Why it matters |
|---|---|---|
| `quantity = 0.0` | Returns `ProfitResult::default()` (all zeros). Linear logs warning, others silent | Zero trades are silently skipped |
| `entry_price <= 0.0` or `exit_price <= 0.0` | Returns zeros. Inverse logs warning | Silent return hides data corruption |
| `leverage = 0.0` | Logs warning, returns zeros. Would otherwise cause `margin = notional / 0 = infinity` | Defensive guard — should never occur in live |
| Fees exceed gross profit | `net_pnl = gross - fees` goes negative. Winning trade becomes loser. No special handling | Critical for micro-edge strategies where fees dominate |
| `actual_fees = Some(0.0)` supplied | Uses exact value, ignores fee rates entirely. `total_fees = 0.0` | Allows exchange-reported fees to override estimates (rebates, VIP tiers) |

### Error Classification

| When X happens... | Y happens | Why it matters |
|---|---|---|
| Exchange returns unknown error code (e.g., -9999) | Falls to message inspection: checks for "duplicate", "insufficient", "margin" keywords. If no match: `ApiErrorKind::Unknown` | Graceful downgrade. Message-based fallback prevents crashes |
| HTTP 5xx without JSON body | `serde_json::from_str()` fails. Creates Unknown error with `code = -(http_status)` | Network/exchange outages don't crash the parser |
| HTTP 418 + message contains "banned" | `ApiErrorKind::IpBanned`. `is_silent()=true` (no Telegram). `is_retryable()=false` | IP bans are persistent. Retrying makes it worse |
| HTTP 429 vs HTTP 418 with same error code | 429 → `RateLimited` (retryable). 418 → `IpBanned` (not retryable). **HTTP status is the differentiator** | Same error code can mean different things depending on HTTP status |
| Message contains "balance" but code is unrecognized | Classified as `InsufficientMargin` via keyword fallback. `is_persistent()=true` → stops strategy | Catches exchange variations that don't use standard codes |

### BotState

| When X happens... | Y happens | Why it matters |
|---|---|---|
| Read state before any updates (e.g., `get_positions()` at startup) | Returns empty collections. All RwLocks initialized to empty HashMap/Vec | No errors — gracefully returns empty |
| Position exists but no ticker received yet | Risk assessment uses `avg_entry` as `current_price` fallback. Distance-to-TP/SL shows nonsense values | Early in session, risk numbers are unreliable |
| `balance = 0.0` during risk calculation | `total_unrealized_pnl_pct` clamped to 0.0 (guards against `pnl / 0.0 = infinity`) | Bankruptcy state doesn't crash calculations |
| More than 200 fills recorded | Ring buffer pops oldest (FIFO). Only last 200 retained (`RECENT_FILLS_CAP`) | Memory bounded. Historical performance is partial |
| Only entry fills, no exits yet | `PerformanceSummary` filters for `position_closed && profit_usd.is_some()`. Returns all zeros | Open positions don't contribute to performance metrics |

### Params

| When X happens... | Y happens | Why it matters |
|---|---|---|
| `.get("missing_key", 5.0)` | Returns default `5.0` silently | No error. Strategy must know its defaults |
| `.require("missing_key")` | **Panics** with "missing required param: missing_key" | `.get()` is silent, `.require()` crashes. Choose carefully |
| Float used as boolean (e.g., `get("flag", 0.0) == 1.0`) | No type coercion. `0.5` is neither true nor false | Direction stored as 0.0/1.0. Ambiguous intermediate values possible |

### TestExchange

| When X happens... | Y happens | Why it matters |
|---|---|---|
| Market order placed before `set_book_ticker()` | Returns Error: "no fill price for market order on SYMBOL — call set_book_ticker first" | Helpful error, but easy to forget in tests |
| Limit order placed, price never reaches it | Status remains `New` forever. **Does NOT auto-fill** — must call `fill_order()` manually | Unlike backtest engine. TestExchange is minimal, not a simulator |
| `fetch_klines()` or `fetch_24hr_stats()` called | Returns `Ok(vec![])` — stubs with no data | TestExchange is for order lifecycle testing only |

## Gotchas

- **ABI version must match** between SDK and strategy. Bump `STRATEGY_ABI_VERSION` on any breaking change to Strategy/Action/FillEvent/StrategyPlugin.
- **StrategyPlugin is `#[repr(C)]`** — never add non-FFI-safe fields.
- **Strategy is `Send` but not `Sync`** — single-threaded per-symbol event loop. MarketAdapter is `Send + Sync` (shared via Arc).
- **`&self` on MarketAdapter** means implementations must use interior mutability (Mutex/RwLock) for mutable state.
- **Profit functions return zeroes** for invalid inputs (leverage <= 0, qty <= 0) — no panics, no errors.
- **TickerEvent/TradeEvent are `#[repr(C)]`** — used in zero-copy mmap binary format. Don't change field layout.
- **ExitOrder.delay_ms** — SL is typically delayed 3000ms after entry fill to avoid immediate trigger on volatile fills.
- **compute_score returns NEG_INFINITY** when trade_count == 0.
