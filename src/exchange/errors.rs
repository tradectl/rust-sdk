use std::fmt;

/// Classified exchange API error.
///
/// Parsed from exchange error responses (e.g. Binance `{"code":-2013,"msg":"..."}`).
/// Provides typed classification for centralized error handling — replaces ad-hoc
/// string matching with structured variants.
#[derive(Debug, Clone)]
pub struct ExchangeApiError {
    pub kind: ApiErrorKind,
    pub code: i32,
    pub message: String,
    /// HTTP method + path (e.g. "POST /fapi/v1/order").
    pub endpoint: String,
    pub http_status: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiErrorKind {
    /// -2013/-2011: Order does not exist (already filled/canceled).
    OrderNotFound,
    /// -2021: Conditional order (stop/TP/SL) would trigger immediately.
    TriggerImmediate,
    /// -2022: ReduceOnly order rejected (position already closed).
    ReduceOnlyRejected,
    /// -4197: No need to modify order (same price).
    SamePrice,
    /// -2019, -1000 w/ margin msg: Insufficient margin/balance.
    InsufficientMargin,
    /// -2015: Invalid API key, secret, or IP not whitelisted (FATAL).
    Unauthorized,
    /// -4199: Symbol is not in trading status (FATAL).
    SymbolNotTrading,
    /// -1015: Too many orders.
    TooManyOrders,
    /// -4005: Quantity exceeds max allowed.
    QuantityExceeded,
    /// -2027: Exceeded maximum allowable position at current leverage.
    /// Persistent: stops the strategy after repeated failures to prevent API spam.
    MaxPositionExceeded,
    /// -4164: Order notional below exchange minimum.
    MinNotional,
    /// -4198: Per-order amendment cap reached. The order can never be modified
    /// again — the runner cancels it and places a fresh, amendable order.
    ModifyLimitExceeded,
    /// Duplicate client order ID (order already placed with this ID).
    DuplicateOrderId,
    /// -1003: Too many requests (rate limit hit).
    RateLimited,
    /// -1003 + HTTP 418: IP banned by exchange (FATAL). Retrying makes it worse.
    IpBanned,
    /// Network/transport error (not an API error).
    Network,
    /// Response deserialization error.
    ParseError,
    /// Unknown/unclassified API error code.
    Unknown,
}

impl ExchangeApiError {
    /// Parse an exchange error response body into a typed error.
    ///
    /// Tries to extract `{"code":-XXXX,"msg":"..."}` (Binance format).
    /// Falls back to Unknown if the body is not structured JSON.
    pub fn from_response(http_status: u16, body: &str, endpoint: String) -> Self {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
            if let (Some(code), Some(msg)) = (v["code"].as_i64(), v["msg"].as_str()) {
                let code = code as i32;
                // HTTP 418 + "banned" = IP ban (not just rate limit)
                let kind = if http_status == 418 && msg.to_lowercase().contains("banned") {
                    ApiErrorKind::IpBanned
                } else {
                    classify_code(code, msg)
                };
                return Self { kind, code, message: msg.to_string(), endpoint, http_status };
            }
        }
        Self {
            kind: ApiErrorKind::Unknown,
            code: -(http_status as i32),
            message: body.to_string(),
            endpoint,
            http_status,
        }
    }

    /// Create from a network/transport error.
    pub fn network(err: impl fmt::Display, endpoint: String) -> Self {
        Self {
            kind: ApiErrorKind::Network,
            code: 0,
            message: err.to_string(),
            endpoint,
            http_status: 0,
        }
    }

    /// Create from a deserialization error.
    pub fn parse(err: impl fmt::Display, endpoint: String, http_status: u16) -> Self {
        Self {
            kind: ApiErrorKind::ParseError,
            code: 0,
            message: err.to_string(),
            endpoint,
            http_status,
        }
    }

    /// Fatal errors that should stop trading. Use `is_account_fatal()` /
    /// `is_symbol_fatal()` to decide the *scope* of the stop — the bot vs a
    /// single symbol.
    pub fn is_fatal(&self) -> bool {
        self.is_account_fatal() || self.is_symbol_fatal()
    }

    /// Account-level fatal: the whole account cannot trade, so the entire bot
    /// (every strategy and symbol) must stop. Only invalid credentials / IP
    /// qualify — there is no per-symbol recovery from these.
    pub fn is_account_fatal(&self) -> bool {
        matches!(self.kind, ApiErrorKind::Unauthorized)
    }

    /// Symbol-level fatal: the affected symbol is halted/delisted (`-4199`),
    /// but the account and every sibling symbol are unaffected. The runner
    /// stops only that symbol's task — it must NOT broadcast a global shutdown.
    pub fn is_symbol_fatal(&self) -> bool {
        matches!(self.kind, ApiErrorKind::SymbolNotTrading)
    }

    /// Human-readable reason for fatal errors.
    pub fn fatal_reason(&self) -> Option<&'static str> {
        match self.kind {
            ApiErrorKind::Unauthorized => Some("invalid API key or IP not whitelisted"),
            ApiErrorKind::SymbolNotTrading => Some("symbol not in trading status"),
            _ => None,
        }
    }

    /// IP ban — handled globally by the coordinator's ping handler
    /// (pause for ban duration, then resume). Not fatal because it's temporary.
    pub fn is_ip_banned(&self) -> bool {
        self.kind == ApiErrorKind::IpBanned
    }

    /// Errors indicating insufficient margin/balance.
    pub fn is_margin(&self) -> bool {
        self.kind == ApiErrorKind::InsufficientMargin
    }

    /// Errors that the strategy can plausibly recover from on its own
    /// timescale — the account-level constraint that produced the error
    /// frees up as other positions close or as the operator adjusts
    /// leverage. Today the only kind in this class is `InsufficientMargin`.
    ///
    /// Runner contract: cancel the offending resting order, set a short
    /// per-symbol pause, keep the strategy alive. If a second recoverable
    /// error fires before any successful placement, escalate to a hard
    /// stop (the underlying constraint isn't clearing).
    pub fn is_recoverable(&self) -> bool {
        self.kind == ApiErrorKind::InsufficientMargin
    }

    /// Persistent errors that should stop the strategy (not the bot) after
    /// repeated failures. These signal a permanent mismatch between the
    /// strategy's params and the exchange/account state — retrying with
    /// the same params will keep failing.
    ///
    /// Excludes `InsufficientMargin`, which is recoverable on the wall
    /// clock and routed through `is_recoverable()` instead.
    pub fn is_persistent(&self) -> bool {
        matches!(self.kind, ApiErrorKind::QuantityExceeded | ApiErrorKind::MinNotional | ApiErrorKind::MaxPositionExceeded)
    }

    /// Errors that are expected and should be handled silently (no Telegram alert).
    /// TooManyOrders (-1015) is silent because the ApiLimitTracker handles it
    /// globally — individual per-order warnings would just spam the logs.
    pub fn is_silent(&self) -> bool {
        matches!(
            self.kind,
            ApiErrorKind::OrderNotFound
                | ApiErrorKind::SamePrice
                | ApiErrorKind::ReduceOnlyRejected
                | ApiErrorKind::TriggerImmediate
                | ApiErrorKind::DuplicateOrderId
                | ApiErrorKind::TooManyOrders
                | ApiErrorKind::IpBanned
        )
    }

    /// `-4198`: the per-order amendment cap was hit. Not retryable and not
    /// fatal — the order is permanently un-amendable, so the runner cancels
    /// it and re-places a fresh order (cancel + replace) rather than waiting.
    pub fn is_modify_limit_exceeded(&self) -> bool {
        self.kind == ApiErrorKind::ModifyLimitExceeded
    }

    /// Errors that can be retried after a short delay.
    ///
    /// Note: `TooManyOrders` (-1015) is NOT retryable — it's a per-minute
    /// order rate limit. Retrying after 1s just adds to the overload.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self.kind,
            ApiErrorKind::Network | ApiErrorKind::RateLimited
        )
    }
}

fn classify_code(code: i32, msg: &str) -> ApiErrorKind {
    match code {
        -2013 | -2011 => ApiErrorKind::OrderNotFound,
        -2021 => ApiErrorKind::TriggerImmediate,
        -2022 => ApiErrorKind::ReduceOnlyRejected,
        -4197 => ApiErrorKind::SamePrice,
        -2019 => ApiErrorKind::InsufficientMargin,
        -2015 => ApiErrorKind::Unauthorized,
        -4199 => ApiErrorKind::SymbolNotTrading,
        -1015 => ApiErrorKind::TooManyOrders,
        -4005 => ApiErrorKind::QuantityExceeded,
        -2027 => ApiErrorKind::MaxPositionExceeded,
        -4164 => ApiErrorKind::MinNotional,
        -4198 => ApiErrorKind::ModifyLimitExceeded,
        -1003 => ApiErrorKind::RateLimited,
        -1112 => ApiErrorKind::DuplicateOrderId,
        _ => {
            let lower = msg.to_lowercase();
            if lower.contains("duplicate") {
                ApiErrorKind::DuplicateOrderId
            } else if lower.contains("insufficient")
                || lower.contains("margin")
                || lower.contains("not enough")
                || lower.contains("balance")
                || lower.contains("exceeds")
                || lower.contains("funds")
            {
                ApiErrorKind::InsufficientMargin
            } else {
                ApiErrorKind::Unknown
            }
        }
    }
}

impl fmt::Display for ExchangeApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} → HTTP {}: [{}] {}",
            self.endpoint, self.http_status, self.code, self.message
        )
    }
}

impl std::error::Error for ExchangeApiError {}

/// Downcast an `ExchangeError` to `ExchangeApiError` if possible.
pub fn classify<'a>(err: &'a (dyn std::error::Error + Send + Sync + 'static)) -> Option<&'a ExchangeApiError> {
    err.downcast_ref::<ExchangeApiError>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_binance_error() {
        let body = r#"{"code":-2013,"msg":"Order does not exist."}"#;
        let err = ExchangeApiError::from_response(400, body, "GET /fapi/v1/order".into());
        assert_eq!(err.kind, ApiErrorKind::OrderNotFound);
        assert_eq!(err.code, -2013);
        assert!(!err.is_fatal());
        assert!(err.is_silent());
    }

    #[test]
    fn parse_fatal_error() {
        let body = r#"{"code":-2015,"msg":"Invalid API-key, IP, or permissions for action."}"#;
        let err = ExchangeApiError::from_response(403, body, "POST /fapi/v1/order".into());
        assert_eq!(err.kind, ApiErrorKind::Unauthorized);
        assert!(err.is_fatal());
        assert_eq!(err.fatal_reason(), Some("invalid API key or IP not whitelisted"));
    }

    #[test]
    fn unauthorized_is_account_fatal_not_symbol_fatal() {
        let err = ExchangeApiError::from_response(
            403,
            r#"{"code":-2015,"msg":"Invalid API-key, IP, or permissions for action."}"#,
            "POST /fapi/v1/order".into(),
        );
        assert_eq!(err.kind, ApiErrorKind::Unauthorized);
        assert!(err.is_fatal());
        assert!(err.is_account_fatal(), "bad key/IP must stop the whole bot");
        assert!(!err.is_symbol_fatal());
    }

    #[test]
    fn symbol_not_trading_is_symbol_fatal_not_account_fatal() {
        let err = ExchangeApiError::from_response(
            400,
            r#"{"code":-4199,"msg":"Symbol is not in trading status."}"#,
            "POST /fapi/v1/order".into(),
        );
        assert_eq!(err.kind, ApiErrorKind::SymbolNotTrading);
        assert!(err.is_fatal());
        assert!(err.is_symbol_fatal(), "a halted symbol must stop only that symbol");
        assert!(!err.is_account_fatal(), "a halted symbol must NOT stop the whole bot");
        assert_eq!(err.fatal_reason(), Some("symbol not in trading status"));
    }

    #[test]
    fn parse_margin_error() {
        let body = r#"{"code":-2019,"msg":"Margin is insufficient."}"#;
        let err = ExchangeApiError::from_response(400, body, "POST /fapi/v1/order".into());
        assert_eq!(err.kind, ApiErrorKind::InsufficientMargin);
        assert!(err.is_margin());
        assert!(err.is_recoverable(), "margin is recoverable");
        assert!(!err.is_persistent(), "margin must NOT be persistent");
        assert!(!err.is_fatal());
    }

    #[test]
    fn parse_unknown_code_with_margin_message() {
        let body = r#"{"code":-1111,"msg":"Not enough balance for this operation"}"#;
        let err = ExchangeApiError::from_response(400, body, "POST /fapi/v1/order".into());
        assert_eq!(err.kind, ApiErrorKind::InsufficientMargin);
        assert!(err.is_margin());
        assert!(err.is_recoverable());
        assert!(!err.is_persistent());
    }

    #[test]
    fn parse_unstructured_error() {
        let err = ExchangeApiError::from_response(500, "Internal Server Error", "GET /fapi/v1/ping".into());
        assert_eq!(err.kind, ApiErrorKind::Unknown);
        assert_eq!(err.code, -500);
    }

    #[test]
    fn network_error() {
        let err = ExchangeApiError::network("connection refused", "POST /fapi/v1/order".into());
        assert_eq!(err.kind, ApiErrorKind::Network);
        assert!(err.is_retryable());
    }

    #[test]
    fn retryable_errors() {
        let body = r#"{"code":-1003,"msg":"Too many requests."}"#;
        let err = ExchangeApiError::from_response(429, body, "GET /fapi/v1/order".into());
        assert!(err.is_retryable());
        assert!(!err.is_fatal());
    }

    #[test]
    fn ip_banned_is_silent_not_retryable() {
        let body = r#"{"code":-1003,"msg":"Way too many requests; IP(1.2.3.4) banned until 1774784983833."}"#;
        let err = ExchangeApiError::from_response(418, body, "GET /dapi/v1/ping".into());
        assert_eq!(err.kind, ApiErrorKind::IpBanned);
        assert!(err.is_ip_banned());
        assert!(err.is_silent());
        assert!(!err.is_fatal());
        assert!(!err.is_retryable());
    }

    #[test]
    fn rate_limited_1003_not_ip_ban_on_429() {
        // Same code -1003 but HTTP 429 (not 418) = regular rate limit, not ban
        let body = r#"{"code":-1003,"msg":"Way too many requests; IP(1.2.3.4) banned until 1774784983833."}"#;
        let err = ExchangeApiError::from_response(429, body, "GET /dapi/v1/ping".into());
        assert_eq!(err.kind, ApiErrorKind::RateLimited);
        assert!(!err.is_fatal());
        assert!(err.is_retryable());
    }

    #[test]
    fn parse_quantity_exceeded() {
        let body = r#"{"code":-4005,"msg":"Quantity greater than max quantity."}"#;
        let err = ExchangeApiError::from_response(400, body, "POST /fapi/v1/order".into());
        assert_eq!(err.kind, ApiErrorKind::QuantityExceeded);
        assert!(err.is_persistent());
        assert!(!err.is_fatal());
        assert!(!err.is_margin());
    }

    #[test]
    fn persistent_excludes_margin_but_covers_quantity() {
        let margin = ExchangeApiError::from_response(
            400,
            r#"{"code":-2019,"msg":"Margin is insufficient."}"#,
            "POST /fapi/v1/order".into(),
        );
        assert!(!margin.is_persistent(), "margin is no longer persistent");
        assert!(margin.is_recoverable());
        assert!(margin.is_margin());

        let qty = ExchangeApiError::from_response(
            400,
            r#"{"code":-4005,"msg":"Quantity greater than max quantity."}"#,
            "POST /fapi/v1/order".into(),
        );
        assert!(qty.is_persistent());
        assert!(!qty.is_recoverable());
        assert!(!qty.is_margin());
    }

    #[test]
    fn parse_modify_limit_exceeded() {
        let body = r#"{"code":-4198,"msg":"Exceed maximum modify order limit."}"#;
        let err = ExchangeApiError::from_response(400, body, "POST /fapi/v1/order/amend".into());
        assert_eq!(err.kind, ApiErrorKind::ModifyLimitExceeded);
        assert!(err.is_modify_limit_exceeded());
        // Cancel+replace is the only handling: it must not be silenced,
        // retried, stopped (fatal/persistent), or paused (recoverable).
        assert!(!err.is_silent());
        assert!(!err.is_retryable());
        assert!(!err.is_fatal());
        assert!(!err.is_persistent());
        assert!(!err.is_recoverable());
    }

    #[test]
    fn parse_min_notional() {
        let body = r#"{"code":-4164,"msg":"Order's notional must be no smaller than 5 (unless you choose reduce only)."}"#;
        let err = ExchangeApiError::from_response(400, body, "POST /fapi/v1/order".into());
        assert_eq!(err.kind, ApiErrorKind::MinNotional);
        assert!(err.is_persistent());
        assert!(!err.is_fatal());
    }

    #[test]
    fn display_format() {
        let body = r#"{"code":-2013,"msg":"Order does not exist."}"#;
        let err = ExchangeApiError::from_response(400, body, "GET /fapi/v1/order".into());
        let s = err.to_string();
        assert!(s.contains("GET /fapi/v1/order"));
        assert!(s.contains("400"));
        assert!(s.contains("-2013"));
    }

    #[test]
    fn margin_is_recoverable_not_persistent() {
        let err = ExchangeApiError {
            kind: ApiErrorKind::InsufficientMargin,
            code: -2019,
            message: "Margin is insufficient.".into(),
            endpoint: "POST /fapi/v1/order".into(),
            http_status: 400,
        };
        assert!(err.is_recoverable(), "margin must classify as recoverable");
        assert!(!err.is_persistent(), "margin must NOT classify as persistent");
        assert!(err.is_margin(), "is_margin still true for code-site readers");
    }

    #[test]
    fn other_persistent_kinds_stay_persistent_and_not_recoverable() {
        for kind in [
            ApiErrorKind::QuantityExceeded,
            ApiErrorKind::MinNotional,
            ApiErrorKind::MaxPositionExceeded,
        ] {
            let err = ExchangeApiError {
                kind,
                code: -9999,
                message: "test".into(),
                endpoint: "test".into(),
                http_status: 400,
            };
            assert!(err.is_persistent(), "{:?} must remain persistent", kind);
            assert!(!err.is_recoverable(), "{:?} must NOT be recoverable", kind);
        }
    }
}
