//! Runtime configuration types for bot config files (e.g. `config.json`).
//!
//! Shared across live runner, paper runner, and CLI.

use std::collections::HashMap;

fn default_true() -> bool { true }
use super::enums::Side;

/// Top-level bot configuration.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BotConfig {
    pub telegram: Option<TelegramConfig>,
    pub api: ApiConfig,
    pub limits: Option<LimitsConfig>,
    pub db: Option<DbConfig>,
    pub log: Option<LogConfig>,
    pub monitor: Option<MonitorConfig>,
    pub paper: Option<PaperSettings>,
    pub strats: Vec<StratEntry>,
    /// Automatically reduce leverage to the exchange's per-symbol maximum
    /// during init. Prevents -2027 errors when the exchange lowers a symbol's
    /// max leverage below the account's cached value. Default: true.
    #[serde(default)]
    pub auto_adjust_leverage: bool,
}

/// Paper trading emulation settings.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaperSettings {
    /// Base latency for order operations in milliseconds (default 0).
    #[serde(default)]
    pub latency_ms: u64,
    /// Random jitter range in milliseconds. Actual latency = `latency_ms ± rand(0, jitter_ms)`.
    #[serde(default)]
    pub jitter_ms: u64,
}

/// Monitor WebSocket server settings.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct MonitorConfig {
    #[serde(default = "default_monitor_host")]
    pub host: String,
    #[serde(default = "default_monitor_port")]
    pub port: u16,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            host: default_monitor_host(),
            port: default_monitor_port(),
        }
    }
}

fn default_monitor_host() -> String { "0.0.0.0".into() }
fn default_monitor_port() -> u16 { 9100 }

/// Telegram notification settings.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct TelegramConfig {
    pub bot_token: String,
    pub chat_id: String,
    #[serde(default = "default_send_interval")]
    pub send_interval: u64,
}

fn default_send_interval() -> u64 { 10 }

/// Exchange API credentials.
///
/// Binance/Bybit: `key` + `secret` (standard API key pair).
/// Hyperliquid: `wallet_address` + `private_key` (on-chain wallet auth).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiConfig {
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub secret: String,
    /// Hyperliquid wallet address (0x...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallet_address: Option<String>,
    /// Hyperliquid private key for signing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_key: Option<String>,
    /// OKX / Bitget passphrase (required for these exchanges).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passphrase: Option<String>,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            key: String::new(),
            secret: String::new(),
            wallet_address: None,
            private_key: None,
            passphrase: None,
        }
    }
}

fn default_provider() -> String { "Binance".into() }

/// Global risk limits.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitsConfig {
    pub max_loss_limit: f64,
}

/// Database path.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct DbConfig {
    pub path: String,
}

/// Logging configuration.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct LogConfig {
    pub path: String,
    pub mode: String,
    pub level: String,
    /// Disable timestamps in log output (useful for deterministic replay logs).
    #[serde(default)]
    pub no_timestamp: bool,
}

/// A single strategy entry in the `strats` array.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StratEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub strategy_type: String,
    pub market_type: String,
    /// Trading direction: `LONG` or `SHORT`. Defaults to `LONG`.
    #[serde(default)]
    pub direction: Side,
    /// Paper-trading mode. Defaults to `false`.
    #[serde(default)]
    pub is_emulator: bool,
    /// Maximum number of open positions + pending entries for this strategy.
    /// 0 = unlimited (default).
    #[serde(default)]
    pub max_order_count: usize,
    #[serde(default)]
    pub pairs: Vec<String>,
    /// Send notifications (Telegram, etc.) for this strategy. Defaults to `true`.
    #[serde(default = "default_true")]
    pub notify: bool,
    /// Strategy source: `"marketplace"` or `"local"` (default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Pinned marketplace version. Omitted = latest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Shadow parameter optimization config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<ShadowConfig>,
    /// Strategy-specific parameters (variable per strategy type).
    #[serde(flatten)]
    pub params: HashMap<String, serde_json::Value>,
}

/// Shadow parameter optimization configuration.
///
/// Runs alternative parameter sets on paper alongside the live strategy,
/// tracking metrics and periodically reporting outperformers.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShadowConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub variants: Vec<ShadowVariant>,
    /// Evaluation window in seconds. Metrics reset after this period. Default: 86400 (24h).
    #[serde(default = "default_evaluation_window")]
    pub evaluation_window_secs: u64,
    /// Minimum number of trades before a variant is reported. Default: 10.
    #[serde(default = "default_min_trades")]
    pub min_trades: usize,
    /// How often to log/broadcast shadow results in seconds. Default: 60.
    #[serde(default = "default_report_interval")]
    pub report_interval_secs: u64,
}

fn default_evaluation_window() -> u64 { 86400 }
fn default_min_trades() -> usize { 10 }
fn default_report_interval() -> u64 { 60 }

/// A named parameter variant for shadow optimization.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ShadowVariant {
    pub name: String,
    /// Parameter overrides. Merged on top of the base strategy params.
    pub params: HashMap<String, serde_json::Value>,
}

impl StratEntry {
    /// Get a float param by key.
    pub fn get_f64(&self, key: &str) -> Option<f64> {
        self.params.get(key).and_then(|v| v.as_f64())
    }

    /// Get a float param with a default.
    pub fn get_f64_or(&self, key: &str, default: f64) -> f64 {
        self.get_f64(key).unwrap_or(default)
    }

    /// Get a bool param by key.
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.params.get(key).and_then(|v| v.as_bool())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_strat_entry() {
        let json = r#"{
            "name": "test",
            "type": "Bollinger",
            "marketType": "INVERSE",
            "isEmulator": false,
            "pairs": ["BNBUSD_PERP"],
            "SL": 0.5,
            "stopLoss": -0.5,
            "orderSize": 1.433,
            "enablePriceReducer": true
        }"#;
        let entry: StratEntry = serde_json::from_str(json).unwrap();

        assert_eq!(entry.name, "test");
        assert_eq!(entry.strategy_type, "Bollinger");
        assert!(!entry.is_emulator);
        assert_eq!(entry.direction, Side::Long); // default
        assert_eq!(entry.get_f64("SL"), Some(0.5));
        assert_eq!(entry.get_f64_or("stopLoss", 0.0), -0.5);
        assert_eq!(entry.get_bool("enablePriceReducer"), Some(true));
    }

    #[test]
    fn is_emulator_defaults_to_false() {
        let json = r#"{
            "name": "test",
            "type": "Demo",
            "marketType": "LINEAR",
            "pairs": ["BTCUSDT"]
        }"#;
        let entry: StratEntry = serde_json::from_str(json).unwrap();
        assert!(!entry.is_emulator);
        assert_eq!(entry.direction, Side::Long);
    }

    #[test]
    fn parse_direction_short() {
        let json = r#"{
            "name": "test",
            "type": "Demo",
            "marketType": "LINEAR",
            "direction": "SHORT",
            "pairs": ["BTCUSDT"]
        }"#;
        let entry: StratEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.direction, Side::Short);
    }

    #[test]
    fn parse_direction_case_insensitive() {
        for val in &["LONG", "Long", "long", "SHORT", "Short", "short"] {
            let json = format!(r#"{{
                "name": "test",
                "type": "Demo",
                "marketType": "LINEAR",
                "direction": "{}",
                "pairs": ["BTCUSDT"]
            }}"#, val);
            let entry: StratEntry = serde_json::from_str(&json).unwrap();
            if val.to_uppercase() == "LONG" {
                assert_eq!(entry.direction, Side::Long);
            } else {
                assert_eq!(entry.direction, Side::Short);
            }
        }
    }
}
