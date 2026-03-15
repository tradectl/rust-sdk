//! Runtime configuration types for bot config files (e.g. `config.json`).
//!
//! Shared across live runner, paper runner, and CLI.

use std::collections::HashMap;

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
    pub strats: Vec<StratEntry>,
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
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            key: String::new(),
            secret: String::new(),
            wallet_address: None,
            private_key: None,
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
}

/// A single strategy entry in the `strats` array.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StratEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub strategy_type: String,
    pub market_type: String,
    /// Paper-trading mode. Defaults to `false`.
    #[serde(default)]
    pub is_emulator: bool,
    pub pairs: Vec<String>,
    /// Strategy source: `"marketplace"` or `"local"` (default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Pinned marketplace version. Omitted = latest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Strategy-specific parameters (variable per strategy type).
    #[serde(flatten)]
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
    }
}
