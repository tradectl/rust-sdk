//! The indicator boundary: what a strategy asks for, and what it gets back.
//!
//! Only plain data lives here. The indicator *implementations* live in
//! `tradectl-indicators` (in the engine repo), which this crate deliberately
//! does not depend on — `rust-sdk` is the leaf every strategy and engine links,
//! and depending on the engine repo would make it unbuildable on its own. So
//! the contract across the plugin ABI is two `#[repr(C)]` structs: a request
//! describing an indicator, and a reading of one.
//!
//! The consequence worth understanding: a strategy never holds indicator state.
//! It declares what it needs through [`Strategy::indicators`], the engine builds
//! and feeds exactly those, and each event's readings arrive in
//! [`StrategyContext::indicators`] in the same order as the declaration. Two
//! things fall out of that:
//!
//! - **A sweep pays for an indicator once, not once per trial.** Identical
//!   requests across trials collapse to one instance, because an indicator
//!   depends only on market data and never on params. Note what that does and
//!   does not cover: 191 trials all asking for `Ema(20)` share one instance,
//!   but a grid *over the period* — `Ema(10)`…`Ema(200)` — is 191 distinct
//!   computations and costs 191. [`IndicatorKind::Sma`] is the exception, and
//!   for a mathematical reason rather than an optimisation: it is the only kind
//!   with a closed form across periods, so one prefix-sum ring answers all of
//!   them. Everything else is recursive — each value depends on the previous
//!   one *for that period* — so period 50 cannot be read out of state built for
//!   period 20.
//! - **Adding an indicator kind costs no ABI change.** The kinds are values of
//!   [`IndicatorKind`], not fields; a new one is an implementation in
//!   `tradectl-indicators` plus a variant here.
//!
//! [`Strategy::indicators`]: crate::Strategy::indicators
//! [`StrategyContext::indicators`]: crate::StrategyContext::indicators

/// Bumped whenever a discriminant of [`IndicatorKind`] changes meaning —
/// reordered, removed, or reused. Appending a kind does not require a bump.
///
/// The layout fingerprint hashes sizes and offsets, and a discriminant is
/// neither: swap two variants and a stale plugin asking for `Ema = 1` gets
/// whatever the engine now calls `1`, with the version check and the
/// fingerprint both passing. Same blind spot as a vtable, same fix as
/// [`BATCH_TRAIT_REVISION`] — a hand-turned number folded into the hash.
///
/// [`BATCH_TRAIT_REVISION`]: crate::strategy::batch::BATCH_TRAIT_REVISION
pub const INDICATOR_KIND_REVISION: u32 = 1;

/// Which indicator to compute.
///
/// `#[repr(u16)]` because this crosses the plugin ABI inside
/// [`IndicatorRequest`]. Append new kinds at the end; if you must reorder or
/// remove one, bump [`INDICATOR_KIND_REVISION`].
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndicatorKind {
    /// Simple moving average — the arithmetic mean of the last `period` closes.
    ///
    /// The only kind with a closed form across periods, so the engine serves
    /// every SMA period *and* lag on one interval from a single prefix-sum
    /// ring. See the note on sharing above: this is the one family where a
    /// period grid is free.
    Sma = 0,
    /// Exponential moving average, `k = 2/(period+1)`.
    Ema = 1,
    /// Relative strength index, Wilder-smoothed. 0…100.
    Rsi = 2,
    /// Moving-average convergence/divergence line (fast EMA − slow EMA).
    /// `period` = fast, `aux[0]` = slow, `aux[1]` = signal.
    Macd = 3,
    /// The MACD signal line — the EMA of the MACD line.
    MacdSignal = 4,
    /// MACD line − signal line.
    MacdHistogram = 5,
    /// Bollinger middle band — the simple average. `aux[0]` = σ × 100
    /// (`0` means the conventional 2σ).
    BollingerMid = 6,
    /// Middle band + σ × the multiplier.
    BollingerUpper = 7,
    /// Middle band − σ × the multiplier.
    BollingerLower = 8,
    /// Average true range, over each bar's high/low/close.
    ///
    /// Needs the bar's range, so a seeded (close-only) history leaves it warm
    /// but flat until live bars replace the seed.
    Atr = 9,
    /// Volume-weighted average price, cumulative over the run. Like `Atr`, it
    /// reads more than the close — a close-only seed carries no volume.
    Vwap = 10,
    /// Population standard deviation of the last `period` closes.
    StdDev = 11,
}

/// The underlying computation behind one or more [`IndicatorKind`]s.
///
/// MACD's three outputs, and Bollinger's three bands, each come off a single
/// instance. Identity is by family so a strategy asking for the line *and* the
/// signal pays for one MACD, not two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndicatorFamily {
    Sma,
    Ema,
    Rsi,
    Macd,
    Bollinger,
    Atr,
    Vwap,
    StdDev,
}

impl IndicatorKind {
    /// Which computation produces this reading.
    pub fn family(&self) -> IndicatorFamily {
        match self {
            Self::Sma => IndicatorFamily::Sma,
            Self::Ema => IndicatorFamily::Ema,
            Self::Rsi => IndicatorFamily::Rsi,
            Self::Macd | Self::MacdSignal | Self::MacdHistogram => IndicatorFamily::Macd,
            Self::BollingerMid | Self::BollingerUpper | Self::BollingerLower => {
                IndicatorFamily::Bollinger
            }
            Self::Atr => IndicatorFamily::Atr,
            Self::Vwap => IndicatorFamily::Vwap,
            Self::StdDev => IndicatorFamily::StdDev,
        }
    }
}

/// Where an indicator's bars come from.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndicatorSource {
    /// Bars aggregated locally from the trade stream. The default, and the only
    /// source that behaves identically in live, replay and backtest — the same
    /// builder decides where a boundary falls in all three.
    Trade = 0,
    /// The venue's own klines. Chart-exact, but a prepared backtest file's kline
    /// segment can legitimately be empty.
    Kline = 1,
}

/// One indicator a strategy wants the engine to maintain for it.
///
/// Construct with [`IndicatorRequest::new`] and the builder methods rather than
/// by literal, so a future field cannot break every call site — the mistake
/// that made adding one MA field a four-repo change.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IndicatorRequest {
    pub kind: IndicatorKind,
    pub source: IndicatorSource,
    /// Averaging length in bars (MACD: the fast length).
    pub period: u32,
    /// Bar width. Zero is rejected by the engine and clamped to one second.
    pub interval_ms: u32,
    /// Read the value as it stood `lag` closed bars ago. `0` is current.
    ///
    /// This is how you get slope and cross without the engine holding an
    /// opinion about either: request the same indicator twice, once lagged, and
    /// subtract. On an SMA it is free — the shared series already holds the
    /// history.
    pub lag: u32,
    /// Kind-specific parameters, unused slots left zero. MACD reads `[slow,
    /// signal, _, _]`; Bollinger reads σ×100 from `aux[0]`.
    ///
    /// A fixed array rather than named fields because the arity is frozen by the
    /// ABI fingerprint: a kind wanting a third parameter (Ichimoku's three
    /// periods and displacement) must fit here or force a version bump. Four is
    /// what the widest indicator in common use needs.
    pub aux: [u32; 4],
}

impl IndicatorRequest {
    /// An indicator of `kind` over `period` bars of `interval_sec`, from the
    /// trade stream, read at the current bar.
    pub fn new(kind: IndicatorKind, period: u32, interval_sec: u32) -> Self {
        Self {
            kind,
            source: IndicatorSource::Trade,
            period,
            interval_ms: interval_sec.max(1) * 1000,
            lag: 0,
            aux: [0; 4],
        }
    }

    /// Read this indicator as it stood `bars` closed bars ago.
    pub fn lagged(mut self, bars: u32) -> Self {
        self.lag = bars;
        self
    }

    /// Take bars from the venue's klines instead of the local trade stream.
    pub fn from_klines(mut self) -> Self {
        self.source = IndicatorSource::Kline;
        self
    }

    /// Set the leading kind-specific parameters (MACD slow + signal, Bollinger σ).
    pub fn with_aux(mut self, a: u32, b: u32) -> Self {
        self.aux[0] = a;
        self.aux[1] = b;
        self
    }

    /// Set every kind-specific parameter, for kinds needing more than two.
    pub fn with_aux_all(mut self, aux: [u32; 4]) -> Self {
        self.aux = aux;
        self
    }

    /// The instance this request reads from — everything that affects the
    /// computation, with the read offsets (`lag`) and the output selection
    /// (which `kind` of one family) collapsed away.
    ///
    /// Two requests with the same key share one indicator. That is what makes a
    /// sweep pay once rather than once per trial, and what stops "MACD line" and
    /// "MACD signal" from being two MACDs.
    pub fn instance_key(&self) -> IndicatorInstanceKey {
        IndicatorInstanceKey {
            family: self.kind.family(),
            source: self.source,
            period: self.period,
            interval_ms: self.interval_ms,
            aux: self.aux,
        }
    }
}

/// Identity of a single maintained indicator — see
/// [`IndicatorRequest::instance_key`]. Not part of the ABI; the engine uses it
/// to decide what to build and share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IndicatorInstanceKey {
    pub family: IndicatorFamily,
    pub source: IndicatorSource,
    pub period: u32,
    pub interval_ms: u32,
    pub aux: [u32; 4],
}

/// One indicator's reading, as of the event being dispatched.
///
/// `ready` is false until the indicator has enough bars. `value` is unspecified
/// while not ready — read it through [`StrategyContext::indicator`], which
/// returns `None` instead.
///
/// [`StrategyContext::indicator`]: crate::StrategyContext::indicator
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IndicatorValue {
    pub value: f64,
    pub ready: bool,
}

impl IndicatorValue {
    /// The reading a not-yet-warm indicator reports.
    ///
    /// NaN, not zero, and deliberately: a strategy that ignores `ready` and
    /// writes `price > ma` compares against this number directly. Against `0.0`
    /// that is true for every price, so a cold indicator would wave through
    /// every entry — the failure reads as a working filter. Every comparison
    /// against NaN is false instead, so the same bug refuses to trade.
    pub const COLD: Self = Self { value: f64::NAN, ready: false };

    pub fn ready(value: f64) -> Self {
        Self { value, ready: true }
    }

    /// The value, or `None` while cold.
    #[inline]
    pub fn get(&self) -> Option<f64> {
        self.ready.then_some(self.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builders_compose_without_a_literal() {
        let r = IndicatorRequest::new(IndicatorKind::Sma, 20, 5)
            .lagged(3)
            .from_klines()
            .with_aux(7, 9);
        assert_eq!(r.period, 20);
        assert_eq!(r.interval_ms, 5_000);
        assert_eq!(r.lag, 3);
        assert_eq!(r.source, IndicatorSource::Kline);
        assert_eq!(r.aux, [7, 9, 0, 0]);
    }

    #[test]
    fn a_zero_interval_cannot_produce_a_zero_width_bar() {
        assert_eq!(IndicatorRequest::new(IndicatorKind::Ema, 10, 0).interval_ms, 1_000);
    }

    /// Lag is a read offset, not a different indicator: the two must collapse to
    /// one instance or a sweep pays twice for the same series.
    #[test]
    fn lagged_reads_share_an_instance_with_their_unlagged_twin() {
        let now = IndicatorRequest::new(IndicatorKind::Sma, 50, 60);
        let back = now.lagged(5);
        assert_ne!(now, back);
        assert_eq!(now.instance_key(), back.instance_key());
    }

    /// The three MACD outputs, and the three Bollinger bands, must each come
    /// off one instance — otherwise asking for a line and its signal quietly
    /// computes the same EMAs twice.
    #[test]
    fn outputs_of_one_family_share_an_instance() {
        let line = IndicatorRequest::new(IndicatorKind::Macd, 12, 60).with_aux(26, 9);
        for other in [IndicatorKind::MacdSignal, IndicatorKind::MacdHistogram] {
            let o = IndicatorRequest { kind: other, ..line };
            assert_ne!(line.kind, o.kind);
            assert_eq!(line.instance_key(), o.instance_key(), "{other:?}");
        }
        let mid = IndicatorRequest::new(IndicatorKind::BollingerMid, 20, 60).with_aux(200, 0);
        let up = IndicatorRequest { kind: IndicatorKind::BollingerUpper, ..mid };
        assert_eq!(mid.instance_key(), up.instance_key());
        // …but a different family never does.
        assert_ne!(line.instance_key(), mid.instance_key());
    }

    #[test]
    fn differing_period_interval_source_or_aux_are_distinct_instances() {
        let base = IndicatorRequest::new(IndicatorKind::Sma, 50, 60);
        for other in [
            IndicatorRequest::new(IndicatorKind::Sma, 51, 60),
            IndicatorRequest::new(IndicatorKind::Sma, 50, 30),
            IndicatorRequest::new(IndicatorKind::Ema, 50, 60),
            base.from_klines(),
            base.with_aux(1, 0),
        ] {
            assert_ne!(base.instance_key(), other.instance_key(), "{other:?}");
        }
    }

    #[test]
    fn a_cold_reading_yields_nothing() {
        assert_eq!(IndicatorValue::COLD.get(), None);
        assert_eq!(IndicatorValue::ready(1.5).get(), Some(1.5));
    }

    /// The reason `COLD` is NaN rather than zero. A strategy that skips the
    /// `ready` check and compares against the raw value is writing a bug either
    /// way — this pins which way the bug fails. Against `0.0` every price is
    /// above the average and the cold filter passes everything; against NaN
    /// every comparison is false and it trades nothing.
    #[test]
    // The negated comparisons are the point: a strategy that skips the ready
    // check writes `price > ma`, and this pins that EVERY ordering against a
    // cold (NaN) value is false. `partial_cmp` would express the same fact in
    // a form no strategy actually writes.
    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    fn a_cold_value_read_raw_refuses_rather_than_admits() {
        let cold = IndicatorValue::COLD.value;
        for price in [0.01, 1.0, 50_000.0] {
            assert!(!(price > cold), "price {price} must not read as above a cold indicator");
            assert!(!(price < cold), "nor below it");
            assert!(!(price == cold), "nor equal to it");
        }
    }

    #[test]
    fn aux_slots_beyond_the_first_two_survive_a_round_trip() {
        let r = IndicatorRequest::new(IndicatorKind::Sma, 9, 60).with_aux_all([1, 2, 3, 4]);
        assert_eq!(r.aux, [1, 2, 3, 4]);
        // Identity covers every slot, so a kind using the later ones does not
        // silently collapse two distinct requests onto one instance.
        let other = IndicatorRequest::new(IndicatorKind::Sma, 9, 60).with_aux_all([1, 2, 3, 5]);
        assert_ne!(r.instance_key(), other.instance_key());
    }
}
