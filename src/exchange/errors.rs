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
    /// -2021: Stop price would trigger immediately.
    SlTriggerPrice,
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
    /// -4164: Order notional below exchange minimum.
    MinNotional,
    /// Duplicate client order ID (order already placed with this ID).
    DuplicateOrderId,
    /// -1003: Too many requests (rate limit hit).
    RateLimited,
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
                let kind = classify_code(code, msg);
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

    /// Fatal errors that should stop all trading immediately.
    pub fn is_fatal(&self) -> bool {
        matches!(self.kind, ApiErrorKind::Unauthorized | ApiErrorKind::SymbolNotTrading)
    }

    /// Human-readable reason for fatal errors.
    pub fn fatal_reason(&self) -> Option<&'static str> {
        match self.kind {
            ApiErrorKind::Unauthorized => Some("invalid API key or IP not whitelisted"),
            ApiErrorKind::SymbolNotTrading => Some("symbol not in trading status"),
            _ => None,
        }
    }

    /// Errors indicating insufficient margin/balance.
    pub fn is_margin(&self) -> bool {
        self.kind == ApiErrorKind::InsufficientMargin
    }

    /// Persistent errors that should stop the strategy (not the bot) after repeated failures.
    /// Covers margin errors and quantity-exceeded — both indicate the strategy's parameters
    /// are incompatible with the current account/exchange state.
    pub fn is_persistent(&self) -> bool {
        matches!(self.kind, ApiErrorKind::InsufficientMargin | ApiErrorKind::QuantityExceeded | ApiErrorKind::MinNotional)
    }

    /// Errors that are expected and should be handled silently (no Telegram alert).
    pub fn is_silent(&self) -> bool {
        matches!(
            self.kind,
            ApiErrorKind::OrderNotFound
                | ApiErrorKind::SamePrice
                | ApiErrorKind::ReduceOnlyRejected
                | ApiErrorKind::SlTriggerPrice
                | ApiErrorKind::DuplicateOrderId
        )
    }

    /// Errors that can be retried after a short delay.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self.kind,
            ApiErrorKind::Network | ApiErrorKind::RateLimited | ApiErrorKind::TooManyOrders
        )
    }
}

fn classify_code(code: i32, msg: &str) -> ApiErrorKind {
    match code {
        -2013 | -2011 => ApiErrorKind::OrderNotFound,
        -2021 => ApiErrorKind::SlTriggerPrice,
        -2022 => ApiErrorKind::ReduceOnlyRejected,
        -4197 => ApiErrorKind::SamePrice,
        -2019 => ApiErrorKind::InsufficientMargin,
        -2015 => ApiErrorKind::Unauthorized,
        -4199 => ApiErrorKind::SymbolNotTrading,
        -1015 => ApiErrorKind::TooManyOrders,
        -4005 | -2027 => ApiErrorKind::QuantityExceeded,
        -4164 => ApiErrorKind::MinNotional,
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
    fn parse_margin_error() {
        let body = r#"{"code":-2019,"msg":"Margin is insufficient."}"#;
        let err = ExchangeApiError::from_response(400, body, "POST /fapi/v1/order".into());
        assert_eq!(err.kind, ApiErrorKind::InsufficientMargin);
        assert!(err.is_margin());
        assert!(!err.is_fatal());
    }

    #[test]
    fn parse_unknown_code_with_margin_message() {
        let body = r#"{"code":-1111,"msg":"Not enough balance for this operation"}"#;
        let err = ExchangeApiError::from_response(400, body, "POST /fapi/v1/order".into());
        assert_eq!(err.kind, ApiErrorKind::InsufficientMargin);
        assert!(err.is_margin());
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
    fn parse_quantity_exceeded() {
        let body = r#"{"code":-4005,"msg":"Quantity greater than max quantity."}"#;
        let err = ExchangeApiError::from_response(400, body, "POST /fapi/v1/order".into());
        assert_eq!(err.kind, ApiErrorKind::QuantityExceeded);
        assert!(err.is_persistent());
        assert!(!err.is_fatal());
        assert!(!err.is_margin());
    }

    #[test]
    fn persistent_covers_margin_and_quantity() {
        let margin = ExchangeApiError::from_response(
            400,
            r#"{"code":-2019,"msg":"Margin is insufficient."}"#,
            "POST /fapi/v1/order".into(),
        );
        assert!(margin.is_persistent());
        assert!(margin.is_margin());

        let qty = ExchangeApiError::from_response(
            400,
            r#"{"code":-4005,"msg":"Quantity greater than max quantity."}"#,
            "POST /fapi/v1/order".into(),
        );
        assert!(qty.is_persistent());
        assert!(!qty.is_margin());
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
}
