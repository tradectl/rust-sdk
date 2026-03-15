//! Paper trading runner — connects to a live exchange WebSocket for real prices,
//! simulates order fills locally, and broadcasts state to the monitor dashboard.
//!
//! No API keys needed. Read-only market data, simulated execution.

use crate::monitor::*;
use crate::runner;
use crate::strategy::*;
use crate::types::*;
use crate::types::config::*;

use futures_util::StreamExt;
use tokio_tungstenite::tungstenite;

struct Position {
    order_id: String,
    id: u64,
    side: Side,
    entry_price: f64,
    quantity: f64,
    entry_time: u64,
}

impl Position {
    fn as_tuple(&self) -> (u64, Side, f64, f64, u64) {
        (self.id, self.side, self.entry_price, self.quantity, self.entry_time)
    }
}

/// Strategy factory: receives strategy config, returns a boxed strategy.
pub type StrategyFactory = fn(&StratEntry) -> Box<dyn Strategy>;

/// Entry point for paper trading. Loads config from CLI arg (or default path),
/// creates the strategy via factory, and runs the paper trading loop.
///
/// ```ignore
/// tradectl_sdk::paper::run("config.json", |strat| {
///     Box::new(MyStrategy::from_config(strat))
/// });
/// ```
pub fn run(default_config: &str, factory: StrategyFactory) {
    runner::init_logging();

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| default_config.to_string());

    let raw = std::fs::read_to_string(&config_path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {}", config_path, e));

    let config: BotConfig = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("Failed to parse {}: {}", config_path, e));

    let strat = config
        .strats
        .first()
        .expect("Config must have at least one strategy in 'strats'");

    let mut strategy = factory(strat);

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
    if let Err(e) = rt.block_on(run_paper(&config, strat, strategy.as_mut())) {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

async fn run_paper(
    config: &BotConfig,
    strat: &StratEntry,
    strategy: &mut dyn Strategy,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let symbol = strat
        .pairs
        .first()
        .expect("Strategy must have at least one pair")
        .to_uppercase();
    let exchange = config.api.provider.to_lowercase();
    let monitor_config = config.monitor.clone().unwrap_or_default();

    let mode = if strat.is_emulator { "paper" } else { "live" };
    let order_size = strat.get_f64_or("orderSize", 0.001);
    let all_pairs: Vec<String> = strat.pairs.iter().map(|p| p.to_uppercase()).collect();

    // Start monitor first so dashboard can connect immediately
    let monitor = MonitorBroadcaster::start(&monitor_config).await?;
    runner::log_monitor(&monitor_config.host, monitor_config.port);
    runner::log_startup(&strat.name, mode, &all_pairs);

    // Connect to exchange WS
    let url = ws_url(&exchange, &symbol);
    log::info!("connecting to {}...", url);
    let (ws, _) = tokio_tungstenite::connect_async(&url).await?;
    runner::log_connected(&config.api.provider, 10_000.0, &all_pairs.join(", "));

    let (_, mut read) = ws.split();

    let mut positions: Vec<Position> = Vec::new();
    let mut next_id: u64 = 1;
    let mut order_seq: u64 = 0;
    let mut trade_count: usize = 0;
    let mut balance = 10_000.0_f64;
    let mut realized_pnl = 0.0_f64;
    let strategy_name = strat.name.clone();

    while let Some(msg) = read.next().await {
        let text = match msg? {
            tungstenite::Message::Text(t) => t,
            _ => continue,
        };

        let (bid, ask, bid_qty, ask_qty, timestamp) =
            match parse_book_ticker(&exchange, &text) {
                Some(v) => v,
                None => continue,
            };

        let ticker = TickerEvent {
            bid_price: bid,
            bid_qty,
            ask_price: ask,
            ask_qty,
            timestamp_ms: timestamp,
        };

        let mid = (bid + ask) * 0.5;

        // Pre-action context for strategy decision
        let tuples: Vec<_> = positions.iter().map(|p| p.as_tuple()).collect();
        let position_infos = runner::build_position_infos(&tuples, mid);
        let unrealized: f64 = position_infos.iter().map(|p| p.unrealized_pnl).sum();
        let ctx = StrategyContext {
            timestamp_ms: timestamp,
            book: Some(&ticker),
            positions: &position_infos,
            balance,
            unrealized_pnl: unrealized,
            realized_pnl,
            trade_count,
        };

        let action = strategy.on_ticker(&ticker, &ctx);
        drop(position_infos);

        match action {
            Action::MarketOpen { side, size } => {
                let quantity = size.unwrap_or(order_size);
                let entry_price = match side {
                    Side::Long => ask,
                    Side::Short => bid,
                };
                let side_str = match side {
                    Side::Long => "BUY",
                    Side::Short => "SELL",
                };
                let order_id = runner::gen_order_id(timestamp, &mut order_seq);

                runner::log_placing(
                    &order_id, &strategy_name, &symbol,
                    side_str, "MARKET", quantity,
                    &format!(" @ {:.2}", entry_price),
                );
                runner::log_filled(
                    &order_id, &strategy_name, &symbol,
                    side_str, quantity, entry_price,
                );
                runner::log_processing(
                    &order_id, &strategy_name, &symbol,
                    "entry", "Filled",
                );

                monitor.broadcast(&MonitorEvent::Fill(MonitorFill {
                    timestamp_ms: timestamp,
                    strategy_name: strategy_name.clone(),
                    symbol: symbol.clone(),
                    side: format!("{:?}", side),
                    price: entry_price,
                    quantity,
                    fill_type: "entry".into(),
                    profit_pct: None,
                    profit_usd: None,
                }));

                positions.push(Position {
                    order_id,
                    id: next_id,
                    side,
                    entry_price,
                    quantity,
                    entry_time: timestamp,
                });
                next_id += 1;
            }

            Action::ClosePosition { position_id, reason } => {
                close_position(
                    &mut positions, position_id, reason, bid, ask, timestamp,
                    &symbol, &strategy_name, &mut balance, &mut realized_pnl,
                    &mut trade_count, &ticker, strategy, &monitor,
                );
            }

            Action::CloseAll => {
                let ids: Vec<u64> = positions.iter().map(|p| p.id).collect();
                for id in ids {
                    close_position(
                        &mut positions, id, CloseReason::ForceClose, bid, ask, timestamp,
                        &symbol, &strategy_name, &mut balance, &mut realized_pnl,
                        &mut trade_count, &ticker, strategy, &monitor,
                    );
                }
            }

            _ => {}
        }

        // Post-action context for monitor snapshot
        let tuples: Vec<_> = positions.iter().map(|p| p.as_tuple()).collect();
        let position_infos = runner::build_position_infos(&tuples, mid);
        let unrealized: f64 = position_infos.iter().map(|p| p.unrealized_pnl).sum();
        let ctx = StrategyContext {
            timestamp_ms: timestamp,
            book: Some(&ticker),
            positions: &position_infos,
            balance,
            unrealized_pnl: unrealized,
            realized_pnl,
            trade_count,
        };

        let snapshot = strategy.monitor_snapshot(&ctx, &ticker);
        monitor.broadcast(&MonitorEvent::Tick(MonitorTick {
            timestamp_ms: timestamp,
            strategy_name: strategy_name.clone(),
            mode: mode.into(),
            symbol: symbol.clone(),
            bid_price: bid,
            ask_price: ask,
            balance,
            trade_count,
            price_lines: snapshot.price_lines,
            strategy_state: snapshot.state,
        }));
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn close_position(
    positions: &mut Vec<Position>,
    position_id: u64,
    reason: CloseReason,
    bid: f64,
    ask: f64,
    timestamp: u64,
    symbol: &str,
    strategy_name: &str,
    balance: &mut f64,
    realized_pnl: &mut f64,
    trade_count: &mut usize,
    ticker: &TickerEvent,
    strategy: &mut dyn Strategy,
    monitor: &MonitorBroadcaster,
) {
    let idx = match positions.iter().position(|p| p.id == position_id) {
        Some(i) => i,
        None => return,
    };
    let pos = positions.remove(idx);
    let close_price = match pos.side {
        Side::Long => bid,
        Side::Short => ask,
    };
    let pnl_pct = match pos.side {
        Side::Long => (close_price - pos.entry_price) / pos.entry_price * 100.0,
        Side::Short => (pos.entry_price - close_price) / pos.entry_price * 100.0,
    };
    let pnl_usd = pnl_pct / 100.0 * pos.quantity * pos.entry_price;
    *balance += pnl_usd;
    *realized_pnl += pnl_usd;
    *trade_count += 1;

    let fill_type = match reason {
        CloseReason::TakeProfit => "tp",
        CloseReason::StopLoss => "sl",
        CloseReason::ForceClose => "close",
    };
    let close_side = match pos.side {
        Side::Long => "SELL",
        Side::Short => "BUY",
    };
    let reason_tag = match reason {
        CloseReason::TakeProfit => "TP",
        CloseReason::StopLoss => "SL",
        CloseReason::ForceClose => "CLOSE",
    };
    let close_order_id = format!("{}-{}", pos.order_id, reason_tag);

    runner::log_filled(
        &close_order_id, strategy_name, symbol,
        close_side, pos.quantity, close_price,
    );
    runner::log_processing(
        &close_order_id, strategy_name, symbol,
        reason_tag, "Filled",
    );

    // Build remaining position infos for ctx_close
    let mid = (bid + ask) * 0.5;
    let tuples: Vec<_> = positions.iter().map(|p| p.as_tuple()).collect();
    let remaining = runner::build_position_infos(&tuples, mid);
    let remaining_upnl: f64 = remaining.iter().map(|p| p.unrealized_pnl).sum();

    let ctx_close = StrategyContext {
        timestamp_ms: timestamp,
        book: Some(ticker),
        positions: &remaining,
        balance: *balance,
        unrealized_pnl: remaining_upnl,
        realized_pnl: *realized_pnl,
        trade_count: *trade_count,
    };

    strategy.on_position_close(
        &CloseInfo {
            symbol: symbol.into(),
            side: pos.side,
            entry_price: pos.entry_price,
            close_price,
            quantity: pos.quantity,
            profit_pct: pnl_pct,
            profit_usd: pnl_usd,
            reason,
        },
        &ctx_close,
    );

    monitor.broadcast(&MonitorEvent::Fill(MonitorFill {
        timestamp_ms: timestamp,
        strategy_name: strategy_name.into(),
        symbol: symbol.into(),
        side: format!("{:?}", pos.side),
        price: close_price,
        quantity: pos.quantity,
        fill_type: fill_type.into(),
        profit_pct: Some(pnl_pct),
        profit_usd: Some(pnl_usd),
    }));
}

// --- Exchange WebSocket helpers ---

fn ws_url(exchange: &str, symbol: &str) -> String {
    let s = symbol.to_lowercase();
    match exchange {
        "binance" => format!("wss://fstream.binance.com/ws/{}@bookTicker", s),
        "bybit" => format!("wss://stream.bybit.com/v5/public/linear/{}", s.to_uppercase()),
        other => panic!("Unsupported exchange for paper trading: {}", other),
    }
}

fn parse_book_ticker(exchange: &str, text: &str) -> Option<(f64, f64, f64, f64, u64)> {
    match exchange {
        "binance" => parse_binance_book_ticker(text),
        _ => None,
    }
}

#[derive(serde::Deserialize)]
struct BinanceBookTicker {
    b: String,
    #[serde(rename = "B")]
    bid_qty: String,
    a: String,
    #[serde(rename = "A")]
    ask_qty: String,
    #[serde(rename = "T")]
    timestamp: u64,
}

fn parse_binance_book_ticker(text: &str) -> Option<(f64, f64, f64, f64, u64)> {
    let bt: BinanceBookTicker = serde_json::from_str(text).ok()?;
    Some((
        bt.b.parse().ok()?,
        bt.a.parse().ok()?,
        bt.bid_qty.parse().ok()?,
        bt.ask_qty.parse().ok()?,
        bt.timestamp,
    ))
}
