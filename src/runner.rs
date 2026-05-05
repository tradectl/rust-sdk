//! Shared runner utilities — logging, order ID generation, position tracking.

use crate::strategy::PositionInfo;
use crate::types::Side;

// ── Logging setup ───────────────────────────────────────────────────

use once_cell::sync::OnceCell;
use std::path::PathBuf;
use std::sync::Mutex;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::fmt::{self, format::Writer, FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;

/// Custom formatter: `[LEVEL] <message>` — matches the legacy simplelog
/// baseline format. No wall-clock timestamp (bot lines self-describe with
/// their own data-timestamp), no target.
struct BracketedLevel;

impl<S, N> FormatEvent<S, N> for BracketedLevel
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        write!(writer, "[{}] ", event.metadata().level())?;
        ctx.field_format().format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

/// Held for the lifetime of the process so the non-blocking writer's
/// background thread keeps draining log records until shutdown.
static LOG_GUARDS: OnceCell<Mutex<Vec<WorkerGuard>>> = OnceCell::new();
static JANITOR: OnceCell<crate::logging::LogJanitor> = OnceCell::new();
static LOG_INIT: std::sync::Once = std::sync::Once::new();

/// Initialise structured logging with both stderr and file outputs.
///
/// `name` is the per-bot identifier used in the log path:
/// files land at `<base>/<sanitized>/<sanitized>.YYYY-MM-DD.log`.
/// Pass the config-file stem (e.g. `bn-session-config`) so daemon and
/// foreground runs share the same file. The name is sanitized before use
/// as a path component — see [`sanitize_bot_name`].
///
/// This is the default for live and daemon runs. Replay paths that must
/// keep stderr quiet (so the harness can diff log files) should call
/// [`setup_logging_file_only`] instead.
pub fn setup_logging(name: &str, config: &Option<crate::types::config::LogConfig>) {
    LOG_INIT.call_once(|| init_inner(name, config, true));
}

/// Initialise structured logging with file output only — no stderr layer.
///
/// Use this for replay runs where the test harness diffs the rotating log
/// file against a baseline and any incidental stderr output would flood
/// the terminal (e.g. `make replay-check`). Live and daemon runs should
/// keep using [`setup_logging`].
///
/// Shares the same once-init guard as [`setup_logging`], so the first
/// call wins per process.
pub fn setup_logging_file_only(name: &str, config: &Option<crate::types::config::LogConfig>) {
    LOG_INIT.call_once(|| init_inner(name, config, false));
}

fn init_inner(name: &str, config: &Option<crate::types::config::LogConfig>, console: bool) {
    let safe_name = sanitize_bot_name(name);

    let (level, base_path, retention_days) = match config {
        Some(cfg) => (cfg.level.as_str(), cfg.path.clone(), cfg.retention_days),
        None => ("info", None, 30),
    };

    // EnvFilter: RUST_LOG overrides config when set.
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(level));

    // Bot log lines self-describe (`[data-ts][bot/symbol] ...`), so we use
    // a custom event formatter that emits `[LEVEL] <message>` — matches the
    // legacy simplelog baseline and skips tracing's wall-clock + target.
    fn make_layer<W>(writer: W) -> Box<dyn Layer<Registry> + Send + Sync>
    where
        W: for<'a> tracing_subscriber::fmt::MakeWriter<'a> + Send + Sync + 'static,
    {
        fmt::layer()
            .with_ansi(false)
            .with_writer(writer)
            .event_format(BracketedLevel)
            .boxed()
    }

    let mut guards: Vec<WorkerGuard> = Vec::new();
    let mut layers: Vec<Box<dyn Layer<Registry> + Send + Sync>> = Vec::new();

    if console {
        layers.push(make_layer(std::io::stderr));
    }

    if retention_days > 0 {
        let dir = resolve_log_dir(base_path.as_deref(), &safe_name);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("setup_logging: failed to create {}: {e}", dir.display());
        }

        // `<dir>/<safe_name>.YYYY-MM-DD.log` — explicit suffix so the janitor
        // and `tradectl logs` parsers can locate the files unambiguously.
        let appender_result = RollingFileAppender::builder()
            .rotation(Rotation::DAILY)
            .filename_prefix(&safe_name)
            .filename_suffix("log")
            .build(&dir);

        match appender_result {
            Ok(appender) => {
                let (non_blocking, guard) = tracing_appender::non_blocking(appender);
                guards.push(guard);
                layers.push(make_layer(non_blocking));

                // Background gzip + retention sweep.
                let janitor = crate::logging::LogJanitor::spawn(
                    dir,
                    safe_name.clone(),
                    retention_days,
                );
                let _ = JANITOR.set(janitor);
            }
            Err(e) => {
                eprintln!(
                    "setup_logging: failed to build rolling appender in {}: {e}",
                    dir.display()
                );
            }
        }
    }

    tracing_subscriber::registry()
        .with(layers)
        .with(env_filter)
        .init();

    let _ = LOG_GUARDS.set(Mutex::new(guards));

    // Bridge `log` facade calls into tracing so existing log::info!() calls
    // route through the same subscribers.
    let _ = tracing_log::LogTracer::init();

    install_panic_hook();
}

/// Replace any character that could escape the log directory or produce a
/// confusing filename with an underscore. Keeps the result non-empty.
///
/// Path separators (`/`, `\`) and the parent reference (`..`) become `_`,
/// as do control characters and whitespace. An empty or all-rejected
/// input falls back to `"bot"`.
pub fn sanitize_bot_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return "bot".to_string();
    }

    let mut out = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        let ok = !ch.is_control()
            && !ch.is_whitespace()
            && !matches!(ch, '/' | '\\' | ':' | '\0');
        out.push(if ok { ch } else { '_' });
    }

    // Collapse `..` runs that survived as literal dots, then collapse
    // resulting `_` runs to a single `_` so a name like `../evil` ends up
    // `_evil` rather than `__evil`.
    while out.contains("..") {
        out = out.replace("..", "_");
    }
    while out.contains("__") {
        out = out.replace("__", "_");
    }

    if out.is_empty() || out == "_" {
        return "bot".to_string();
    }
    out
}

fn resolve_log_dir(base: Option<&str>, sanitized_name: &str) -> PathBuf {
    let base_path = match base {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => default_log_root(),
    };
    base_path.join(sanitized_name)
}

fn default_log_root() -> PathBuf {
    std::env::var("TRADECTL_HOME").ok().map(PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tradectl")
        .join("logs")
}

/// Route panics through `tracing::error!` so they survive daemon mode
/// (where stdout/stderr are wired to /dev/null) and end up in the rotating
/// log file alongside the rest of the bot output.
fn install_panic_hook() {
    static HOOK_INSTALLED: std::sync::Once = std::sync::Once::new();
    HOOK_INSTALLED.call_once(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let location = info
                .location()
                .map(|l| format!("{}:{}", l.file(), l.line()))
                .unwrap_or_else(|| "<unknown>".to_string());
            let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = info.payload().downcast_ref::<String>() {
                s.clone()
            } else {
                "<non-string panic payload>".to_string()
            };
            tracing::error!(target: "panic", "panic at {location}: {payload}");
            // Still call the default hook so terminal/foreground users see
            // the standard backtrace; in daemon mode it goes to /dev/null.
            default(info);
        }));
    });
}

/// Shorthand: console-only logging at info level under the name "tradectl".
pub fn init_logging() {
    setup_logging("tradectl", &None);
}

#[cfg(test)]
mod logging_tests {
    use super::*;

    #[test]
    fn resolve_log_dir_uses_default_when_path_empty() {
        let p = resolve_log_dir(Some(""), "mybot");
        assert!(p.ends_with("logs/mybot"), "got {}", p.display());
    }

    #[test]
    fn resolve_log_dir_uses_default_when_path_none() {
        let p = resolve_log_dir(None, "mybot");
        assert!(p.ends_with("logs/mybot"), "got {}", p.display());
    }

    #[test]
    fn resolve_log_dir_honours_custom_base() {
        let p = resolve_log_dir(Some("/var/log/tradectl/x"), "mybot");
        assert_eq!(p, std::path::PathBuf::from("/var/log/tradectl/x/mybot"));
    }

    #[test]
    fn default_log_root_prefers_tradectl_home_env() {
        // Save and restore HOME / TRADECTL_HOME to avoid polluting other tests.
        let prev_home = std::env::var("HOME").ok();
        let prev_th = std::env::var("TRADECTL_HOME").ok();

        std::env::set_var("HOME", "/should-be-ignored");
        std::env::set_var("TRADECTL_HOME", "/tmp/tradectl-home-test");

        let root = default_log_root();
        assert_eq!(root, std::path::PathBuf::from("/tmp/tradectl-home-test/.tradectl/logs"));

        // restore
        match prev_home { Some(v) => std::env::set_var("HOME", v), None => std::env::remove_var("HOME") }
        match prev_th { Some(v) => std::env::set_var("TRADECTL_HOME", v), None => std::env::remove_var("TRADECTL_HOME") }
    }

    #[test]
    fn sanitize_passes_normal_names() {
        assert_eq!(sanitize_bot_name("bncm03L"), "bncm03L");
        assert_eq!(sanitize_bot_name("bn-session-config"), "bn-session-config");
        assert_eq!(sanitize_bot_name("bot.with.dots"), "bot.with.dots");
    }

    #[test]
    fn sanitize_rejects_path_separators() {
        assert_eq!(sanitize_bot_name("../evil"), "_evil");
        assert_eq!(sanitize_bot_name("a/b"), "a_b");
        assert_eq!(sanitize_bot_name("a\\b"), "a_b");
        assert_eq!(sanitize_bot_name(".."), "bot");
        assert_eq!(sanitize_bot_name("."), "bot");
    }

    #[test]
    fn sanitize_rejects_whitespace_and_control() {
        assert_eq!(sanitize_bot_name("my bot"), "my_bot");
        assert_eq!(sanitize_bot_name("a\tb"), "a_b");
        assert_eq!(sanitize_bot_name("a\nb"), "a_b");
        assert_eq!(sanitize_bot_name("a\0b"), "a_b");
    }

    #[test]
    fn sanitize_falls_back_to_bot_for_empty() {
        assert_eq!(sanitize_bot_name(""), "bot");
        assert_eq!(sanitize_bot_name("   "), "bot");
    }
}

// ── Order ID ────────────────────────────────────────────────────────

/// Generate order ID matching production format: `p{timestamp_ms}{seq:04}`.
pub fn gen_order_id(timestamp_ms: u64, seq: &mut u64) -> String {
    *seq += 1;
    format!("p{}{:04}", timestamp_ms, *seq)
}

use std::sync::atomic::{AtomicU64, Ordering};

/// Global data timestamp (ms). Updated by the runner on every event.
/// When `noTimestamp` is set, the file layer omits its wall-clock timestamp,
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
