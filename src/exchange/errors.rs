use std::fmt;

/// Classified exchange API error.
///
/// The venue-neutral vocabulary the runner reasons about. Each adapter maps
/// its own numeric codes into [`ApiErrorKind`] — that mapping is venue-specific
/// and lives beside the adapter (`binance::parse_binance_error`,
/// `okx::parse_okx_error`, `bybit::classify_bybit_code`). What lives here is
/// the taxonomy and the behaviour predicates, so `is_fatal()` means the same
/// thing whichever venue produced the error.
#[derive(Debug, Clone)]
pub struct ExchangeApiError {
    pub kind: ApiErrorKind,
    pub code: i32,
    pub message: String,
    /// HTTP method + path (e.g. "POST /fapi/v1/order").
    pub endpoint: String,
    pub http_status: u16,
}

/// What went wrong, in terms the runner acts on.
///
/// Codes cited below are Binance's, as the venue these were first derived
/// from — they are illustrative, not definitional. Every adapter maps its own
/// space onto these kinds, and the same kind can arrive from any venue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiErrorKind {
    /// e.g. Binance -2013/-2011: Order does not exist (already filled/canceled).
    OrderNotFound,
    /// -2021: Conditional order (stop/TP/SL) would trigger immediately.
    TriggerImmediate,
    /// -2022: ReduceOnly order rejected (position already closed).
    ReduceOnlyRejected,
    /// -4197 (COIN-M) / -5027 (USD-M): No need to modify order (same price).
    SamePrice,
    /// -2019, -1000 w/ margin msg: Insufficient margin/balance.
    InsufficientMargin,
    /// -2015: Invalid API key, secret, or IP not whitelisted (FATAL).
    Unauthorized,
    /// -2014 / -1022: an individual ORDER placement was rejected for auth
    /// reasons — a malformed/empty API key ("API-key format invalid") or a
    /// bad request signature. Unlike `Unauthorized` (-2015, account-fatal),
    /// this stops only the offending STRATEGY after repeated failures
    /// (persistent breaker), leaving paper and healthy sibling strategies
    /// running. Prevents a misconfigured-live strategy (or a mid-session key
    /// rotation) from spamming rejected live orders — 2026-07-18 emu→live
    /// incident: a paper strategy switched to live on a keyless bot placed
    /// 470 rejected orders in ~2 min before it was stopped by hand.
    AuthRejected,
    /// -4199: Symbol is not in trading status (FATAL).
    SymbolNotTrading,
    /// -4061: "Order's position side does not match user's setting" — the
    /// account's Binance position mode (one-way vs hedge/dual-side) disagrees
    /// with what the bot sends: hedge-format orders carry `positionSide=
    /// LONG/SHORT`, one-way orders omit it, and the exchange rejects the
    /// mismatch. This is an ACCOUNT-wide misconfiguration — it fails every
    /// order of every strategy (entries, exits, closes) deterministically, and
    /// only clears when the operator aligns the account mode with the config,
    /// so it's account-fatal: stop the whole bot on the first occurrence rather
    /// than retry-loop rejected orders against the account's rate limits.
    /// 2026-07-18 COIN-M v0.2.1 incident: ~200 rejected orders/sec until the
    /// bot was stopped by hand. `ensure_position_mode` normally prevents this
    /// at startup; this classification catches a mode that flips mid-session.
    PositionModeMismatch,
    /// -1015: Too many orders.
    TooManyOrders,
    /// -4005: Quantity exceeds max allowed.
    QuantityExceeded,
    /// -2027: Exceeded maximum allowable position at current leverage.
    /// Persistent: stops the strategy after repeated failures to prevent API spam.
    MaxPositionExceeded,
    /// -4164 (USD-M) / -4178 (COIN-M): Order notional below exchange minimum.
    MinNotional,
    /// -1111: Price/quantity precision over the maximum defined for the
    /// symbol. Deterministic — the same request can never succeed. Reaching
    /// the exchange with this error means order values were built without
    /// (or with stale) pair metadata; the persistent breaker stops the
    /// strategy instead of retrying, because rejected orders still count
    /// against the account's order-rate limits.
    PrecisionError,
    /// -4198 (COIN-M) / -5026 (USD-M): per-order amendment cap reached. The
    /// order can never be modified again — the runner cancels it and places
    /// a fresh, amendable order.
    ModifyLimitExceeded,
    /// Duplicate client order ID (order already placed with this ID).
    DuplicateOrderId,
    /// Spot `-2021`: a cancel-replace where exactly ONE leg succeeded. The
    /// resting order is most likely gone with nothing put back in its place,
    /// so the amend cannot be treated as a no-op — the caller has to re-read
    /// order state. Spot-only: on USD-M/COIN-M this same number means
    /// `TriggerImmediate`, which is why classification is keyed by market.
    CancelReplacePartial,
    /// Spot `-2022`: a cancel-replace where BOTH legs failed. The original
    /// order is still resting, so nothing was lost — but the amend did not
    /// happen and the caller must not assume the new price took effect.
    /// Spot-only: on USD-M/COIN-M this number means `ReduceOnlyRejected`.
    CancelReplaceFailed,
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
    /// Parse an exchange error response body into a typed error, without
    /// interpreting the venue's numeric code.
    ///
    /// A number like `-2021` means nothing without knowing who sent it, so
    /// this venue-agnostic path keeps the code for reporting and classifies
    /// by message alone. An adapter that knows its own code space classifies
    /// there and builds `ExchangeApiError` directly — see
    /// `binance::parse_binance_error` and `okx::parse_okx_error`.
    ///
    /// Falls back to Unknown if the body is not structured JSON.
    pub fn from_response(http_status: u16, body: &str, endpoint: String) -> Self {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
            if let (Some(code), Some(msg)) = (v["code"].as_i64(), v["msg"].as_str()) {
                let code = code as i32;
                return Self {
                    kind: classify_by_message(msg),
                    code,
                    message: msg.to_string(),
                    endpoint,
                    http_status,
                };
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
        matches!(self.kind, ApiErrorKind::Unauthorized | ApiErrorKind::PositionModeMismatch)
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
            ApiErrorKind::PositionModeMismatch => Some(
                "account position mode (one-way vs hedge) does not match the bot's config \
                 — every order is rejected (-4061); align the account's Binance position \
                 mode with this bot's hedgeMode setting"),
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
        matches!(self.kind, ApiErrorKind::QuantityExceeded | ApiErrorKind::MinNotional | ApiErrorKind::MaxPositionExceeded | ApiErrorKind::PrecisionError | ApiErrorKind::AuthRejected)
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

    /// `-4198`/`-5026`: the per-order amendment cap was hit. Not retryable and not
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

/// Venue-agnostic classification from the error message alone.
///
/// The fallback for a code no table describes — an unmapped code inside a
/// venue's own classifier, or any code from a venue with no table at all.
/// Deliberately conservative: it returns `Unknown` unless the wording is
/// unambiguous, because every kind it can return carries a runner behaviour.
pub fn classify_by_message(msg: &str) -> ApiErrorKind {
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
    } else if lower.contains("api-key")
        || lower.contains("apikey")
        || lower.contains("api key")
        || lower.contains("signature")
    {
        // Auth rejection on an order (bad/empty key, bad signature)
        // that didn't carry a mapped code — persistent (strategy
        // self-halt), not account-fatal.
        ApiErrorKind::AuthRejected
    } else if lower.contains("position side") {
        // -4061 worded without its code (e.g. a differently-wrapped
        // WS API error) → still an account position-mode mismatch.
        ApiErrorKind::PositionModeMismatch
    } else {
        ApiErrorKind::Unknown
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

    /// The venue-agnostic parser must NOT read the number. The same body that
    /// means "order does not exist" on Binance carries no such promise from a
    /// venue whose space we do not have — classification falls to the message.
    #[test]
    fn from_response_never_interprets_the_numeric_code() {
        let err = ExchangeApiError::from_response(
            400,
            r#"{"code":-2013,"msg":"some venue wording"}"#,
            "GET /order".into(),
        );
        assert_eq!(err.kind, ApiErrorKind::Unknown, "the number must not be read");
        assert_eq!(err.code, -2013, "but it is kept for reporting");

        // -2015 is account-fatal on Binance. Arriving from an unknown space it
        // must not stop the bot.
        let err = ExchangeApiError::from_response(
            403,
            r#"{"code":-2015,"msg":"unrelated condition"}"#,
            "POST /order".into(),
        );
        assert!(!err.is_fatal(), "an uninterpreted code can never be fatal");
    }

    #[test]
    fn message_keywords_classify_without_a_code_table() {
        for (msg, want) in [
            ("Not enough balance for this operation", ApiErrorKind::InsufficientMargin),
            ("Duplicate order sent.", ApiErrorKind::DuplicateOrderId),
            ("Invalid Api-Key format supplied.", ApiErrorKind::AuthRejected),
            ("Signature for this request is not valid.", ApiErrorKind::AuthRejected),
            ("Order's position side does not match user's setting.", ApiErrorKind::PositionModeMismatch),
            ("something entirely unremarkable", ApiErrorKind::Unknown),
        ] {
            assert_eq!(classify_by_message(msg), want, "{msg:?}");
        }
    }

    #[test]
    fn parse_unstructured_error() {
        let err = ExchangeApiError::from_response(500, "Internal Server Error", "GET /ping".into());
        assert_eq!(err.kind, ApiErrorKind::Unknown);
        assert_eq!(err.code, -500);
    }

    #[test]
    fn network_error() {
        let err = ExchangeApiError::network("connection refused", "POST /order".into());
        assert_eq!(err.kind, ApiErrorKind::Network);
        assert!(err.is_retryable());
    }

    #[test]
    fn parse_error_is_its_own_kind() {
        let err = ExchangeApiError::parse("expected value", "GET /order".into(), 200);
        assert_eq!(err.kind, ApiErrorKind::ParseError);
        assert!(!err.is_retryable());
        assert!(!err.is_fatal());
    }

    #[test]
    fn display_format() {
        let err = ExchangeApiError::from_response(
            400,
            r#"{"code":-2013,"msg":"Order does not exist."}"#,
            "GET /fapi/v1/order".into(),
        );
        let s = err.to_string();
        assert!(s.contains("GET /fapi/v1/order"));
        assert!(s.contains("400"));
        assert!(s.contains("-2013"));
    }

    // Behaviour predicates. These are the contract the runner reads, so they
    // are asserted against kinds directly — no venue, no codes.

    fn kind(kind: ApiErrorKind) -> ExchangeApiError {
        ExchangeApiError {
            kind,
            code: -1,
            message: "test".into(),
            endpoint: "test".into(),
            http_status: 400,
        }
    }

    #[test]
    fn fatal_scopes_are_disjoint_and_named() {
        let acct = kind(ApiErrorKind::Unauthorized);
        assert!(acct.is_fatal() && acct.is_account_fatal() && !acct.is_symbol_fatal());
        assert!(acct.fatal_reason().is_some());

        let sym = kind(ApiErrorKind::SymbolNotTrading);
        assert!(sym.is_fatal() && sym.is_symbol_fatal() && !sym.is_account_fatal());
        assert!(sym.fatal_reason().is_some());

        let mode = kind(ApiErrorKind::PositionModeMismatch);
        assert!(mode.is_account_fatal(), "a mismatched account mode stops the bot");
        assert!(!mode.is_persistent(), "account-fatal, not the per-strategy breaker");
    }

    #[test]
    fn margin_is_recoverable_and_never_persistent() {
        let margin = kind(ApiErrorKind::InsufficientMargin);
        assert!(margin.is_recoverable());
        assert!(margin.is_margin());
        assert!(!margin.is_persistent(), "margin frees up on the wall clock");
    }

    #[test]
    fn persistent_kinds_are_not_recoverable() {
        for k in [
            ApiErrorKind::QuantityExceeded,
            ApiErrorKind::MinNotional,
            ApiErrorKind::MaxPositionExceeded,
            ApiErrorKind::PrecisionError,
            ApiErrorKind::AuthRejected,
        ] {
            assert!(kind(k).is_persistent(), "{k:?} must be persistent");
            assert!(!kind(k).is_recoverable(), "{k:?} must not be recoverable");
            assert!(!kind(k).is_retryable(), "{k:?} must not be retried");
        }
    }

    #[test]
    fn silent_kinds_are_the_expected_ones() {
        for k in [
            ApiErrorKind::OrderNotFound,
            ApiErrorKind::SamePrice,
            ApiErrorKind::ReduceOnlyRejected,
            ApiErrorKind::TriggerImmediate,
            ApiErrorKind::DuplicateOrderId,
            ApiErrorKind::TooManyOrders,
            ApiErrorKind::IpBanned,
        ] {
            assert!(kind(k).is_silent(), "{k:?} must not alert");
        }
        // A vanished order must always reach the operator.
        for k in [ApiErrorKind::CancelReplacePartial, ApiErrorKind::CancelReplaceFailed] {
            assert!(!kind(k).is_silent(), "{k:?} must alert");
            assert!(!kind(k).is_retryable(), "{k:?} must not be blindly retried");
        }
    }

    #[test]
    fn amend_cap_is_handled_by_cancel_replace_only() {
        let err = kind(ApiErrorKind::ModifyLimitExceeded);
        assert!(err.is_modify_limit_exceeded());
        // Not silenced, retried, stopped, or paused — the runner re-places it.
        assert!(!err.is_silent());
        assert!(!err.is_retryable());
        assert!(!err.is_fatal());
        assert!(!err.is_persistent());
        assert!(!err.is_recoverable());
    }

    #[test]
    fn only_network_and_rate_limit_retry() {
        assert!(kind(ApiErrorKind::Network).is_retryable());
        assert!(kind(ApiErrorKind::RateLimited).is_retryable());
        assert!(!kind(ApiErrorKind::IpBanned).is_retryable(), "retrying extends the ban");
        assert!(!kind(ApiErrorKind::TooManyOrders).is_retryable(), "retrying adds to the overload");
    }
}
