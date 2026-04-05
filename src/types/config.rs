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
    /// MCP server configuration (tools-only, no LLM dependency).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<McpConfig>,
    /// AI / LLM configuration (Telegram agent, on-demand explanations).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai: Option<AiConfig>,
    /// Strategy documentation (loaded from STRATEGY.md by CLI, not user-edited).
    #[serde(skip)]
    pub strategy_docs: HashMap<String, String>,
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

/// MCP server configuration (tools-only, no LLM dependency).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_mcp_host")]
    pub host: String,
    #[serde(default = "default_mcp_port")]
    pub port: u16,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: default_mcp_host(),
            port: default_mcp_port(),
        }
    }
}

fn default_mcp_host() -> String { "127.0.0.1".into() }
fn default_mcp_port() -> u16 { 9101 }

/// AI / LLM configuration (Telegram agent, on-demand explanations).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiConfig {
    /// LLM provider: "anthropic", "openai", "ollama".
    #[serde(default = "default_ai_provider")]
    pub provider: String,
    /// Environment variable name holding the API key (not the key itself).
    #[serde(default)]
    pub api_key_env: String,
    /// Model identifier (e.g. "claude-sonnet-4-20250514", "gpt-4o", "llama3").
    #[serde(default = "default_ai_model")]
    pub model: String,
    /// Max tokens for LLM responses. Default: 200.
    #[serde(default = "default_ai_max_tokens")]
    pub max_tokens: u32,
    /// Base URL override (for Ollama or custom endpoints).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Enable Telegram AI agent (free-text Q&A in chat). Default: false.
    #[serde(default)]
    pub telegram_agent: bool,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            provider: default_ai_provider(),
            api_key_env: String::new(),
            model: default_ai_model(),
            max_tokens: default_ai_max_tokens(),
            base_url: None,
            telegram_agent: false,
        }
    }
}

fn default_ai_provider() -> String { "anthropic".into() }
fn default_ai_model() -> String { "claude-sonnet-4-20250514".into() }
fn default_ai_max_tokens() -> u32 { 200 }

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
    /// Use WebSocket API for order operations (lower latency than REST).
    /// Supported by Binance only. Default: false.
    #[serde(default)]
    pub ws: bool,
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
            ws: false,
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
///
/// Supports two modes of variant specification:
/// - **Explicit variants**: `"variants": [{"name": "v1", "params": {...}}]`
/// - **Range-based**: Inline param ranges generate cartesian product automatically.
///   ```json
///   "shadow": {
///       "enabled": true,
///       "entryDistance": {"min": 0.20, "max": 0.40, "step": 0.02},
///       "takeProfit": {"min": 0.12, "max": 0.25, "step": 0.01}
///   }
///   ```
///   Call [`ShadowConfig::expand_ranges`] to generate variants from ranges.
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
    /// Shadow-only mode: suppress live entries until a promotion is applied.
    /// Shadow variants paper-trade normally; real orders only start after promotion.
    #[serde(default)]
    pub shadow_only: bool,
    /// Promotion configuration for auto/manual/agent param rotation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promotion: Option<PromotionConfig>,
    /// Constraints between range parameters (e.g. `chaseSensitivity < entryDistance`).
    /// Applied during [`expand_ranges`] to prune invalid combinations.
    #[serde(default)]
    pub constraints: Vec<ShadowConstraint>,
    /// Parameter names to collapse when variant groups are idle (zero trades).
    /// For each listed param, groups sharing all other params are collapsed to
    /// the minimum value only. Reduces active variant count at runtime.
    /// Example: `["takeProfit"]` — only test min TP when entry hasn't triggered.
    #[serde(default)]
    pub prune_when_idle: Vec<String>,
    /// Inline parameter ranges (captured via serde flatten).
    /// Keys with `{"min": f64, "max": f64, "step": f64}` values are treated as ranges.
    #[serde(flatten, default)]
    pub ranges: HashMap<String, serde_json::Value>,
    /// Staleness detection for live parameters (opt-in).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staleness: Option<StalenessConfig>,
    /// Edge decay detection and kill switch (opt-in).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_decay: Option<EdgeDecayConfig>,
}

/// A constraint between two range parameters.
///
/// During [`ShadowConfig::expand_ranges`], combinations where the constraint
/// is violated are pruned from the cartesian product.
///
/// ```json
/// { "left": "chaseSensitivity", "op": "<", "right": "entryDistance" }
/// ```
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ShadowConstraint {
    pub left: String,
    /// Comparison operator: `"<"`, `"<="`, `">"`, `">="`.
    pub op: String,
    pub right: String,
}

impl ShadowConfig {
    /// Expand inline parameter ranges into explicit variants via cartesian product.
    ///
    /// Range params are objects with `min`, `max`, `step` keys:
    /// ```json
    /// "entryDistance": {"min": 0.20, "max": 0.40, "step": 0.02}
    /// ```
    ///
    /// Generates all combinations and appends them to `self.variants`.
    /// No-op if no valid ranges are found.
    pub fn expand_ranges(&mut self) {
        let mut range_params: Vec<(String, Vec<f64>)> = Vec::new();

        for (key, val) in &self.ranges {
            if let Some(obj) = val.as_object() {
                if let (Some(min), Some(max), Some(step)) = (
                    obj.get("min").and_then(|v| v.as_f64()),
                    obj.get("max").and_then(|v| v.as_f64()),
                    obj.get("step").and_then(|v| v.as_f64()),
                ) {
                    if step > 0.0 && max >= min {
                        let mut values = Vec::new();
                        let mut v = min;
                        // epsilon tolerance for float rounding at boundary
                        while v <= max + step * 0.01 {
                            values.push((v * 1e8).round() / 1e8);
                            v += step;
                        }
                        range_params.push((key.clone(), values));
                    }
                }
            }
        }

        if range_params.is_empty() {
            return;
        }

        // Sort by param name for deterministic variant ordering
        range_params.sort_by(|a, b| a.0.cmp(&b.0));

        // Cartesian product of all ranges
        let mut combos: Vec<Vec<(&str, f64)>> = vec![vec![]];
        for (param_name, values) in &range_params {
            let mut next = Vec::with_capacity(combos.len() * values.len());
            for combo in &combos {
                for &val in values {
                    let mut c = combo.clone();
                    c.push((param_name.as_str(), val));
                    next.push(c);
                }
            }
            combos = next;
        }

        // Filter combos by constraints (e.g. chaseSensitivity < entryDistance)
        let before = combos.len();
        if !self.constraints.is_empty() {
            combos.retain(|combo| {
                for c in &self.constraints {
                    let lv = combo.iter().find(|(k, _)| *k == c.left.as_str()).map(|(_, v)| *v);
                    let rv = combo.iter().find(|(k, _)| *k == c.right.as_str()).map(|(_, v)| *v);
                    if let (Some(l), Some(r)) = (lv, rv) {
                        let ok = match c.op.as_str() {
                            "<" => l < r,
                            "<=" => l <= r,
                            ">" => l > r,
                            ">=" => l >= r,
                            _ => true,
                        };
                        if !ok { return false; }
                    }
                }
                true
            });
        }
        let after = combos.len();
        if before != after {
            log::info!(
                "[shadow] constraints pruned {} → {} variants (removed {})",
                before, after, before - after,
            );
        }

        // Convert to ShadowVariant and append
        for combo in combos {
            let name = combo.iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect::<Vec<_>>()
                .join("_");
            let params: HashMap<String, serde_json::Value> = combo.into_iter()
                .map(|(k, v)| (k.to_string(), serde_json::Value::from(v)))
                .collect();
            self.variants.push(ShadowVariant { name, params });
        }
    }
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

/// Promotion configuration for shadow parameter rotation.
///
/// Controls when and how a winning shadow variant's params replace the live strategy.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromotionConfig {
    /// `"off"` (default), `"auto"`, `"manual"`, or `"agent"`.
    #[serde(default = "default_promotion_mode")]
    pub mode: String,
    /// Minimum absolute score for a variant to be promotion-eligible.
    #[serde(default)]
    pub min_score: f64,
    /// Candidate score must exceed live baseline by at least this margin.
    #[serde(default = "default_min_margin")]
    pub min_margin: f64,
    /// Reject candidates whose max drawdown exceeds this percentage.
    #[serde(default = "default_max_drawdown")]
    pub max_drawdown_pct: f64,
    /// Only promote when no position is open. Default: true.
    #[serde(default = "default_true")]
    pub require_no_position: bool,
    /// Minimum seconds between promotions per symbol. Default: 3600.
    #[serde(default = "default_cooldown")]
    pub cooldown_secs: u64,
    /// After promotion, add old live params as a shadow variant. Default: true.
    #[serde(default = "default_true")]
    pub track_live_as_variant: bool,
    /// Maximum promotions per evaluation window. Default: 1.
    #[serde(default = "default_max_promotions")]
    pub max_promotions_per_window: usize,
    /// Minimum trades specifically for promotion eligibility.
    /// When set, overrides shadow.minTrades for promotion decisions only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_trades_for_promotion: Option<usize>,
    /// Minimum Sharpe ratio (mean_return / std_dev) for promotion eligibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_sharpe: Option<f64>,
}

fn default_promotion_mode() -> String { "off".to_string() }
fn default_min_margin() -> f64 { 0.3 }
fn default_max_drawdown() -> f64 { 15.0 }
fn default_cooldown() -> u64 { 3600 }
fn default_max_promotions() -> usize { 1 }

impl PromotionConfig {
    pub fn is_enabled(&self) -> bool {
        self.mode != "off"
    }
}

/// A shadow variant that has been identified as a promotion candidate.
#[derive(Debug, Clone)]
pub struct PromotionCandidate {
    pub symbol: String,
    pub strategy_name: String,
    pub variant_name: String,
    pub variant_params: crate::Params,
    pub variant_score: f64,
    pub variant_pnl_pct: f64,
    pub variant_max_dd_pct: f64,
    pub variant_trade_count: usize,
    pub live_score: f64,
    pub margin: f64,
    pub timestamp_ms: u64,
    pub variant_sharpe: f64,
    pub live_params_age_secs: Option<u64>,
    pub live_score_trend: Option<Vec<f64>>,
}

/// Decision outcome for a promotion candidate.
#[derive(Debug, Clone)]
pub enum PromotionDecision {
    Approve,
    Reject { reason: String },
    /// Fall back to manual mode (ask user via Telegram).
    Defer,
}

/// Staleness detection for live parameters.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StalenessConfig {
    /// Number of recent evaluation windows to track for rolling score. Default: 3.
    #[serde(default = "default_staleness_window_count")]
    pub window_count: usize,
    /// Alert when rolling live score drops below this. Default: 0.0.
    #[serde(default)]
    pub score_threshold: f64,
    /// Alert when live score declines by this % from rolling peak. Default: 50.0.
    #[serde(default = "default_decline_pct")]
    pub decline_pct: f64,
    /// Alert when live params have been active longer than this (seconds). 0 = disabled.
    #[serde(default)]
    pub max_age_secs: u64,
}

fn default_staleness_window_count() -> usize { 3 }
fn default_decline_pct() -> f64 { 50.0 }

/// Edge decay detection and kill switch.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeDecayConfig {
    /// Score threshold: both live and best variant must be below this. Default: 0.0.
    #[serde(default)]
    pub score_threshold: f64,
    /// Consecutive evaluation windows below threshold before action. Default: 3.
    #[serde(default = "default_consecutive_windows")]
    pub consecutive_windows: usize,
    /// Action: `"pause"` (suppress entries) or `"notify"` (alert only). Default: `"pause"`.
    #[serde(default = "default_edge_action")]
    pub action: String,
}

fn default_consecutive_windows() -> usize { 3 }
fn default_edge_action() -> String { "pause".to_string() }

/// Recorded promotion event (for history / audit trail).
#[derive(Debug, Clone)]
pub struct PromotionRecord {
    pub timestamp_ms: u64,
    pub symbol: String,
    pub strategy_name: String,
    pub from_variant: String,
    pub to_variant: String,
    pub from_score: f64,
    pub to_score: f64,
    pub mode: String,
    pub approved: bool,
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

    // ── Shadow config tests ────────────────────────────────────────

    #[test]
    fn shadow_parse_explicit_variants() {
        let json = r#"{
            "enabled": true,
            "variants": [
                {"name": "v1", "params": {"entryDistance": 0.3, "takeProfit": 0.2}},
                {"name": "v2", "params": {"entryDistance": 0.4}}
            ],
            "evaluationWindowSecs": 3600,
            "minTrades": 5,
            "reportIntervalSecs": 30
        }"#;
        let sc: ShadowConfig = serde_json::from_str(json).unwrap();
        assert!(sc.enabled);
        assert_eq!(sc.variants.len(), 2);
        assert_eq!(sc.variants[0].name, "v1");
        assert_eq!(sc.evaluation_window_secs, 3600);
        assert_eq!(sc.min_trades, 5);
        assert_eq!(sc.report_interval_secs, 30);
    }

    #[test]
    fn shadow_parse_range_config() {
        let json = r#"{
            "enabled": true,
            "entryDistance": {"min": 0.20, "max": 0.24, "step": 0.02},
            "takeProfit": {"min": 0.10, "max": 0.12, "step": 0.01}
        }"#;
        let sc: ShadowConfig = serde_json::from_str(json).unwrap();
        assert!(sc.enabled);
        assert!(sc.variants.is_empty());
        assert_eq!(sc.ranges.len(), 2);
        assert!(sc.ranges.contains_key("entryDistance"));
        assert!(sc.ranges.contains_key("takeProfit"));
    }

    #[test]
    fn shadow_expand_ranges_cartesian_product() {
        let json = r#"{
            "enabled": true,
            "entryDistance": {"min": 0.20, "max": 0.24, "step": 0.02},
            "takeProfit": {"min": 0.10, "max": 0.12, "step": 0.01}
        }"#;
        let mut sc: ShadowConfig = serde_json::from_str(json).unwrap();
        sc.expand_ranges();

        // entryDistance: 0.20, 0.22, 0.24 = 3 values
        // takeProfit: 0.10, 0.11, 0.12 = 3 values
        // 3 × 3 = 9 variants
        assert_eq!(sc.variants.len(), 9);

        // Sorted by param name, so entryDistance comes first
        assert!(sc.variants[0].name.starts_with("entryDistance="));
        assert!(sc.variants[0].name.contains("takeProfit="));

        // Check first and last variant values
        let first = &sc.variants[0];
        assert_eq!(first.params.get("entryDistance").unwrap().as_f64().unwrap(), 0.2);
        assert_eq!(first.params.get("takeProfit").unwrap().as_f64().unwrap(), 0.1);

        let last = &sc.variants[8];
        assert_eq!(last.params.get("entryDistance").unwrap().as_f64().unwrap(), 0.24);
        assert_eq!(last.params.get("takeProfit").unwrap().as_f64().unwrap(), 0.12);
    }

    #[test]
    fn shadow_expand_ranges_single_param() {
        let json = r#"{
            "enabled": true,
            "takeProfit": {"min": 0.10, "max": 0.13, "step": 0.01}
        }"#;
        let mut sc: ShadowConfig = serde_json::from_str(json).unwrap();
        sc.expand_ranges();

        // 0.10, 0.11, 0.12, 0.13 = 4 variants
        assert_eq!(sc.variants.len(), 4);
        assert_eq!(sc.variants[0].name, "takeProfit=0.1");
        assert_eq!(sc.variants[3].name, "takeProfit=0.13");
    }

    #[test]
    fn shadow_expand_ranges_preserves_explicit_variants() {
        let json = r#"{
            "enabled": true,
            "variants": [{"name": "manual", "params": {"x": 1.0}}],
            "takeProfit": {"min": 0.10, "max": 0.11, "step": 0.01}
        }"#;
        let mut sc: ShadowConfig = serde_json::from_str(json).unwrap();
        sc.expand_ranges();

        // 1 explicit + 2 from range
        assert_eq!(sc.variants.len(), 3);
        assert_eq!(sc.variants[0].name, "manual");
        assert_eq!(sc.variants[1].name, "takeProfit=0.1");
        assert_eq!(sc.variants[2].name, "takeProfit=0.11");
    }

    #[test]
    fn shadow_expand_ranges_no_ranges_is_noop() {
        let json = r#"{
            "enabled": true,
            "variants": [{"name": "v1", "params": {"x": 1.0}}]
        }"#;
        let mut sc: ShadowConfig = serde_json::from_str(json).unwrap();
        sc.expand_ranges();
        assert_eq!(sc.variants.len(), 1);
    }

    #[test]
    fn shadow_expand_ranges_invalid_range_ignored() {
        // step = 0 and max < min should be ignored
        let json = r#"{
            "enabled": true,
            "bad1": {"min": 0.5, "max": 0.3, "step": 0.1},
            "bad2": {"min": 0.1, "max": 0.5, "step": 0.0},
            "bad3": {"min": 0.1, "max": 0.5}
        }"#;
        let mut sc: ShadowConfig = serde_json::from_str(json).unwrap();
        sc.expand_ranges();
        assert_eq!(sc.variants.len(), 0);
    }

    #[test]
    fn shadow_defaults() {
        let json = r#"{"enabled": true}"#;
        let sc: ShadowConfig = serde_json::from_str(json).unwrap();
        assert_eq!(sc.evaluation_window_secs, 86400);
        assert_eq!(sc.min_trades, 10);
        assert_eq!(sc.report_interval_secs, 60);
        assert!(sc.variants.is_empty());
        assert!(sc.ranges.is_empty());
        assert!(sc.promotion.is_none());
        assert!(sc.constraints.is_empty());
        assert!(sc.prune_when_idle.is_empty());
    }

    #[test]
    fn shadow_constraints_prune_invalid_combos() {
        let json = r#"{
            "enabled": true,
            "entryDistance": {"min": 0.10, "max": 0.30, "step": 0.10},
            "chaseSensitivity": {"min": 0.10, "max": 0.30, "step": 0.10},
            "constraints": [{"left": "chaseSensitivity", "op": "<", "right": "entryDistance"}]
        }"#;
        let mut sc: ShadowConfig = serde_json::from_str(json).unwrap();
        sc.expand_ranges();

        // ED: 0.10, 0.20, 0.30 (3 values)
        // CS: 0.10, 0.20, 0.30 (3 values)
        // Without constraints: 3 × 3 = 9
        // With CS < ED:
        //   ED=0.10 → no valid CS (0 combos)
        //   ED=0.20 → CS=0.10 (1 combo)
        //   ED=0.30 → CS=0.10, 0.20 (2 combos)
        // Total = 3 combos
        assert_eq!(sc.variants.len(), 3);

        // Verify the combos
        let combos: Vec<(f64, f64)> = sc.variants.iter().map(|v| {
            let cs = v.params["chaseSensitivity"].as_f64().unwrap();
            let ed = v.params["entryDistance"].as_f64().unwrap();
            (cs, ed)
        }).collect();
        assert!(combos.iter().all(|(cs, ed)| cs < ed));
    }

    #[test]
    fn shadow_constraints_multiple() {
        let json = r#"{
            "enabled": true,
            "entryDistance": {"min": 0.10, "max": 0.30, "step": 0.10},
            "chaseSensitivity": {"min": 0.10, "max": 0.30, "step": 0.10},
            "takeProfit": {"min": 0.10, "max": 0.30, "step": 0.10},
            "constraints": [
                {"left": "chaseSensitivity", "op": "<", "right": "entryDistance"},
                {"left": "takeProfit", "op": "<", "right": "entryDistance"}
            ]
        }"#;
        let mut sc: ShadowConfig = serde_json::from_str(json).unwrap();
        sc.expand_ranges();

        // Every variant must satisfy both CS < ED and TP < ED
        for v in &sc.variants {
            let cs = v.params["chaseSensitivity"].as_f64().unwrap();
            let ed = v.params["entryDistance"].as_f64().unwrap();
            let tp = v.params["takeProfit"].as_f64().unwrap();
            assert!(cs < ed, "cs={} < ed={}", cs, ed);
            assert!(tp < ed, "tp={} < ed={}", tp, ed);
        }

        // ED=0.10 → 0 (no valid CS or TP)
        // ED=0.20 → CS=0.10, TP=0.10 → 1×1 = 1
        // ED=0.30 → CS=0.10,0.20, TP=0.10,0.20 → 2×2 = 4
        // Total = 5
        assert_eq!(sc.variants.len(), 5);
    }

    #[test]
    fn shadow_constraints_no_match_prunes_all() {
        // All combos have TP >= ED, so constraint TP < ED prunes everything
        let json = r#"{
            "enabled": true,
            "entryDistance": {"min": 0.10, "max": 0.10, "step": 0.10},
            "takeProfit": {"min": 0.10, "max": 0.20, "step": 0.10},
            "constraints": [{"left": "takeProfit", "op": "<", "right": "entryDistance"}]
        }"#;
        let mut sc: ShadowConfig = serde_json::from_str(json).unwrap();
        sc.expand_ranges();
        // ED=0.10, TP=0.10 or 0.20 — neither is < 0.10
        assert_eq!(sc.variants.len(), 0);
    }

    // ── Promotion config tests ────────────────────────────────────

    #[test]
    fn promotion_config_defaults() {
        let json = r#"{}"#;
        let pc: PromotionConfig = serde_json::from_str(json).unwrap();
        assert_eq!(pc.mode, "off");
        assert!(!pc.is_enabled());
        assert_eq!(pc.min_score, 0.0);
        assert_eq!(pc.min_margin, 0.3);
        assert_eq!(pc.max_drawdown_pct, 15.0);
        assert!(pc.require_no_position);
        assert_eq!(pc.cooldown_secs, 3600);
        assert!(pc.track_live_as_variant);
        assert_eq!(pc.max_promotions_per_window, 1);
        assert!(pc.min_trades_for_promotion.is_none());
        assert!(pc.min_sharpe.is_none());
    }

    #[test]
    fn promotion_config_auto() {
        let json = r#"{
            "mode": "auto",
            "minScore": 1.5,
            "minMargin": 0.5,
            "maxDrawdownPct": 10.0,
            "cooldownSecs": 7200,
            "maxPromotionsPerWindow": 2
        }"#;
        let pc: PromotionConfig = serde_json::from_str(json).unwrap();
        assert!(pc.is_enabled());
        assert_eq!(pc.mode, "auto");
        assert_eq!(pc.min_score, 1.5);
        assert_eq!(pc.min_margin, 0.5);
        assert_eq!(pc.max_drawdown_pct, 10.0);
        assert_eq!(pc.cooldown_secs, 7200);
        assert_eq!(pc.max_promotions_per_window, 2);
    }

    #[test]
    fn shadow_config_with_promotion() {
        let json = r#"{
            "enabled": true,
            "evaluationWindowSecs": 3600,
            "minTrades": 5,
            "reportIntervalSecs": 30,
            "promotion": {
                "mode": "manual",
                "minScore": 2.0,
                "minMargin": 0.3
            }
        }"#;
        let sc: ShadowConfig = serde_json::from_str(json).unwrap();
        assert!(sc.promotion.is_some());
        let pc = sc.promotion.unwrap();
        assert_eq!(pc.mode, "manual");
        assert_eq!(pc.min_score, 2.0);
    }

    #[test]
    fn promotion_config_statistical_fields() {
        let json = r#"{
            "mode": "auto",
            "minScore": 0.5,
            "minTradesForPromotion": 20,
            "minSharpe": 0.5
        }"#;
        let pc: PromotionConfig = serde_json::from_str(json).unwrap();
        assert_eq!(pc.min_trades_for_promotion, Some(20));
        assert_eq!(pc.min_sharpe, Some(0.5));
    }

    #[test]
    fn shadow_config_without_promotion() {
        let json = r#"{"enabled": true}"#;
        let sc: ShadowConfig = serde_json::from_str(json).unwrap();
        assert!(sc.promotion.is_none());
        assert!(sc.staleness.is_none());
        assert!(sc.edge_decay.is_none());
    }

    #[test]
    fn staleness_config_defaults() {
        let json = r#"{}"#;
        let sc: StalenessConfig = serde_json::from_str(json).unwrap();
        assert_eq!(sc.window_count, 3);
        assert_eq!(sc.score_threshold, 0.0);
        assert_eq!(sc.decline_pct, 50.0);
        assert_eq!(sc.max_age_secs, 0);
    }

    #[test]
    fn edge_decay_config_defaults() {
        let json = r#"{}"#;
        let ec: EdgeDecayConfig = serde_json::from_str(json).unwrap();
        assert_eq!(ec.score_threshold, 0.0);
        assert_eq!(ec.consecutive_windows, 3);
        assert_eq!(ec.action, "pause");
    }

    #[test]
    fn shadow_config_with_staleness_and_edge_decay() {
        let json = r#"{
            "enabled": true,
            "staleness": {
                "windowCount": 5,
                "scoreThreshold": 0.5,
                "declinePct": 30.0,
                "maxAgeSecs": 172800
            },
            "edgeDecay": {
                "scoreThreshold": -1.0,
                "consecutiveWindows": 4,
                "action": "notify"
            }
        }"#;
        let sc: ShadowConfig = serde_json::from_str(json).unwrap();
        let st = sc.staleness.unwrap();
        assert_eq!(st.window_count, 5);
        assert_eq!(st.score_threshold, 0.5);
        assert_eq!(st.decline_pct, 30.0);
        assert_eq!(st.max_age_secs, 172800);
        let ed = sc.edge_decay.unwrap();
        assert_eq!(ed.score_threshold, -1.0);
        assert_eq!(ed.consecutive_windows, 4);
        assert_eq!(ed.action, "notify");
    }
}
