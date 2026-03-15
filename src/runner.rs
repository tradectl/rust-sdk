//! Shared runner utilities — logging, order ID generation, position tracking.

use crate::strategy::PositionInfo;
use crate::types::Side;

// ── Logging setup ───────────────────────────────────────────────────

/// Initialize structured logging from config.
///
/// Supports three modes: `"console"` (stderr only), `"file"` (file only),
/// `"console_file"` (both). Pass `None` for console-only at info level.
pub fn setup_logging(config: &Option<crate::types::config::LogConfig>) {
    use simplelog::*;
    use time::macros::format_description;

    let (level, mode, path) = match config {
        Some(cfg) => {
            let level = match cfg.level.to_lowercase().as_str() {
                "trace" => LevelFilter::Trace,
                "debug" => LevelFilter::Debug,
                "info" => LevelFilter::Info,
                "warn" => LevelFilter::Warn,
                "error" => LevelFilter::Error,
                _ => LevelFilter::Info,
            };
            (level, cfg.mode.as_str(), Some(&cfg.path))
        }
        None => (LevelFilter::Info, "console", None),
    };

    let log_config = ConfigBuilder::new()
        .set_time_format_custom(format_description!(
            "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:9]Z"
        ))
        .build();

    let mut loggers: Vec<Box<dyn SharedLogger>> = Vec::new();

    if mode != "file" {
        loggers.push(TermLogger::new(
            level, log_config.clone(), TerminalMode::Stderr, ColorChoice::Auto,
        ));
    }

    if mode != "console" {
        if let Some(dir) = path {
            let _ = std::fs::create_dir_all(dir);
            let filename = format!("{}/bot-{}.log",
                dir, chrono::Utc::now().format("%Y%m%d-%H%M%S"));
            if let Ok(file) = std::fs::File::create(&filename) {
                loggers.push(WriteLogger::new(level, log_config, file));
                log::info!("logging to {}", filename);
            }
        }
    }

    if !loggers.is_empty() {
        let _ = CombinedLogger::init(loggers);
    }
}

/// Shorthand: console-only logging at info level.
pub fn init_logging() {
    setup_logging(&None);
}

// ── Order ID ────────────────────────────────────────────────────────

/// Generate order ID matching production format: `p{timestamp_ms}{seq:04}`.
pub fn gen_order_id(timestamp_ms: u64, seq: &mut u64) -> String {
    *seq += 1;
    format!("p{}{:04}", timestamp_ms, *seq)
}

// ── Shared log functions ────────────────────────────────────────────
//
// Both the LoggingAdapter and paper runner call these so that the
// format is defined once.

/// Core order log: `[cid][name/symbol] message`.
pub fn log_order(cid: &str, name: &str, symbol: &str, msg: impl std::fmt::Display) {
    log::info!("[{}][{}/{}] {}", cid, name, symbol, msg);
}

/// `[cid][name/symbol] placing SIDE TYPE qty …`
pub fn log_placing(
    cid: &str, name: &str, symbol: &str,
    side: &str, order_type: &str,
    qty: impl std::fmt::Display, price_str: &str,
) {
    log_order(cid, name, symbol, format_args!(
        "placing {} {} {}{}", side, order_type, qty, price_str
    ));
}

/// `[cid][name/symbol] filled: SIDE qty @ price`
pub fn log_filled(
    cid: &str, name: &str, symbol: &str,
    side: &str, qty: impl std::fmt::Display, price: f64,
) {
    log_order(cid, name, symbol, format_args!(
        "filled: {} {} @ {:.2}", side, qty, price
    ));
}

/// `[cid][name/symbol] editing: price -> X, qty Y`
pub fn log_editing(
    cid: &str, name: &str, symbol: &str,
    price: f64, qty_str: &str,
) {
    log_order(cid, name, symbol, format_args!(
        "editing: price -> {:.2}{}", price, qty_str
    ));
}

/// `[cid][name/symbol] canceling`
pub fn log_canceling(cid: &str, name: &str, symbol: &str) {
    log_order(cid, name, symbol, "canceling");
}

/// `[cid][name/symbol] processing KIND order update: status=STATUS`
pub fn log_processing(
    cid: &str, name: &str, symbol: &str,
    kind: &str, status: &str,
) {
    log_order(cid, name, symbol, format_args!(
        "processing {} order update: status={}", kind, status
    ));
}

/// `[cid][name/symbol] SL scheduled in Xs`
pub fn log_sl_scheduled(cid: &str, name: &str, symbol: &str, delay_secs: u64) {
    log_order(cid, name, symbol, format_args!("SL scheduled in {}s", delay_secs));
}

/// `[cid][name/symbol] SL placed @ price`
pub fn log_sl_placed(cid: &str, name: &str, symbol: &str, price: f64) {
    log_order(cid, name, symbol, format_args!("SL placed @ {:.2}", price));
}

/// `[name] starting (mode, pairs: ...)`
pub fn log_startup(name: &str, mode: &str, pairs: &[String]) {
    log::info!("[{}] starting ({}, pairs: {})", name, mode, pairs.join(", "));
}

/// `PROVIDER ws connected, balance: X, pairs: ...`
pub fn log_connected(provider: &str, balance: f64, pairs: &str) {
    log::info!("{} ws connected, balance: {:.2}, pairs: {}", provider, balance, pairs);
}

/// `monitor WS listening on HOST:PORT`
pub fn log_monitor(host: &str, port: u16) {
    log::info!("monitor WS listening on {}:{}", host, port);
}

// ── Position tracking ───────────────────────────────────────────────

/// Build PositionInfo snapshots from positions (unrealized PnL in USD).
pub fn build_position_infos(
    positions: &[(u64, Side, f64, f64, u64)], // (id, side, entry_price, quantity, entry_time)
    mid: f64,
) -> Vec<PositionInfo> {
    positions
        .iter()
        .map(|&(id, side, entry_price, quantity, entry_time)| {
            let upnl = match side {
                Side::Long => (mid - entry_price) * quantity,
                Side::Short => (entry_price - mid) * quantity,
            };
            PositionInfo {
                id,
                side,
                entry_price,
                quantity,
                unrealized_pnl: upnl,
                entry_time,
            }
        })
        .collect()
}
