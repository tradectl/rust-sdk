//! Logging utilities: daily-rotating log files with gzip + retention.
//!
//! `setup_logging` (in `crate::runner`) wires `tracing-appender` for daily
//! rotation; this module owns the background `LogJanitor` thread that
//! gzips past-day files and prunes by age.

pub mod janitor;

pub use janitor::{LogJanitor, sweep_once};
