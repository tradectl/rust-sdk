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

    let (level, mode, path, no_timestamp) = match config {
        Some(cfg) => {
            let level = match cfg.level.to_lowercase().as_str() {
                "trace" => LevelFilter::Trace,
                "debug" => LevelFilter::Debug,
                "info" => LevelFilter::Info,
                "warn" => LevelFilter::Warn,
                "error" => LevelFilter::Error,
                _ => LevelFilter::Info,
            };
            (level, cfg.mode.as_str(), Some(&cfg.path), cfg.no_timestamp)
        }
        None => (LevelFilter::Info, "console", None, false),
    };

    let mut builder = ConfigBuilder::new();
    if no_timestamp {
        builder.set_time_level(LevelFilter::Off);
    } else {
        builder.set_time_format_custom(format_description!(
            "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:9]Z"
        ));
    }
    let log_config = builder.build();

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

use std::sync::atomic::{AtomicU64, Ordering};

/// Global data timestamp (ms). Updated by the runner on every event.
/// When `noTimestamp` is set, `simplelog` omits its wall-clock timestamp,
/// and log messages can include this instead for deterministic replay logs.
static DATA_TIMESTAMP_MS: AtomicU64 = AtomicU64::new(0);

/// Set the current data timestamp (called by the runner on every event).
pub fn set_data_timestamp(ms: u64) {
    DATA_TIMESTAMP_MS.store(ms, Ordering::Relaxed);
}

/// Format a millisecond timestamp as ISO 8601 (e.g. `2025-01-12T10:30:45.123Z`).
pub fn format_data_ts() -> String {
    let ms = DATA_TIMESTAMP_MS.load(Ordering::Relaxed);
    if ms == 0 { return String::new(); }
    let secs = (ms / 1000) as i64;
    let nanos = ((ms % 1000) * 1_000_000) as u32;
    chrono::DateTime::from_timestamp(secs, nanos)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(|| format!("{}ms", ms))
}

// ── Price formatting ────────────────────────────────────────────────

/// Truncate (not round) a float to 5 decimal places for display.
/// Trailing zeros stripped but keeps at least 2 decimals:
/// `108880.38000` → `108880.38`, `94151.50000` → `94151.50`.
pub fn trunc5(v: f64) -> String {
    let units = (v.abs() * 100_000.0).trunc() as u64;
    let whole = units / 100_000;
    let frac = units % 100_000;
    let raw = if v < 0.0 && units > 0 {
        format!("-{}.{:05}", whole, frac)
    } else {
        format!("{}.{:05}", whole, frac)
    };
    // Keep at least 2 decimal places (trim only positions 3-5)
    let (head, tail) = raw.split_at(raw.len() - 3);
    format!("{}{}", head, tail.trim_end_matches('0'))
}

// ── Shared log functions ────────────────────────────────────────────
//
// Both the LoggingAdapter and paper runner call these so that the
// format is defined once.

/// Core order log: `[timestamp] [cid][name/symbol] message`.
/// Uses data timestamp when set (replay mode), omits it otherwise.
pub fn log_order(cid: &str, name: &str, symbol: &str, msg: impl std::fmt::Display) {
    let ts = DATA_TIMESTAMP_MS.load(Ordering::Relaxed);
    if ts > 0 {
        let ts_str = format_data_ts();
        log::info!("[{}] [{}][{}/{}] {}", ts_str, cid, name, symbol, msg);
    } else {
        log::info!("[{}][{}/{}] {}", cid, name, symbol, msg);
    }
}

/// `[cid][name/symbol][Xms] placed SIDE TYPE qty …`
pub fn log_placed(
    cid: &str, name: &str, symbol: &str,
    side: &str, order_type: &str,
    qty: impl std::fmt::Display, price_str: &str,
    elapsed_ms: u128,
) {
    log_order(cid, name, symbol, format_args!(
        "[{}ms] placed {} {} {}{}", elapsed_ms, side, order_type, qty, price_str
    ));
}

/// `[cid][name/symbol] filled: SIDE qty @ price`
pub fn log_filled(
    cid: &str, name: &str, symbol: &str,
    side: &str, qty: impl std::fmt::Display, price: f64,
) {
    log_order(cid, name, symbol, format_args!(
        "filled: {} {} @ {}", side, qty, trunc5(price)
    ));
}

/// `[cid][name/symbol][Xms] edited: price -> X, qty Y`
pub fn log_edited(
    cid: &str, name: &str, symbol: &str,
    price: f64, qty_str: &str,
    elapsed_ms: u128,
) {
    log_order(cid, name, symbol, format_args!(
        "[{}ms] edited: price -> {}{}", elapsed_ms, trunc5(price), qty_str
    ));
}

/// `[cid][name/symbol][Xms] canceled`
pub fn log_canceled(cid: &str, name: &str, symbol: &str, elapsed_ms: u128) {
    log_order(cid, name, symbol, format_args!("[{}ms] canceled", elapsed_ms));
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
    log_order(cid, name, symbol, format_args!("SL placed @ {}", trunc5(price)));
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

/// Build a single PositionInfo from accumulated position state.
pub fn build_position_info(
    side: Side,
    avg_entry: f64,
    quantity: f64,
    total_entered: f64,
    entry_count: usize,
    last_entry_price: f64,
) -> PositionInfo {
    PositionInfo {
        side,
        avg_entry,
        quantity,
        total_entered,
        entry_count,
        last_entry_price,
    }
}
