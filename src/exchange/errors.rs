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
    /// A deterministic defect in the request we built: a filter violation, a
    /// malformed parameter, an illegal flag combination, an endpoint that no
    /// longer exists. Distinct from `PrecisionError` and `MinNotional` only in
    /// that those name a specific cause worth reading in an alert; the
    /// behaviour is the same, and one kind carrying the venue's own code and
    /// wording beats a kind per code.
    ///
    /// The same bytes can never succeed, so it is never retried and it feeds
    /// the breaker: a rejected order still costs the account's order-rate
    /// budget.
    InvalidRequest,
    /// Binance `-1112` NO_DEPTH, `-5041` no BBO: the book is empty on the side
    /// we are trying to trade. A new listing, a halt, or a contract thin
    /// enough to have no resting liquidity.
    ///
    /// Not `Benign` and emphatically not `DuplicateOrderId`, which is where
    /// `-1112` used to land — that made an empty book silent, so a strategy
    /// could hammer one with no alert and no bound. It is transient in
    /// principle, but nothing we do makes it clear, and the venue charges us
    /// for every attempt; so the breaker owns it, as it does the rest of
    /// `Bug`, and the operator is told.
    NoDepth,
    /// The venue-side cap on resting orders for the symbol or account is
    /// full (`-2025` max open orders, `-4045` max stop orders). Retrying
    /// cannot succeed until one of them leaves the book, so the rate
    /// mechanism owns it, not the retry loop.
    MaxOpenOrders,
    /// `-2024`: a reduce-only order larger than the position it would close.
    /// The position we believe in and the one the venue has disagree, so the
    /// answer is to re-read it and resize — not to retry the same size, and
    /// not to stop.
    PositionNotSufficient,
    /// The whole account is restricted from opening new exposure but can
    /// still reduce: liquidation mode (`-2023`), reduce-only restriction
    /// (`-4189`), a cooling-off period (`-4192`), quantitative rules
    /// (`-4400`/`-4401`).
    ///
    /// Neither fatal nor persistent, deliberately. Stopping the strategy
    /// would abandon the exits an open position still needs; retrying is the
    /// -2014-storm shape. Entries stop locally, exits keep flowing, and the
    /// gate re-probes so trading resumes on its own when the restriction
    /// lifts.
    AccountRestricted,
    /// The same condition scoped to one symbol — Binance's position risk
    /// control (`-4105`..`-4107`).
    ///
    /// A separate kind rather than a field, because the runner reads kinds
    /// and never venue codes: the alternative was `matches!(err.code, -4105
    /// | -4106 | -4107)` in the runner, which is exactly the coupling the
    /// classifier exists to prevent. Blocking every symbol because one is
    /// under risk control would be a real over-reach.
    SymbolRestricted,
    /// The symbol is gone, not merely halted: `-4141` SYMBOL_ALREADY_CLOSED,
    /// `-1122` invalid symbol status, `-4140` invalid status for opening a
    /// position. Kept separate from `SymbolNotTrading` (an amend refusal,
    /// which may clear) because a delisting will not.
    SymbolClosed,
    /// The request's goal is already true, so the venue refused it as a
    /// no-op: `-4046` margin type unchanged, `-4059` position mode unchanged,
    /// `-4171` no need to change multi-assets mode.
    ///
    /// Not an error to the caller. The `-4046` case was handled by matching
    /// the code as a SUBSTRING of the error's `Display` output — which also
    /// matches any message that happens to contain those five characters, and
    /// nothing else in this file works that way.
    AlreadyApplied,
    /// The local clock has drifted outside the venue's `recvWindow`:
    /// `-1021`, `-5028` / `-4188` (matching-engine recvWindow reject).
    ///
    /// Not an order error at all — every signed request fails the same way,
    /// including the ones that read state. `spawn_time_sync` normally
    /// prevents it; this is what the runner acts on when that task has died
    /// or a bad link keeps discarding samples.
    ClockSkew,
    /// `-1125`: the user-data listen key no longer exists.
    ///
    /// The bot is blind — fills, cancels and liquidations all arrive on that
    /// stream. This is the condition behind the 2026-06-30 COIN-M incident,
    /// where a dead stream produced 15 phantom positions before anyone
    /// noticed, and it classified as nothing at all.
    ListenKeyDead,
    /// A leverage or margin-configuration change was refused: the value is
    /// outside the symbol's bracket, there are open orders or a position, the
    /// account is in multi-assets mode, and so on.
    ///
    /// Consumed only by the `autoAdjustLeverage` path, never on the order
    /// path. Its whole job is to make a failed clamp say *why* — the -2027
    /// investigation stalled on "leverage clamp failed" with the venue's
    /// reason discarded.
    LeverageRejected,
    /// The request may or may not have executed, and the response does not
    /// say which: Binance -1006 UNEXPECTED_RESP (message-bus desync), -1007
    /// TIMEOUT (the backend answered late), HTTP 500 ("execution status
    /// UNKNOWN" in Binance's own words), a WS-API request that timed out, or
    /// a socket that dropped with the request in flight.
    ///
    /// The distinction from [`ServerBusy`](Self::ServerBusy) is the whole
    /// point: that one is known *not* to have executed and is safe to
    /// re-send, this one is not. Re-sending an order here is how one entry
    /// becomes two, so the runner reads the order back before deciding
    /// anything.
    AmbiguousOutcome,
    /// The venue is temporarily unable to serve the request and says so:
    /// Binance -1001 DISCONNECTED ("Please try again"), -1008 SERVER_BUSY, or
    /// a 502/503/504 with no JSON body. The request never reached the matching
    /// engine, so re-sending it is safe and is what the venue asks for. Before
    /// this kind existed these fell to `Unknown` and were dropped, which is
    /// how an exit edit could silently not happen during a venue blip.
    ServerBusy,
    /// Network/transport error (not an API error).
    Network,
    /// Response deserialization error.
    ParseError,
    /// Unknown/unclassified API error code.
    Unknown,
}

/// What the runner does about a kind — the closed set of behaviours, each with
/// exactly one owning mechanism.
///
/// This exists so that *coverage* is a property of the class, not of the code:
/// a venue error is handled the moment its code reaches a kind, and adding a
/// kind without deciding its behaviour does not compile. The behaviour
/// predicates below (`is_retryable`, `is_persistent`, …) are all derived from
/// this one exhaustive match, so they cannot drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Behaviour {
    /// The request's goal is already true. Not an error to the caller.
    Success,
    /// Transient; re-send with backoff. The request did not execute.
    Retry,
    /// The outcome is unknown — read the order back before deciding anything.
    /// Never blind-retried: the request may have executed.
    Reconcile,
    /// A weight/order budget is exhausted. The rate tracker owns the pause;
    /// retrying inside it only deepens the hole.
    Rate,
    /// The order can no longer be amended. Cancel and place a fresh one.
    Amend,
    /// A deterministic defect in the request we built — the same bytes can
    /// never succeed. Feeds the 3-in-60s breaker, never retried.
    Bug,
    /// Not enough margin/balance right now. Cancel, pause, resize.
    Resource,
    /// The account or symbol is restricted to reduce-only. Entries stop,
    /// exits keep flowing.
    Gate,
    /// The symbol is halted, closed or delisted. Stop that symbol's task.
    Symbol,
    /// The whole account cannot trade. Stop the bot.
    Account,
    /// The credential was rejected for this order. Breaker stops the
    /// strategy; healthy siblings keep running.
    Auth,
    /// Expected during normal operation — the request simply did not need to
    /// happen. Release the slot and move on.
    Benign,
    /// Not an order error: a subsystem is degraded (clock, listen key,
    /// leverage config). Repair it; never touch order flow.
    Health,
    /// Unmapped. §4 of `engine/exchange/ERROR-COVERAGE-PLAN.md`: never retried,
    /// alert throttled, breaker credit on order-mutation paths.
    Unknown,
}

impl ApiErrorKind {
    /// The one place a kind's behaviour is decided.
    ///
    /// Deliberately has no `_` arm: a new kind must be given a behaviour here
    /// or the crate does not build. That is the whole point — before this,
    /// adding a variant cost zero compile errors across six independent
    /// `matches!` lists and silently inherited "do nothing".
    pub fn behaviour(self) -> Behaviour {
        match self {
            Self::AlreadyApplied => Behaviour::Success,

            Self::ClockSkew | Self::ListenKeyDead | Self::LeverageRejected => Behaviour::Health,

            Self::ServerBusy | Self::Network | Self::RateLimited => Behaviour::Retry,

            Self::AmbiguousOutcome
            | Self::CancelReplacePartial
            | Self::CancelReplaceFailed
            | Self::PositionNotSufficient => Behaviour::Reconcile,

            Self::TooManyOrders | Self::IpBanned | Self::MaxOpenOrders => Behaviour::Rate,

            Self::ModifyLimitExceeded => Behaviour::Amend,

            Self::PrecisionError
            | Self::QuantityExceeded
            | Self::MinNotional
            | Self::MaxPositionExceeded
            | Self::InvalidRequest
            | Self::NoDepth => Behaviour::Bug,

            Self::InsufficientMargin => Behaviour::Resource,

            Self::AccountRestricted | Self::SymbolRestricted => Behaviour::Gate,

            Self::SymbolNotTrading | Self::SymbolClosed => Behaviour::Symbol,

            Self::Unauthorized | Self::PositionModeMismatch => Behaviour::Account,

            Self::AuthRejected => Behaviour::Auth,

            Self::OrderNotFound
            | Self::TriggerImmediate
            | Self::ReduceOnlyRejected
            | Self::SamePrice
            | Self::DuplicateOrderId => Behaviour::Benign,

            // A body we could not read tells us nothing, so it gets the
            // Unknown policy rather than a guess.
            Self::ParseError | Self::Unknown => Behaviour::Unknown,
        }
    }

    /// Whether the operator hears about it.
    ///
    /// Orthogonal to [`behaviour`](Self::behaviour) — `Rate` holds both a
    /// silent kind (`TooManyOrders`, which the tracker already reports in
    /// aggregate) and would hold a loud one — so it is its own exhaustive
    /// match, and for the same reason: a new kind must state whether it
    /// alerts.
    pub fn is_silent(self) -> bool {
        match self {
            Self::OrderNotFound
            | Self::SamePrice
            | Self::ReduceOnlyRejected
            | Self::TriggerImmediate
            | Self::DuplicateOrderId
            // -1015 and an IP ban are both reported in aggregate by the
            // rate tracker; per-order lines would just be noise.
            | Self::TooManyOrders
            | Self::IpBanned
            // The venue refusing a change that is already in effect is not
            // news; the call site treats it as the success it is.
            | Self::AlreadyApplied => true,

            // A full order book cap is the rate mechanism's business and
            // recurs constantly on a busy account; the operator hears about
            // it through the pause, not per order.
            Self::MaxOpenOrders => true,

            Self::AmbiguousOutcome
            | Self::ServerBusy
            | Self::Network
            | Self::RateLimited
            | Self::CancelReplacePartial
            | Self::CancelReplaceFailed
            | Self::ModifyLimitExceeded
            | Self::PrecisionError
            | Self::QuantityExceeded
            | Self::MinNotional
            | Self::MaxPositionExceeded
            | Self::InsufficientMargin
            | Self::SymbolNotTrading
            | Self::Unauthorized
            | Self::PositionModeMismatch
            | Self::AuthRejected
            | Self::InvalidRequest
            | Self::NoDepth
            | Self::PositionNotSufficient
            | Self::AccountRestricted
            | Self::SymbolRestricted
            | Self::SymbolClosed
            | Self::ClockSkew
            | Self::ListenKeyDead
            | Self::LeverageRejected
            | Self::ParseError
            | Self::Unknown => false,
        }
    }
}

impl ExchangeApiError {
    /// Parse an error body from a venue whose code space we do not have.
    ///
    /// Keeps the venue's code and message for logs and alerts, but claims no
    /// meaning for either: `kind` is always `Unknown`. A number like `-2021`
    /// means nothing without knowing who sent it, and the wording means little
    /// more — every other kind carries a runner behaviour, up to halting the
    /// account, which is too much to hang on a substring match.
    ///
    /// A venue that knows its own numbers classifies there and builds
    /// `ExchangeApiError` directly — see `binance::parse_binance_error`,
    /// `okx::parse_okx_error` and `bybit::classify_bybit_code`.
    pub fn unclassified(http_status: u16, body: &str, endpoint: String) -> Self {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
            if let (Some(code), Some(msg)) = (v["code"].as_i64(), v["msg"].as_str()) {
                return Self {
                    kind: ApiErrorKind::Unknown,
                    code: code as i32,
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

    /// What the runner does about this error. See [`Behaviour`].
    pub fn behaviour(&self) -> Behaviour {
        self.kind.behaviour()
    }

    /// Account-level fatal: the whole account cannot trade, so the entire bot
    /// (every strategy and symbol) must stop. Only invalid credentials / IP
    /// qualify — there is no per-symbol recovery from these.
    pub fn is_account_fatal(&self) -> bool {
        self.behaviour() == Behaviour::Account
    }

    /// Symbol-level fatal: the affected symbol is halted/delisted (`-4199`),
    /// but the account and every sibling symbol are unaffected. The runner
    /// stops only that symbol's task — it must NOT broadcast a global shutdown.
    pub fn is_symbol_fatal(&self) -> bool {
        self.behaviour() == Behaviour::Symbol
    }

    /// Human-readable reason for fatal errors.
    ///
    /// The runner branches on `fatal_reason().is_some()` rather than on
    /// `is_fatal()`, so the two must agree. They do by construction: the class
    /// decides whether there is a reason at all, and the kind only chooses the
    /// wording.
    pub fn fatal_reason(&self) -> Option<&'static str> {
        match self.behaviour() {
            Behaviour::Account => Some(match self.kind {
                ApiErrorKind::Unauthorized => "invalid API key or IP not whitelisted",
                // No venue's codes here: any adapter can map to this kind, and
                // the error's own code and message are printed alongside this
                // string.
                ApiErrorKind::PositionModeMismatch =>
                    "account position mode (one-way vs hedge) does not match the bot's config \
                     — every order is rejected; align the account's position mode with this \
                     bot's hedgeMode setting",
                _ => "the account cannot trade",
            }),
            Behaviour::Symbol => Some(match self.kind {
                ApiErrorKind::SymbolNotTrading => "symbol not in trading status",
                ApiErrorKind::SymbolClosed => "symbol is closed or delisted",
                _ => "symbol cannot be traded",
            }),
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
        self.behaviour() == Behaviour::Resource
    }

    /// Persistent errors that should stop the strategy (not the bot) after
    /// repeated failures. These signal a permanent mismatch between the
    /// strategy's params and the exchange/account state — retrying with
    /// the same params will keep failing.
    ///
    /// Excludes `InsufficientMargin`, which is recoverable on the wall
    /// clock and routed through `is_recoverable()` instead.
    ///
    /// `Auth` is here as well as `Bug`: a rejected credential is just as
    /// deterministic as a malformed request, and the same breaker stops it.
    pub fn is_persistent(&self) -> bool {
        matches!(self.behaviour(), Behaviour::Bug | Behaviour::Auth)
    }

    /// Errors that are expected and should be handled silently (no Telegram alert).
    /// TooManyOrders (-1015) is silent because the ApiLimitTracker handles it
    /// globally — individual per-order warnings would just spam the logs.
    pub fn is_silent(&self) -> bool {
        self.kind.is_silent()
    }

    /// `-4198`/`-5026`: the per-order amendment cap was hit. Not retryable and not
    /// fatal — the order is permanently un-amendable, so the runner cancels
    /// it and re-places a fresh order (cancel + replace) rather than waiting.
    pub fn is_modify_limit_exceeded(&self) -> bool {
        self.behaviour() == Behaviour::Amend
    }

    /// Errors that can be retried after a short delay.
    ///
    /// Note: `TooManyOrders` (-1015) is NOT retryable — it's a per-minute
    /// order rate limit. Retrying after 1s just adds to the overload. Nor is
    /// `IpBanned`: retrying extends the ban.
    pub fn is_retryable(&self) -> bool {
        self.behaviour() == Behaviour::Retry
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

    /// Without a code table there is nothing to read. The same body that means
    /// "order does not exist" on Binance carries no such promise from a venue
    /// whose space we do not have, so the number is kept for reporting and
    /// nothing is inferred from it.
    #[test]
    fn unclassified_never_interprets_the_numeric_code() {
        let err = ExchangeApiError::unclassified(
            400,
            r#"{"code":-2013,"msg":"some venue wording"}"#,
            "GET /order".into(),
        );
        assert_eq!(err.kind, ApiErrorKind::Unknown, "the number must not be read");
        assert_eq!(err.code, -2013, "but it is kept for reporting");

        // -2015 is account-fatal on Binance. Arriving from an unknown space it
        // must not stop the bot.
        let err = ExchangeApiError::unclassified(
            403,
            r#"{"code":-2015,"msg":"unrelated condition"}"#,
            "POST /order".into(),
        );
        assert!(!err.is_fatal(), "an uninterpreted code can never be fatal");
    }

    /// Wording is not classification. Every kind but `Unknown` carries a runner
    /// behaviour — cancelling entries, halting a strategy, halting the account —
    /// and these messages all used to reach one by substring match.
    #[test]
    fn message_wording_is_never_classified() {
        for msg in [
            "Not enough balance for this operation",
            "Duplicate order sent.",
            "Invalid Api-Key format supplied.",
            "Signature for this request is not valid.",
            "Order's position side does not match user's setting.",
            "something entirely unremarkable",
        ] {
            let body = serde_json::json!({ "code": -9999, "msg": msg }).to_string();
            let err = ExchangeApiError::unclassified(400, &body, "POST /order".into());
            assert_eq!(err.kind, ApiErrorKind::Unknown, "{msg:?}");
            assert_eq!(err.message, msg, "the wording is still reported");
        }
    }

    #[test]
    fn parse_unstructured_error() {
        let err = ExchangeApiError::unclassified(500, "Internal Server Error", "GET /ping".into());
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
        let err = ExchangeApiError::unclassified(
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

    /// The venue asking us to try again is the one case where NOT retrying is
    /// the bug: -1001/-1008 never reached the matching engine, and dropping
    /// them means an exit edit silently doesn't happen.
    #[test]
    fn a_busy_server_is_retried_and_heard() {
        let e = kind(ApiErrorKind::ServerBusy);
        assert_eq!(e.behaviour(), Behaviour::Retry);
        assert!(e.is_retryable());
        assert!(!e.is_silent(), "a venue blip that outlasts the retries must surface");
        assert!(!e.is_fatal() && !e.is_persistent() && !e.is_recoverable());
    }

    /// The two look alike and must never be confused: both mean "the request
    /// failed for a reason that is not our fault", but one is known not to
    /// have executed and the other might have. Re-sending the second is how
    /// one entry becomes two.
    #[test]
    fn an_ambiguous_outcome_is_not_a_transient_fault() {
        let ambiguous = kind(ApiErrorKind::AmbiguousOutcome);
        assert_eq!(ambiguous.behaviour(), Behaviour::Reconcile);
        assert!(!ambiguous.is_retryable(), "the order may already exist");
        assert!(!ambiguous.is_silent(), "the operator must see a reconcile happen");
        assert!(!ambiguous.is_fatal() && !ambiguous.is_persistent() && !ambiguous.is_recoverable());

        assert!(kind(ApiErrorKind::ServerBusy).is_retryable(), "this one did NOT execute");
    }

    /// A restricted account still has positions that need their exits.
    /// Stopping the strategy would abandon them; retrying is the -2014-storm
    /// shape. Only entries stop, and only locally.
    #[test]
    fn a_restricted_account_stops_entries_and_nothing_else() {
        for k in [ApiErrorKind::AccountRestricted, ApiErrorKind::SymbolRestricted] {
            let e = kind(k);
            assert_eq!(e.behaviour(), Behaviour::Gate, "{k:?}");
            assert!(!e.is_fatal(), "{k:?}: positions still need managing");
            assert!(!e.is_persistent(), "{k:?}: the breaker would stand the strategy down");
            assert!(!e.is_retryable(), "{k:?}: this is how -2014 became 470 rejected orders");
            assert!(!e.is_silent(), "{k:?}: the operator must know the bot stopped opening");
        }
    }

    /// -1112 used to be `DuplicateOrderId`, which is silent — so a strategy
    /// could hammer an empty book with no alert and no bound at all.
    #[test]
    fn an_empty_book_is_heard_and_bounded() {
        let e = kind(ApiErrorKind::NoDepth);
        assert_eq!(e.behaviour(), Behaviour::Bug);
        assert!(e.is_persistent(), "the breaker is what bounds it");
        assert!(!e.is_silent(), "and the operator is told why");
        assert!(!e.is_retryable(), "nothing we send makes a book appear");
    }

    /// A closed symbol and a refused amend are both `Symbol`-scoped, but only
    /// one of them will ever clear — worth separate kinds so the message the
    /// operator reads is true.
    #[test]
    fn a_closed_symbol_stops_that_symbol_alone() {
        for k in [ApiErrorKind::SymbolClosed, ApiErrorKind::SymbolNotTrading] {
            let e = kind(k);
            assert!(e.is_symbol_fatal(), "{k:?}");
            assert!(!e.is_account_fatal(), "{k:?} must not stop the whole bot");
            assert!(e.fatal_reason().is_some(), "{k:?}");
        }
        assert_ne!(
            kind(ApiErrorKind::SymbolClosed).fatal_reason(),
            kind(ApiErrorKind::SymbolNotTrading).fatal_reason(),
            "a delisting and a refused amend must not read the same",
        );
    }

    /// A reduce-only order bigger than the position means our view of the
    /// position is wrong. Re-reading is the fix; retrying the same size is not.
    #[test]
    fn a_short_position_is_reconciled_not_retried() {
        let e = kind(ApiErrorKind::PositionNotSufficient);
        assert_eq!(e.behaviour(), Behaviour::Reconcile);
        assert!(!e.is_retryable());
        assert!(!e.is_persistent(), "an exit path must not stand the strategy down");
    }

    /// A degraded subsystem must never look like an order failure. None of
    /// these reach the breaker, the margin pause, or a stand-down — they have
    /// their own repair, and the order path is not involved.
    #[test]
    fn health_conditions_never_touch_order_flow() {
        for k in [
            ApiErrorKind::ClockSkew,
            ApiErrorKind::ListenKeyDead,
            ApiErrorKind::LeverageRejected,
        ] {
            let e = kind(k);
            assert_eq!(e.behaviour(), Behaviour::Health, "{k:?}");
            assert!(!e.is_fatal(), "{k:?}");
            assert!(!e.is_persistent(), "{k:?}");
            assert!(!e.is_recoverable(), "{k:?}");
            assert!(!e.is_retryable(), "{k:?}: repair first, then the caller may re-ask");
            assert!(!e.is_silent(), "{k:?}: being blind or out of sync must be heard");
        }
    }

    /// The venue refusing a change that is already in effect is the outcome
    /// the caller wanted.
    #[test]
    fn an_already_applied_config_change_is_a_success() {
        let e = kind(ApiErrorKind::AlreadyApplied);
        assert_eq!(e.behaviour(), Behaviour::Success);
        assert!(e.is_silent(), "there is nothing to report");
        assert!(!e.is_fatal() && !e.is_persistent() && !e.is_retryable());
    }

    /// Every predicate now reads one exhaustive `behaviour()`, so a kind
    /// cannot be in two mechanisms at once. This asserts the disjointness the
    /// old independent `matches!` lists only had by convention.
    #[test]
    fn behaviour_classes_own_disjoint_mechanisms() {
        for (b, k) in [
            (Behaviour::Retry, ApiErrorKind::Network),
            (Behaviour::Reconcile, ApiErrorKind::CancelReplacePartial),
            (Behaviour::Rate, ApiErrorKind::TooManyOrders),
            (Behaviour::Amend, ApiErrorKind::ModifyLimitExceeded),
            (Behaviour::Bug, ApiErrorKind::PrecisionError),
            (Behaviour::Resource, ApiErrorKind::InsufficientMargin),
            (Behaviour::Symbol, ApiErrorKind::SymbolNotTrading),
            (Behaviour::Account, ApiErrorKind::Unauthorized),
            (Behaviour::Auth, ApiErrorKind::AuthRejected),
            (Behaviour::Benign, ApiErrorKind::OrderNotFound),
            (Behaviour::Unknown, ApiErrorKind::Unknown),
        ] {
            let e = kind(k);
            assert_eq!(e.behaviour(), b, "{k:?}");
            assert_eq!(e.is_retryable(), b == Behaviour::Retry, "{k:?} retryable");
            assert_eq!(e.is_recoverable(), b == Behaviour::Resource, "{k:?} recoverable");
            assert_eq!(e.is_account_fatal(), b == Behaviour::Account, "{k:?} account-fatal");
            assert_eq!(e.is_symbol_fatal(), b == Behaviour::Symbol, "{k:?} symbol-fatal");
            assert_eq!(e.is_modify_limit_exceeded(), b == Behaviour::Amend, "{k:?} amend");
            assert_eq!(
                e.is_persistent(),
                matches!(b, Behaviour::Bug | Behaviour::Auth),
                "{k:?} persistent",
            );
        }
    }

    /// `handle_order_error` branches on `fatal_reason().is_some()`, not on
    /// `is_fatal()`. They agree by construction — this pins it for every kind
    /// a class can hold, including ones added later that never get their own
    /// wording.
    #[test]
    fn fatal_reason_agrees_with_is_fatal() {
        for k in [
            ApiErrorKind::Unauthorized,
            ApiErrorKind::PositionModeMismatch,
            ApiErrorKind::SymbolNotTrading,
            ApiErrorKind::PrecisionError,
            ApiErrorKind::Network,
            ApiErrorKind::ServerBusy,
            ApiErrorKind::Unknown,
        ] {
            let e = kind(k);
            assert_eq!(e.fatal_reason().is_some(), e.is_fatal(), "{k:?}");
        }
    }

    /// An unmapped code has no behaviour to inherit. The safety it does get —
    /// throttled alerts and breaker credit on order paths — is the runner's
    /// (§4 of the coverage plan), and depends on this staying inert here.
    #[test]
    fn unknown_carries_no_behaviour() {
        for k in [ApiErrorKind::Unknown, ApiErrorKind::ParseError] {
            let e = kind(k);
            assert_eq!(e.behaviour(), Behaviour::Unknown, "{k:?}");
            assert!(!e.is_retryable(), "{k:?} must never be blind-retried");
            assert!(!e.is_fatal() && !e.is_persistent() && !e.is_recoverable(), "{k:?}");
            assert!(!e.is_silent(), "{k:?} must reach the operator");
        }
    }
}
