use std::fmt;

use crate::types::MarketType;

/// Which venue's numeric error space a response body belongs to.
///
/// A raw `{"code":-2021,...}` is meaningless without knowing who sent it and
/// from which market. Binance splits its space by market type — the same
/// number carries different meanings on futures and Spot (`-2021` is "order
/// would immediately trigger" on USD-M/COIN-M but a cancel-replace partial
/// failure on Spot) — so the market must travel with the body all the way
/// into classification.
///
/// Venues that classify their own codes (OKX's `classify_okx_code`, Bybit's
/// `retCode` wrapper, HTX's `err-code`) never route through the Binance table:
/// they pass `Other`, which falls back to message inspection only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeSpace {
    /// Binance, in a specific market's code space.
    Binance(MarketType),
    /// A venue whose numeric codes this table does not describe. Message
    /// keywords only — never the Binance code table.
    Other,
}

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
    /// Parse an exchange error response body into a typed error.
    ///
    /// Tries to extract `{"code":-XXXX,"msg":"..."}` (Binance format).
    /// Falls back to Unknown if the body is not structured JSON.
    pub fn from_response(
        http_status: u16,
        body: &str,
        endpoint: String,
        space: CodeSpace,
    ) -> Self {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
            if let (Some(code), Some(msg)) = (v["code"].as_i64(), v["msg"].as_str()) {
                let code = code as i32;
                // HTTP 418 + "banned" = IP ban (not just rate limit)
                let kind = if http_status == 418 && msg.to_lowercase().contains("banned") {
                    ApiErrorKind::IpBanned
                } else {
                    classify_code(code, msg, space)
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

fn classify_code(code: i32, msg: &str, space: CodeSpace) -> ApiErrorKind {
    match space {
        CodeSpace::Binance(market) => classify_binance(code, msg, market),
        // No numeric table for this venue — the code is not ours to read.
        // Message keywords are the only safe signal.
        CodeSpace::Other => classify_by_message(msg),
    }
}

/// Binance's numeric space, resolved against the market the response came from.
///
/// Two classes of code live here:
///
/// 1. **Conflicting** — the same number means different things per market.
///    These MUST be resolved before the shared table, and are the entire
///    reason `CodeSpace` carries a `MarketType`. Today that is Spot's
///    `-2021`/`-2022` (cancel-replace outcomes) against futures'
///    `TriggerImmediate`/`ReduceOnlyRejected`.
/// 2. **Disjoint** — USD-M and COIN-M number the same condition differently
///    (`-4164`/`-4178` min-notional, `-5027`/`-4197` same-price,
///    `-5026`/`-4198` amend cap). Those numbers never collide, so the shared
///    table below accepts both spellings on any futures market. Gating them
///    per-market would buy no correctness and would reject valid fixtures.
fn classify_binance(code: i32, msg: &str, market: MarketType) -> ApiErrorKind {
    // (1) Conflicting codes — market decides the meaning.
    if market == MarketType::Spot {
        match code {
            -2021 => return ApiErrorKind::CancelReplacePartial,
            -2022 => return ApiErrorKind::CancelReplaceFailed,
            _ => {}
        }
    }

    // (2) Shared table.
    match code {
        -2013 | -2011 => ApiErrorKind::OrderNotFound,
        -2021 => ApiErrorKind::TriggerImmediate,
        -2022 => ApiErrorKind::ReduceOnlyRejected,
        // -4197 COIN-M / -5027 USD-M: "No need to modify the order."
        -4197 | -5027 => ApiErrorKind::SamePrice,
        -2019 => ApiErrorKind::InsufficientMargin,
        -2015 => ApiErrorKind::Unauthorized,
        -2014 | -1022 => ApiErrorKind::AuthRejected,
        -4199 => ApiErrorKind::SymbolNotTrading,
        -4061 => ApiErrorKind::PositionModeMismatch,
        -1015 => ApiErrorKind::TooManyOrders,
        -4005 => ApiErrorKind::QuantityExceeded,
        -2027 => ApiErrorKind::MaxPositionExceeded,
        // -4164 USD-M / -4178 COIN-M: order notional below the venue minimum.
        -4164 | -4178 => ApiErrorKind::MinNotional,
        -1111 => ApiErrorKind::PrecisionError,
        // -4198 COIN-M / -5026 USD-M: per-order amendment cap reached.
        -4198 | -5026 => ApiErrorKind::ModifyLimitExceeded,
        -1003 => ApiErrorKind::RateLimited,
        -1112 => ApiErrorKind::DuplicateOrderId,
        _ => classify_by_message(msg),
    }
}

/// Last-resort classification for a code this table does not describe —
/// an unmapped Binance code, or any code from a venue with no table at all.
fn classify_by_message(msg: &str) -> ApiErrorKind {
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

    /// Classify a Binance body in the USD-M space. Every pre-existing test
    /// used the (then market-blind) Binance table, so USD-M is the faithful
    /// stand-in; the codes whose meaning actually depends on the market get
    /// their own explicit-market tests below.
    fn binance_err(http_status: u16, body: &str, endpoint: &str) -> ExchangeApiError {
        ExchangeApiError::from_response(
            http_status,
            body,
            endpoint.to_string(),
            CodeSpace::Binance(MarketType::Linear),
        )
    }

    fn classify_at(code: i32, msg: &str, space: CodeSpace) -> ApiErrorKind {
        let body = serde_json::json!({ "code": code, "msg": msg }).to_string();
        ExchangeApiError::from_response(400, &body, "POST /order".into(), space).kind
    }

    /// The reason `CodeSpace` carries a market at all: -2021/-2022 are
    /// "would immediately trigger" / "reduceOnly rejected" on futures, but
    /// cancel-replace outcomes on Spot. Before keying, a failed Spot amend
    /// classified as a silent futures condition and was swallowed whole.
    #[test]
    fn conflicting_codes_resolve_by_market() {
        for futures in [MarketType::Linear, MarketType::Inverse] {
            let space = CodeSpace::Binance(futures);
            assert_eq!(
                classify_at(-2021, "Order would immediately trigger.", space),
                ApiErrorKind::TriggerImmediate,
                "{futures:?} -2021",
            );
            assert_eq!(
                classify_at(-2022, "ReduceOnly Order is rejected.", space),
                ApiErrorKind::ReduceOnlyRejected,
                "{futures:?} -2022",
            );
        }

        let spot = CodeSpace::Binance(MarketType::Spot);
        assert_eq!(
            classify_at(-2021, "Order cancel-replace partially failed.", spot),
            ApiErrorKind::CancelReplacePartial,
        );
        assert_eq!(
            classify_at(-2022, "Order cancel-replace failed.", spot),
            ApiErrorKind::CancelReplaceFailed,
        );
    }

    /// Both cancel-replace outcomes must reach the operator. The pre-keying
    /// mapping put them on `TriggerImmediate`/`ReduceOnlyRejected`, both of
    /// which are silent — so a Spot order could vanish with no alert.
    #[test]
    fn spot_cancel_replace_failures_are_never_silent() {
        for (code, kind) in [
            (-2021, ApiErrorKind::CancelReplacePartial),
            (-2022, ApiErrorKind::CancelReplaceFailed),
        ] {
            let err = ExchangeApiError::from_response(
                400,
                &serde_json::json!({ "code": code, "msg": "cancel-replace" }).to_string(),
                "POST /api/v3/order/cancelReplace".into(),
                CodeSpace::Binance(MarketType::Spot),
            );
            assert_eq!(err.kind, kind);
            assert!(!err.is_silent(), "{code} must alert");
            assert!(!err.is_retryable(), "{code} must not be blindly retried");
            assert!(!err.is_fatal());
            assert!(!err.is_persistent());
        }
    }

    /// USD-M and COIN-M spell the same condition with different numbers.
    /// These never collide, so both spellings resolve on either futures
    /// market rather than being gated behind the exact one.
    #[test]
    fn disjoint_usdm_and_coinm_spellings_both_resolve() {
        for market in [MarketType::Linear, MarketType::Inverse] {
            let space = CodeSpace::Binance(market);
            // min notional: -4164 USD-M, -4178 COIN-M
            assert_eq!(classify_at(-4164, "notional", space), ApiErrorKind::MinNotional);
            assert_eq!(classify_at(-4178, "notional", space), ApiErrorKind::MinNotional);
            // same price: -5027 USD-M, -4197 COIN-M
            assert_eq!(classify_at(-5027, "no need to modify", space), ApiErrorKind::SamePrice);
            assert_eq!(classify_at(-4197, "no need to modify", space), ApiErrorKind::SamePrice);
            // amend cap: -5026 USD-M, -4198 COIN-M
            assert_eq!(
                classify_at(-5026, "exceed modify limit", space),
                ApiErrorKind::ModifyLimitExceeded,
            );
            assert_eq!(
                classify_at(-4198, "exceed modify limit", space),
                ApiErrorKind::ModifyLimitExceeded,
            );
        }
    }

    /// A venue with its own table (OKX, Bybit, HTX, …) must never be read
    /// through Binance's numbers. Only message keywords may apply.
    #[test]
    fn other_venues_never_use_the_binance_table() {
        // -2013 is "order does not exist" on Binance and nothing here.
        assert_eq!(
            classify_at(-2013, "some other venue wording", CodeSpace::Other),
            ApiErrorKind::Unknown,
        );
        // -2015 is account-fatal on Binance; it must not stop a bot on a
        // venue where that number means something else entirely.
        assert_eq!(
            classify_at(-2015, "unrelated condition", CodeSpace::Other),
            ApiErrorKind::Unknown,
        );
        // Message fallback still applies — that part is venue-agnostic.
        assert_eq!(
            classify_at(51008, "Order placement failed due to insufficient balance", CodeSpace::Other),
            ApiErrorKind::InsufficientMargin,
        );
    }

    /// IP-ban detection keys off HTTP 418 + wording, not the numeric table,
    /// so it must survive on a venue with no table.
    #[test]
    fn ip_ban_detection_is_space_agnostic() {
        let body = r#"{"code":-1003,"msg":"Way too many requests; IP(1.2.3.4) banned until 1774784983833."}"#;
        for space in [CodeSpace::Binance(MarketType::Linear), CodeSpace::Other] {
            let err = ExchangeApiError::from_response(418, body, "GET /ping".into(), space);
            assert_eq!(err.kind, ApiErrorKind::IpBanned, "{space:?}");
        }
    }

    #[test]
    fn parse_binance_error() {
        let body = r#"{"code":-2013,"msg":"Order does not exist."}"#;
        let err = binance_err(400, body, "GET /fapi/v1/order".into());
        assert_eq!(err.kind, ApiErrorKind::OrderNotFound);
        assert_eq!(err.code, -2013);
        assert!(!err.is_fatal());
        assert!(err.is_silent());
    }

    #[test]
    fn parse_fatal_error() {
        let body = r#"{"code":-2015,"msg":"Invalid API-key, IP, or permissions for action."}"#;
        let err = binance_err(403, body, "POST /fapi/v1/order".into());
        assert_eq!(err.kind, ApiErrorKind::Unauthorized);
        assert!(err.is_fatal());
        assert_eq!(err.fatal_reason(), Some("invalid API key or IP not whitelisted"));
    }

    #[test]
    fn order_auth_rejection_is_persistent_not_fatal() {
        // -2014 (the 2026-07-18 emu→live storm): a live order rejected for a
        // malformed/empty key must self-halt the STRATEGY (persistent breaker),
        // NOT stop the whole bot (account-fatal) — paper siblings keep running.
        let err = binance_err(
            401,
            r#"{"code":-2014,"msg":"API-key format invalid."}"#,
            "POST /dapi/v1/order".into(),
        );
        assert_eq!(err.kind, ApiErrorKind::AuthRejected);
        assert!(err.is_persistent(), "auth-rejected orders feed the 3-in-60s breaker");
        assert!(!err.is_fatal(), "must not stop the whole bot");
        assert!(!err.is_account_fatal());
        assert!(!err.is_retryable(), "retrying with the same bad key just spams");
    }

    #[test]
    fn signature_error_and_message_fallback_are_auth_rejected() {
        // Mapped code -1022.
        let sig = binance_err(
            400,
            r#"{"code":-1022,"msg":"Signature for this request is not valid."}"#,
            "POST /dapi/v1/order".into(),
        );
        assert_eq!(sig.kind, ApiErrorKind::AuthRejected);
        // Unmapped code but auth-worded message → still AuthRejected via fallback.
        let fallback = binance_err(
            401,
            r#"{"code":-9999,"msg":"Invalid Api-Key format supplied."}"#,
            "POST /dapi/v1/order".into(),
        );
        assert_eq!(fallback.kind, ApiErrorKind::AuthRejected);
        assert!(fallback.is_persistent());
    }

    #[test]
    fn unauthorized_is_account_fatal_not_symbol_fatal() {
        let err = binance_err(
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
        let err = binance_err(
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
    fn position_mode_mismatch_is_account_fatal() {
        // -4061 (the 2026-07-18 COIN-M v0.2.1 storm): the account's position
        // mode (one-way vs hedge) disagrees with what the bot sends, so EVERY
        // order of every strategy is rejected deterministically. It must stop
        // the whole bot on the first occurrence — not retry, not per-symbol,
        // not per-strategy — because only an operator account change clears it.
        let err = binance_err(
            400,
            r#"{"code":-4061,"msg":"Order's position side does not match user's setting."}"#,
            "POST /dapi/v1/order".into(),
        );
        assert_eq!(err.kind, ApiErrorKind::PositionModeMismatch);
        assert!(err.is_fatal());
        assert!(err.is_account_fatal(), "-4061 must stop the whole bot");
        assert!(!err.is_symbol_fatal());
        assert!(!err.is_persistent(), "account-fatal, not the per-strategy breaker");
        assert!(!err.is_retryable(), "retrying against a mismatched mode just spams -4061");
        assert!(!err.is_silent(), "operator must be alerted");
        assert!(err.fatal_reason().is_some());
        // Message fallback: -4061 worded without its code still classifies.
        let fallback = binance_err(
            400,
            r#"{"code":-9999,"msg":"Order's position side does not match user's setting."}"#,
            "POST /dapi/v1/order".into(),
        );
        assert_eq!(fallback.kind, ApiErrorKind::PositionModeMismatch);
    }

    #[test]
    fn parse_margin_error() {
        let body = r#"{"code":-2019,"msg":"Margin is insufficient."}"#;
        let err = binance_err(400, body, "POST /fapi/v1/order".into());
        assert_eq!(err.kind, ApiErrorKind::InsufficientMargin);
        assert!(err.is_margin());
        assert!(err.is_recoverable(), "margin is recoverable");
        assert!(!err.is_persistent(), "margin must NOT be persistent");
        assert!(!err.is_fatal());
    }

    #[test]
    fn parse_unknown_code_with_margin_message() {
        let body = r#"{"code":-9876,"msg":"Not enough balance for this operation"}"#;
        let err = binance_err(400, body, "POST /fapi/v1/order".into());
        assert_eq!(err.kind, ApiErrorKind::InsufficientMargin);
        assert!(err.is_margin());
        assert!(err.is_recoverable());
        assert!(!err.is_persistent());
    }

    #[test]
    fn parse_precision_error_is_persistent() {
        // A symbol missing from the pair-info cache gets ordered with an
        // unrounded quantity. The exchange rejects with -1111
        // deterministically — the same request can never succeed — so the
        // persistent-error breaker must stop the strategy instead of
        // retrying forever: rejected orders still count against the
        // account's order-rate limits and starve every other symbol.
        let body = r#"{"code":-1111,"msg":"Precision is over the maximum defined for this asset."}"#;
        let err = binance_err(400, body, "WS order.place".into());
        assert_eq!(err.kind, ApiErrorKind::PrecisionError);
        assert!(err.is_persistent(), "-1111 must trip the persistent breaker");
        assert!(!err.is_recoverable(), "retrying the same request cannot succeed");
        assert!(!err.is_retryable());
        assert!(!err.is_fatal());
        assert!(!err.is_margin());
        assert!(!err.is_silent(), "operator must see the alert when the breaker trips");
    }

    #[test]
    fn precision_error_code_takes_precedence_over_message_keywords() {
        // Code-based classification must win even if the message happens to
        // contain a keyword-fallback trigger word like "exceeds".
        let body = r#"{"code":-1111,"msg":"Precision exceeds the maximum for this asset."}"#;
        let err = binance_err(400, body, "POST /fapi/v1/order".into());
        assert_eq!(err.kind, ApiErrorKind::PrecisionError);
        assert!(err.is_persistent());
    }

    #[test]
    fn parse_unstructured_error() {
        let err = binance_err(500, "Internal Server Error", "GET /fapi/v1/ping".into());
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
        let err = binance_err(429, body, "GET /fapi/v1/order".into());
        assert!(err.is_retryable());
        assert!(!err.is_fatal());
    }

    #[test]
    fn ip_banned_is_silent_not_retryable() {
        let body = r#"{"code":-1003,"msg":"Way too many requests; IP(1.2.3.4) banned until 1774784983833."}"#;
        let err = binance_err(418, body, "GET /dapi/v1/ping".into());
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
        let err = binance_err(429, body, "GET /dapi/v1/ping".into());
        assert_eq!(err.kind, ApiErrorKind::RateLimited);
        assert!(!err.is_fatal());
        assert!(err.is_retryable());
    }

    #[test]
    fn parse_quantity_exceeded() {
        let body = r#"{"code":-4005,"msg":"Quantity greater than max quantity."}"#;
        let err = binance_err(400, body, "POST /fapi/v1/order".into());
        assert_eq!(err.kind, ApiErrorKind::QuantityExceeded);
        assert!(err.is_persistent());
        assert!(!err.is_fatal());
        assert!(!err.is_margin());
    }

    #[test]
    fn persistent_excludes_margin_but_covers_quantity() {
        let margin = binance_err(
            400,
            r#"{"code":-2019,"msg":"Margin is insufficient."}"#,
            "POST /fapi/v1/order".into(),
        );
        assert!(!margin.is_persistent(), "margin is no longer persistent");
        assert!(margin.is_recoverable());
        assert!(margin.is_margin());

        let qty = binance_err(
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
        let err = binance_err(400, body, "POST /fapi/v1/order/amend".into());
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
    fn parse_modify_limit_exceeded_ws_api() {
        // Same amendment-cap condition as -4198, but reported as -5026 by
        // the WS API (`order.modify`) — the path live edits actually take.
        let body = r#"{"code":-5026,"msg":"Exceed maximum modify order limit."}"#;
        let err = binance_err(400, body, "WS order.modify".into());
        assert_eq!(err.kind, ApiErrorKind::ModifyLimitExceeded);
        assert!(err.is_modify_limit_exceeded());
        assert!(!err.is_silent());
        assert!(!err.is_retryable());
        assert!(!err.is_fatal());
        assert!(!err.is_persistent());
        assert!(!err.is_recoverable());
    }

    #[test]
    fn parse_min_notional() {
        let body = r#"{"code":-4164,"msg":"Order's notional must be no smaller than 5 (unless you choose reduce only)."}"#;
        let err = binance_err(400, body, "POST /fapi/v1/order".into());
        assert_eq!(err.kind, ApiErrorKind::MinNotional);
        assert!(err.is_persistent());
        assert!(!err.is_fatal());
    }

    #[test]
    fn display_format() {
        let body = r#"{"code":-2013,"msg":"Order does not exist."}"#;
        let err = binance_err(400, body, "GET /fapi/v1/order".into());
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
