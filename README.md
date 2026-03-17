# tradectl-sdk

Rust SDK for the [tradectl](https://tradectl.com) crypto trading platform. Write, backtest, and deploy trading strategies across CEX/DEX.

This crate provides the core types, traits, and exchange abstractions that strategies, backtesting engines, and exchange adapters build on.

## Quick start

```toml
[dependencies]
tradectl-sdk = { git = "https://github.com/tradectl/rust-sdk" }
```

```rust
use tradectl_sdk::*;

struct DipBuyer { bought: bool }

impl Strategy for DipBuyer {
    fn name(&self) -> &str { "dip_buyer" }

    fn on_ticker(&mut self, ticker: &TickerEvent, ctx: &StrategyContext) -> Action {
        if !self.bought && ctx.positions.is_empty() && ticker.ask_price < 95.0 {
            self.bought = true;
            Action::MarketOpen { side: Side::Long, size: None }
        } else {
            Action::Hold
        }
    }
}
```

That's it — the backtest engine and live runner execute your `Action`s, manage TP/SL, and handle exchange communication.

## What's in the crate

### Strategy trait

The core abstraction. Implement `on_ticker` and/or `on_trade`, return an `Action`:

| Action | Description |
|--------|-------------|
| `Hold` | Do nothing |
| `MarketOpen { side, size }` | Open at market (`size: None` = engine default) |
| `LimitOpen { side, price, size }` | Place a limit order |
| `ClosePosition { position_id }` | Close specific position |
| `CloseAll` | Close all positions |
| `CancelPending` | Cancel pending limit order |

The engine provides a `StrategyContext` on every event with positions, balance, PnL, and the latest book snapshot. You also get `on_position_close` callbacks for TP/SL/force-close events.

### Types

- **Market events** — `TickerEvent`, `TradeEvent`, `MarketEvent` (`#[repr(C)]` for zero-copy mmap)
- **Orders** — `Order`, `OrderRequest`, enums (`OrderType`, `OrderSide`, `OrderStatus`, `MarketType`, `TimeInForce`)
- **Params** — key-value `f64` store for strategy configuration, with `ParamDef` schema for UI/sweeps
- **Profit** — pure functions for linear, inverse, and spot PnL calculation
- **Market data** — `BookTicker`, `KlineData`, `TradeData`, `Ticker24hr`, `PairInfo`

### Exchange abstractions

**`MarketAdapter`** — unified async trait that every exchange implements:

```rust
#[async_trait]
pub trait MarketAdapter: Send + Sync {
    async fn init(&mut self) -> ExchangeResult<()>;
    async fn place_order(&mut self, req: &OrderRequest) -> ExchangeResult<Order>;
    async fn cancel_order(&mut self, symbol: &str, order_id: &str) -> ExchangeResult<()>;
    fn on_book_ticker(&mut self, symbol: &str, cb: BookTickerCallback) -> CallbackId;
    // ... lifecycle, market data, account, profit, ID generation
}
```

**`TestExchange`** — full in-memory implementation for unit testing strategies without network calls. Set book tickers, fill orders, check balances.

**`Provider`** — factory for creating exchange-specific adapters across market types (spot, linear, inverse).

**`ExchangeApiError`** — structured error classification (`is_retryable()`, `is_fatal()`, `is_silent()`) for centralized error handling.

**`OrderTracker`** — in-memory order state with fill/complete/partial-fill callbacks.

### Monitor (feature-gated)

```toml
tradectl-sdk = { git = "...", features = ["monitor"] }
```

WebSocket server that broadcasts live strategy state to connected clients. Zero overhead when no clients are connected.

```rust
use tradectl_sdk::monitor::*;

let broadcaster = MonitorBroadcaster::start(&MonitorConfig::default()).await?;
broadcaster.broadcast(&MonitorEvent::Tick(MonitorTick { /* ... */ }));
```

## AI Integration (MCP)

tradectl exposes an [MCP](https://modelcontextprotocol.io) endpoint so AI coding assistants can interact with the platform directly — query strategies, run backtests, check trades, and publish to the marketplace from within a conversation.

```bash
# Connect Claude Code to your tradectl account
claude mcp add --transport http tradectl https://tradectl.com/mcp \
  --header "Authorization: Bearer st_live_YOUR_API_KEY"

# Or for local dev
claude mcp add --transport http tradectl-dev http://localhost:3001/mcp \
  --header "Authorization: Bearer st_live_YOUR_API_KEY"
```

Generate an API key at **tradectl.com → Account → API Keys** or via the CLI:
```bash
tradectl api-key create
```

Once connected, your AI assistant can:

- List strategies and their performance
- Run backtests and check results
- Query trade history with filters
- Publish strategies to the marketplace
- Check deployments and portfolio

See [MCP.md](../MCP.md) for the full tool reference.

## Dependencies

Core (always):
`async-trait`, `chrono`, `serde`, `serde_json`, `tokio` (time + sync), `log`

With `monitor`:
adds `tokio-tungstenite`, `futures-util`, `tokio` (net + rt + macros)

## License

Proprietary. See LICENSE for details.
