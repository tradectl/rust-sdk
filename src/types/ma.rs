//! Moving averages over closed bars — every period at once, O(1) per update.
//!
//! ## Why not one `Sma` per period
//!
//! A sweep over `maPeriod=10:200:1` is 191 periods. One ring buffer per period
//! is 191 updates per bar for values that are bit-identical across trials. An MA
//! is a function of *market data only*, never of strategy parameters, so one
//! [`MaSeries`] serves every period and every trial: the indicator cost becomes
//! O(1) in trial count instead of O(N).
//!
//! ## How
//!
//! One ring of the last `CAP` closes plus one parallel ring of running sums:
//!
//! ```text
//! S[n]  = S[n-1] + close[n]
//! MA(p) = (S[n] - S[n-p]) * inv_p[p]      // 1 sub + 1 mul, any p <= CAP
//! ```
//!
//! Update is one store and one add regardless of how many periods anyone reads;
//! a period nobody reads costs nothing. `inv_p[p]` is precomputed, so there is no
//! division on the read path.
//!
//! ## The precision trap
//!
//! `S[n]` grows without bound while `S[n] - S[n-p]` stays small — on a stream of
//! 40 000-scale prices running for weeks that difference loses significant bits
//! (catastrophic cancellation, silent, and it corrupts long periods first). The
//! prefix ring is therefore **re-baselined** every `max_period` pushes: one O(CAP)
//! pass that resets the oldest in-window prefix to zero, which is amortised O(1)
//! and bounds `|S|` to roughly `CAP × price`.

use crate::types::events::TradeEvent;
use crate::types::params::Params;

/// Hard ceiling on a series' capacity. 1000 closes cost 16 KB per series, which
/// is small enough that the limit never has to be revisited.
pub const MA_MAX_PERIOD: usize = 1000;

/// How an engine should build its [`MaSeries`], read from strategy params.
///
/// `Params` is `f64`-only, so every knob is numeric — there is no
/// `maInterval: "1m"`, only `maIntervalSec`.
///
/// | Param | Meaning | Default |
/// |---|---|---|
/// | `maPeriod` | the period the strategy reads; **0 disables MA entirely** | 0 |
/// | `maMaxPeriod` | series capacity. Sweeps set this to the grid maximum so every trial shares one series and one warmup | `maPeriod` |
/// | `maIntervalSec` | bar length in seconds (60 = 1m, 300 = 5m) | 60 |
/// | `maSource` | 0 = aggregate bars from the trade stream, 1 = exchange klines | 0 |
/// | `maSlopeBars` | slope lookback; widens the capacity so `slope()` can answer | 0 |
/// | `maWarmupBars` | bars to sit out before entries are allowed | `maMaxPeriod` |
///
/// `maPeriod = 0` means no series is built and no code path changes, so
/// existing replay baselines stay valid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MaConfig {
    /// Series capacity — the largest period anyone will ask for.
    pub max_period: usize,
    pub interval_ms: u32,
    /// Bars come from the venue's klines rather than from local aggregation.
    pub from_klines: bool,
    /// Bars to accumulate before the engine lets the strategy enter.
    ///
    /// Defaults to `max_period`, which is what keeps a sweep honest: with a
    /// per-trial warmup, short periods would start trading hours before long
    /// ones and `compute_score` — which multiplies by
    /// `min(trades/min_trades, 1)` — would rank them higher for a reason that
    /// has nothing to do with edge. One capacity, one warmup, same start bar
    /// for every trial.
    pub warmup_bars: usize,
}

impl MaConfig {
    /// Read MA settings from strategy params. `None` when `maPeriod` is 0 or
    /// absent — the disabled path, where no series is allocated.
    pub fn from_params(params: &Params) -> Option<Self> {
        let period = params.get("maPeriod", 0.0);
        if !(period >= 1.0) {
            return None;
        }
        let period = period as usize;
        // `slope(p, back)` reads `p + back` bars, so a series sized to exactly
        // `maPeriod` can never answer one — it would return `None` forever and
        // a slope-gated strategy would hold for the entire run without ever
        // saying why. Size the capacity to cover the slope lookback too.
        let slope_bars = params.get("maSlopeBars", 0.0).max(0.0) as usize;
        let max_period = (params.get("maMaxPeriod", period as f64) as usize)
            .max(period + slope_bars)
            .clamp(1, MA_MAX_PERIOD);
        let interval_ms = (params.get("maIntervalSec", 60.0).max(1.0) * 1000.0) as u32;
        Some(Self {
            max_period,
            interval_ms,
            from_klines: params.get("maSource", 0.0) as i64 == 1,
            warmup_bars: params.get("maWarmupBars", max_period as f64).max(0.0) as usize,
        })
    }

    /// One config covering a whole trial grid — the shape sweep and shadow
    /// need, where each trial has its own `maPeriod` but they share a series.
    ///
    /// Capacity and warmup take the maximum across the grid: capacity because
    /// the series must answer the largest period anyone asks for, warmup
    /// because a shared start bar is the only way trials are comparable.
    /// Interval and source come from the first trial that configures MA;
    /// trials that disagree are ignored, since one series can only have one
    /// bar length.
    pub fn merge_grid<'a>(grid: impl IntoIterator<Item = &'a Params>) -> Option<Self> {
        let mut merged: Option<Self> = None;
        for params in grid {
            let Some(c) = Self::from_params(params) else { continue };
            merged = Some(match merged {
                None => c,
                Some(m) => Self {
                    max_period: m.max_period.max(c.max_period),
                    warmup_bars: m.warmup_bars.max(c.warmup_bars),
                    ..m
                },
            });
        }
        merged
    }

    /// An empty series sized for this config.
    pub fn series(&self) -> MaSeries {
        MaSeries::new(self.max_period, self.interval_ms)
    }

    /// A bar builder matching this config's interval.
    pub fn builder(&self) -> CandleBuilder {
        CandleBuilder::new(self.interval_ms)
    }

    /// The venue's name for this interval, for `fetch_klines` warmup seeding.
    ///
    /// `None` for a bar length no exchange offers — local aggregation still
    /// works there, the series just has to warm up from live trades instead of
    /// being seeded.
    pub fn interval_label(&self) -> Option<&'static str> {
        Some(match self.interval_ms {
            60_000 => "1m",
            180_000 => "3m",
            300_000 => "5m",
            900_000 => "15m",
            1_800_000 => "30m",
            3_600_000 => "1h",
            7_200_000 => "2h",
            14_400_000 => "4h",
            21_600_000 => "6h",
            43_200_000 => "12h",
            86_400_000 => "1d",
            _ => return None,
        })
    }
}

/// Moving averages over the last `p` closed bars, for every `p` at once.
///
/// `value(p)` is the arithmetic mean of the last `p` closes — what charting
/// tools label SMA and what "MA" means unqualified. This is the only kind
/// implemented, and deliberately so: the arithmetic mean is the only common MA
/// with a closed form across periods, which is what keeps the whole series O(1)
/// no matter how many periods exist. Weighted and recursive variants (EMA, HMA)
/// cost work per configured period and would need an explicit period list.
///
/// Cheap to read, so read it — do **not** cache "the MA last bar" in strategy
/// state. [`value_at`](Self::value_at) and [`slope`](Self::slope) give you bar
/// history for free, and a private copy is exactly the desync class that the
/// cached pending-entry latch caused (ABI 6→7).
#[derive(Debug, Clone)]
pub struct MaSeries {
    /// Ring of the last `max_period + 1` closes, indexed by `bar_number % len`.
    closes: Vec<f64>,
    /// Parallel ring of running sums: `prefix[n] = prefix[n-1] + closes[n]`,
    /// periodically re-baselined so it cannot grow without bound.
    prefix: Vec<f64>,
    /// `inv_p[p] == 1.0 / p` (index 0 unused) — keeps division off the read path.
    inv_p: Vec<f64>,
    max_period: usize,
    /// Total closed bars ever pushed. Also the ring cursor: the newest bar lives
    /// at `bars % closes.len()`.
    bars: u64,
    since_rebase: usize,
    /// Price of the in-progress (not yet closed) bar, if the feeder reports one.
    forming: Option<f64>,
    interval_ms: u32,
}

impl MaSeries {
    /// Create a series able to answer every period in `1..=max_period`.
    ///
    /// `max_period` is clamped to `1..=`[`MA_MAX_PERIOD`]. `interval_ms` is
    /// carried for the reader's benefit only — the series never looks at time,
    /// it only counts bars, so whoever feeds it decides what a bar is.
    pub fn new(max_period: usize, interval_ms: u32) -> Self {
        let max_period = max_period.clamp(1, MA_MAX_PERIOD);
        // One extra slot so `value(max_period)` can read S[n] and S[n-max_period]
        // from two distinct slots.
        let len = max_period + 1;
        let mut inv_p = vec![0.0; len];
        for (p, inv) in inv_p.iter_mut().enumerate().skip(1) {
            *inv = 1.0 / p as f64;
        }
        Self {
            closes: vec![0.0; len],
            prefix: vec![0.0; len],
            inv_p,
            max_period,
            bars: 0,
            since_rebase: 0,
            forming: None,
            interval_ms,
        }
    }

    /// Append one closed bar. Amortised O(1) — one store and one add, plus an
    /// O(`max_period`) re-baseline once every `max_period` pushes.
    #[inline]
    pub fn push_close(&mut self, close: f64) {
        let len = self.closes.len() as u64;
        self.bars += 1;
        let slot = (self.bars % len) as usize;
        let prev = ((self.bars - 1) % len) as usize;
        self.closes[slot] = close;
        self.prefix[slot] = self.prefix[prev] + close;
        self.forming = None;

        self.since_rebase += 1;
        if self.since_rebase >= self.max_period {
            self.rebase();
        }
    }

    /// Append `count` bars that all closed at the same price. Used to fill the
    /// gap left by intervals with no trades, so `maPeriod` keeps meaning the
    /// same span of wall-clock time on a thin symbol as on a busy one.
    #[inline]
    pub fn push_close_repeat(&mut self, close: f64, count: u32) {
        for _ in 0..count {
            self.push_close(close);
        }
    }

    /// Report the price of the bar currently forming. Only [`value_live`] reads
    /// it; it is discarded by the next [`push_close`].
    ///
    /// [`value_live`]: Self::value_live
    /// [`push_close`]: Self::push_close
    #[inline]
    pub fn set_forming(&mut self, price: f64) {
        self.forming = Some(price);
    }

    /// Warm the series from historical closes (oldest first). Only the last
    /// `max_period` are kept. Replaces any existing content.
    pub fn seed(&mut self, closes: &[f64]) {
        self.reset();
        let start = closes.len().saturating_sub(self.max_period);
        for &c in &closes[start..] {
            self.push_close(c);
        }
    }

    /// Drop all bars. Capacity, period table and interval survive.
    pub fn reset(&mut self) {
        self.closes.fill(0.0);
        self.prefix.fill(0.0);
        self.bars = 0;
        self.since_rebase = 0;
        self.forming = None;
    }

    /// `MA(p)` over the last `p` **closed** bars, or `None` until `p` bars exist.
    ///
    /// Stable within a bar, so a signal built on it cannot flip-flop intra-bar.
    /// This is the default reader; prefer it to [`value_live`](Self::value_live).
    #[inline]
    pub fn value(&self, p: usize) -> Option<f64> {
        if !self.ready(p) {
            return None;
        }
        let len = self.closes.len() as u64;
        let newest = self.prefix[(self.bars % len) as usize];
        let oldest = self.prefix[((self.bars - p as u64) % len) as usize];
        Some((newest - oldest) * self.inv_p[p])
    }

    /// `MA(p)` including the bar currently forming, over `p-1` closed bars plus
    /// the live price. Re-evaluates every tick and will chatter — use it only
    /// where that is wanted. Falls back to [`value`](Self::value) when no
    /// forming price has been reported.
    #[inline]
    pub fn value_live(&self, p: usize) -> Option<f64> {
        let Some(forming) = self.forming else {
            return self.value(p);
        };
        if p == 0 || p > self.max_period {
            return None;
        }
        let closed = (p - 1) as u64;
        if self.bars < closed {
            return None;
        }
        let len = self.closes.len() as u64;
        let newest = self.prefix[(self.bars % len) as usize];
        let oldest = self.prefix[((self.bars - closed) % len) as usize];
        Some((newest - oldest + forming) * self.inv_p[p])
    }

    /// `MA(p)` as it stood `back` closed bars ago. Needs `p + back <= max_period`.
    #[inline]
    pub fn value_at(&self, p: usize, back: usize) -> Option<f64> {
        if p == 0 || p + back > self.max_period {
            return None;
        }
        let need = (p + back) as u64;
        if self.bars < need {
            return None;
        }
        let len = self.closes.len() as u64;
        let n = self.bars - back as u64;
        let newest = self.prefix[(n % len) as usize];
        let oldest = self.prefix[((n - p as u64) % len) as usize];
        Some((newest - oldest) * self.inv_p[p])
    }

    /// Change in `MA(p)` over the last `back` bars — positive is rising.
    ///
    /// Absolute price units, not a percentage; divide by
    /// [`value`](Self::value) if you want one.
    #[inline]
    pub fn slope(&self, p: usize, back: usize) -> Option<f64> {
        Some(self.value(p)? - self.value_at(p, back)?)
    }

    /// Whether `fast` crossed `slow` on the most recent bar:
    /// `1` = crossed up, `-1` = crossed down, `0` = no cross.
    ///
    /// `None` until both periods are ready one bar back.
    #[inline]
    pub fn cross(&self, fast: usize, slow: usize) -> Option<i8> {
        let now = self.value(fast)? - self.value(slow)?;
        let prev = self.value_at(fast, 1)? - self.value_at(slow, 1)?;
        Some(if prev <= 0.0 && now > 0.0 {
            1
        } else if prev >= 0.0 && now < 0.0 {
            -1
        } else {
            0
        })
    }

    /// Whether `value(p)` will return a value.
    #[inline]
    pub fn ready(&self, p: usize) -> bool {
        p >= 1 && p <= self.max_period && self.bars >= p as u64
    }

    /// Closed bars accumulated so far (not capped at `max_period`).
    #[inline]
    pub fn bars(&self) -> u64 {
        self.bars
    }

    /// Largest period this series can answer.
    #[inline]
    pub fn max_period(&self) -> usize {
        self.max_period
    }

    /// Bar length in milliseconds, as declared at construction.
    #[inline]
    pub fn interval_ms(&self) -> u32 {
        self.interval_ms
    }

    /// Close of the most recent completed bar.
    #[inline]
    pub fn last_close(&self) -> Option<f64> {
        if self.bars == 0 {
            return None;
        }
        let len = self.closes.len() as u64;
        Some(self.closes[(self.bars % len) as usize])
    }

    /// Recompute the prefix ring from the close ring with the oldest in-window
    /// sum reset to zero. See the module docs — this is what keeps `S[n]` from
    /// growing until `S[n] - S[n-p]` stops being accurate.
    fn rebase(&mut self) {
        let len = self.closes.len() as u64;
        let window = self.bars.min(self.max_period as u64);
        let start = self.bars - window;
        self.prefix[(start % len) as usize] = 0.0;
        for k in (start + 1)..=self.bars {
            let slot = (k % len) as usize;
            let prev = ((k - 1) % len) as usize;
            self.prefix[slot] = self.prefix[prev] + self.closes[slot];
        }
        self.since_rebase = 0;
        debug_assert!(self.matches_direct_sum(), "prefix ring diverged from direct summation");
    }

    /// Debug-only: every ready period agrees with summing the close ring directly.
    #[cfg(debug_assertions)]
    fn matches_direct_sum(&self) -> bool {
        let len = self.closes.len() as u64;
        for p in 1..=self.max_period {
            if !self.ready(p) {
                break;
            }
            let mut sum = 0.0;
            for k in (self.bars - p as u64 + 1)..=self.bars {
                sum += self.closes[(k % len) as usize];
            }
            let direct = sum / p as f64;
            let got = self.value(p).unwrap();
            if (got - direct).abs() > direct.abs() * 1e-9 + 1e-9 {
                return false;
            }
        }
        true
    }

    #[cfg(not(debug_assertions))]
    fn matches_direct_sum(&self) -> bool {
        true
    }
}

/// A configured series plus the builder feeding it — everything an engine needs
/// to offer `ctx.ma`.
///
/// Both the live runner and the backtest own one of these per symbol and drive
/// it with the same calls, which is what makes "the same window produces the
/// same MA in backtest and live" a structural property rather than a hope.
#[derive(Debug, Clone)]
pub struct MaFeed {
    config: MaConfig,
    series: MaSeries,
    candles: CandleBuilder,
}

impl MaFeed {
    /// Build from strategy params. `None` when `maPeriod` is 0 or absent, which
    /// is the disabled path: no series, no feeding, no behaviour change.
    pub fn from_params(params: &Params) -> Option<Self> {
        Some(Self::from_config(MaConfig::from_params(params)?))
    }

    /// Build from an already-resolved config — the sweep/shadow path, where the
    /// config covers a whole trial grid rather than one strategy's params.
    pub fn from_config(config: MaConfig) -> Self {
        Self { config, series: config.series(), candles: config.builder() }
    }

    /// Warm the series from historical closes, oldest first (live startup:
    /// `fetch_klines` before the first tick). Without this, live would spend the
    /// first `maPeriod` bars unable to trade.
    pub fn seed(&mut self, closes: &[f64]) {
        self.series.seed(closes);
    }

    /// Advance the bars from a trade print — the default, local candle source.
    /// No-op under `maSource=1`.
    #[inline]
    pub fn on_trade(&mut self, trade: &TradeEvent) {
        if !self.config.from_klines {
            self.candles.feed(trade.timestamp_ms, trade.price, &mut self.series);
        }
    }

    /// Advance the bars from a venue kline (`maSource=1`). No-op otherwise, and
    /// unclosed klines are ignored.
    #[inline]
    pub fn on_kline(&mut self, close: f64, closed: bool) {
        if self.config.from_klines && closed {
            self.series.push_close(close);
        }
    }

    /// Whether enough bars exist for the engine to allow entries.
    #[inline]
    pub fn warm(&self) -> bool {
        self.series.bars() >= self.config.warmup_bars as u64
    }

    /// The series strategies read through `ctx.ma`.
    #[inline]
    pub fn series(&self) -> &MaSeries {
        &self.series
    }

    #[inline]
    pub fn config(&self) -> &MaConfig {
        &self.config
    }
}

/// `can_enter` contribution of an optional feed: no MA configured means no
/// opinion; a configured one blocks entries until it is warm.
#[inline]
pub fn ma_allows_entry(feed: Option<&MaFeed>) -> bool {
    feed.is_none_or(|f| f.warm())
}

/// Bars closed by a single [`CandleBuilder`] push.
///
/// `count > 1` means intervals passed with no trades in them; they are reported
/// as flat bars at `close` so a period keeps meaning the same span of time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClosedBars {
    pub close: f64,
    pub count: u32,
}

/// Turns a trade stream into fixed-interval bar closes.
///
/// This is the **only** place that decides where a bar boundary falls, so the
/// live runner and the backtest cannot drift: both feed it the same trades and
/// get the same bars. That is the whole reason the default candle source is
/// local aggregation rather than exchange klines — the trade segment is present
/// in every prepared file, whereas the kline segment can legitimately be empty.
///
/// Locally-built bars differ from a venue's own klines by the
/// first-trade-of-interval boundary and by any trades the venue counted that we
/// did not see; expect a few bps of divergence from a charting-tool MA. A
/// strategy tuned against a chart should use the exchange-kline source instead.
#[derive(Debug, Clone)]
pub struct CandleBuilder {
    interval_ms: u64,
    /// `timestamp / interval_ms` of the bar currently forming.
    bucket: u64,
    close: f64,
    started: bool,
    max_gap_fill: u32,
}

impl CandleBuilder {
    /// `interval_ms` is clamped to at least 1ms.
    pub fn new(interval_ms: u32) -> Self {
        Self {
            interval_ms: (interval_ms as u64).max(1),
            bucket: 0,
            close: 0.0,
            started: false,
            max_gap_fill: MA_MAX_PERIOD as u32 + 1,
        }
    }

    /// Feed one price. Returns the bars that just closed, if any.
    ///
    /// Out-of-order timestamps (a trade older than the forming bar) are ignored
    /// rather than rewinding the bar clock.
    #[inline]
    pub fn push(&mut self, timestamp_ms: u64, price: f64) -> Option<ClosedBars> {
        let bucket = timestamp_ms / self.interval_ms;
        if !self.started {
            self.started = true;
            self.bucket = bucket;
            self.close = price;
            return None;
        }
        if bucket <= self.bucket {
            if bucket == self.bucket {
                self.close = price;
            }
            return None;
        }
        // The forming bar closes, plus one flat bar per empty interval between.
        let count = (bucket - self.bucket).min(self.max_gap_fill as u64) as u32;
        let closed = ClosedBars { close: self.close, count };
        self.bucket = bucket;
        self.close = price;
        Some(closed)
    }

    /// [`push`](Self::push) for a trade event.
    #[inline]
    pub fn push_trade(&mut self, trade: &TradeEvent) -> Option<ClosedBars> {
        self.push(trade.timestamp_ms, trade.price)
    }

    /// Push a trade straight into a series, closing bars and reporting the
    /// forming price in one call. The common wiring.
    #[inline]
    pub fn feed(&mut self, timestamp_ms: u64, price: f64, ma: &mut MaSeries) {
        if let Some(c) = self.push(timestamp_ms, price) {
            ma.push_close_repeat(c.close, c.count);
        }
        ma.set_forming(price);
    }

    /// Price of the bar currently forming.
    #[inline]
    pub fn forming(&self) -> Option<f64> {
        self.started.then_some(self.close)
    }

    /// Bar length in milliseconds.
    #[inline]
    pub fn interval_ms(&self) -> u64 {
        self.interval_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn direct_ma(closes: &[f64], p: usize) -> f64 {
        let tail = &closes[closes.len() - p..];
        tail.iter().sum::<f64>() / p as f64
    }

    #[test]
    fn value_matches_direct_summation_for_every_period() {
        // Long random walk at a realistic price scale, so the run crosses many
        // re-baseline boundaries — the case where an unbounded prefix sum would
        // start losing bits.
        let mut ma = MaSeries::new(200, 60_000);
        let mut closes = Vec::new();
        let mut price = 40_000.0_f64;
        let mut state = 12_345_u64;
        for _ in 0..10_000 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let step = ((state >> 33) as f64 / (1u64 << 31) as f64 - 0.5) * 20.0;
            price = (price + step).max(1.0);
            closes.push(price);
            ma.push_close(price);
        }
        for p in 10..=200 {
            let want = direct_ma(&closes, p);
            let got = ma.value(p).unwrap();
            assert!(
                (got - want).abs() / want < 1e-12,
                "p={p}: got {got}, want {want}"
            );
        }
    }

    #[test]
    fn stays_exact_across_a_rebase_boundary() {
        // Step onto, and one past, the exact push that triggers a re-baseline.
        let cap = 32;
        let mut ma = MaSeries::new(cap, 60_000);
        let mut closes = Vec::new();
        for i in 0..(cap * 3 + 1) {
            let c = 1_000_000.0 + i as f64;
            closes.push(c);
            ma.push_close(c);
            for p in 1..=cap.min(closes.len()) {
                let want = direct_ma(&closes, p);
                let got = ma.value(p).unwrap();
                assert!((got - want).abs() < 1e-9, "bar {i}, p={p}: {got} != {want}");
            }
        }
    }

    #[test]
    fn value_is_none_for_exactly_the_first_p_bars() {
        let mut ma = MaSeries::new(10, 60_000);
        for n in 1..=10 {
            ma.push_close(n as f64);
            for p in 1..=10 {
                assert_eq!(ma.value(p).is_some(), p <= n, "n={n} p={p}");
            }
        }
        // Above capacity is never ready, however many bars arrive.
        assert!(ma.value(11).is_none());
    }

    #[test]
    fn seed_makes_every_period_ready_immediately() {
        let mut ma = MaSeries::new(50, 60_000);
        let closes: Vec<f64> = (1..=200).map(|i| i as f64).collect();
        ma.seed(&closes);
        assert_eq!(ma.bars(), 50);
        for p in 1..=50 {
            assert!(ma.ready(p));
            assert!((ma.value(p).unwrap() - direct_ma(&closes, p)).abs() < 1e-9);
        }
    }

    #[test]
    fn seed_accepts_fewer_closes_than_capacity() {
        let mut ma = MaSeries::new(50, 60_000);
        ma.seed(&[1.0, 2.0, 3.0]);
        assert_eq!(ma.bars(), 3);
        assert_eq!(ma.value(3), Some(2.0));
        assert!(ma.value(4).is_none());
    }

    #[test]
    fn value_at_and_slope_read_history() {
        let mut ma = MaSeries::new(20, 60_000);
        for i in 1..=20 {
            ma.push_close(i as f64);
        }
        // MA(5) now = mean(16..20) = 18; five bars ago = mean(11..15) = 13.
        assert_eq!(ma.value(5), Some(18.0));
        assert_eq!(ma.value_at(5, 5), Some(13.0));
        assert_eq!(ma.slope(5, 5), Some(5.0));
        assert_eq!(ma.slope(5, 0), Some(0.0));
        // p + back must fit in capacity.
        assert!(ma.value_at(20, 1).is_none());
    }

    #[test]
    fn cross_reports_the_bar_it_happened_on() {
        let mut ma = MaSeries::new(10, 60_000);
        // Falling then sharply rising: fast MA must cut up through slow.
        for c in [10.0, 9.0, 8.0, 7.0, 6.0, 5.0] {
            ma.push_close(c);
        }
        assert_eq!(ma.cross(2, 5), Some(0));
        let mut crossed_up = 0;
        for c in [20.0, 30.0, 40.0] {
            ma.push_close(c);
            if ma.cross(2, 5) == Some(1) {
                crossed_up += 1;
            }
        }
        assert_eq!(crossed_up, 1, "the cross is reported on exactly one bar");
    }

    #[test]
    fn live_value_includes_the_forming_bar() {
        let mut ma = MaSeries::new(10, 60_000);
        for c in [1.0, 2.0, 3.0] {
            ma.push_close(c);
        }
        // No forming price yet → live == closed.
        assert_eq!(ma.value_live(3), ma.value(3));
        ma.set_forming(10.0);
        // Two closed bars (2, 3) plus the forming 10.
        assert_eq!(ma.value_live(3), Some(5.0));
        assert_eq!(ma.value(3), Some(2.0), "closed reader is unaffected");
        // Closing a bar clears the forming price.
        ma.push_close(4.0);
        assert_eq!(ma.value_live(3), ma.value(3));
    }

    #[test]
    fn live_value_is_ready_one_bar_before_closed() {
        let mut ma = MaSeries::new(10, 60_000);
        ma.push_close(1.0);
        ma.push_close(2.0);
        ma.set_forming(3.0);
        assert_eq!(ma.value_live(3), Some(2.0));
        assert!(ma.value(3).is_none());
    }

    #[test]
    fn reset_clears_state_but_keeps_capacity() {
        let mut ma = MaSeries::new(10, 60_000);
        for i in 1..=10 {
            ma.push_close(i as f64);
        }
        ma.reset();
        assert_eq!(ma.bars(), 0);
        assert!(ma.value(1).is_none());
        assert_eq!(ma.max_period(), 10);
        ma.push_close(7.0);
        assert_eq!(ma.value(1), Some(7.0));
    }

    #[test]
    fn capacity_is_clamped_to_the_hard_ceiling() {
        assert_eq!(MaSeries::new(0, 1).max_period(), 1);
        assert_eq!(MaSeries::new(MA_MAX_PERIOD * 4, 1).max_period(), MA_MAX_PERIOD);
    }

    #[test]
    fn config_is_absent_unless_a_period_is_set() {
        assert_eq!(MaConfig::from_params(&Params::new()), None);
        assert_eq!(MaConfig::from_params(&Params::new().set("maPeriod", 0.0)), None);
        assert_eq!(MaConfig::from_params(&Params::new().set("maPeriod", 0.4)), None);
        assert!(MaConfig::from_params(&Params::new().set("maPeriod", 1.0)).is_some());
    }

    #[test]
    fn config_defaults_capacity_and_warmup_to_the_period() {
        let c = MaConfig::from_params(&Params::new().set("maPeriod", 50.0)).unwrap();
        assert_eq!(c.max_period, 50);
        assert_eq!(c.warmup_bars, 50);
        assert_eq!(c.interval_ms, 60_000);
        assert!(!c.from_klines);
    }

    #[test]
    fn config_capacity_covers_the_slope_lookback() {
        // Sizing capacity to `maPeriod` alone would make `slope()` return None
        // forever — the strategy would hold for the whole run, silently.
        let params = Params::new().set("maPeriod", 50.0).set("maSlopeBars", 5.0);
        let c = MaConfig::from_params(&params).unwrap();
        assert_eq!(c.max_period, 55);
        let mut series = c.series();
        for i in 0..60 {
            series.push_close(100.0 + i as f64);
        }
        assert!(series.slope(50, 5).is_some(), "the configured slope must be answerable");
    }

    #[test]
    fn config_capacity_covers_a_sweep_grid() {
        // A sweep sets maMaxPeriod to the grid maximum, so every trial shares
        // one series and — crucially — the same warmup, whatever its own period.
        let params = Params::new().set("maPeriod", 10.0).set("maMaxPeriod", 200.0);
        let c = MaConfig::from_params(&params).unwrap();
        assert_eq!(c.max_period, 200);
        assert_eq!(c.warmup_bars, 200);
        // A capacity below the period is raised to it rather than truncating.
        let params = Params::new().set("maPeriod", 90.0).set("maMaxPeriod", 10.0);
        assert_eq!(MaConfig::from_params(&params).unwrap().max_period, 90);
    }

    #[test]
    fn config_reads_interval_source_and_warmup() {
        let params = Params::new()
            .set("maPeriod", 20.0)
            .set("maIntervalSec", 300.0)
            .set("maSource", 1.0)
            .set("maWarmupBars", 0.0);
        let c = MaConfig::from_params(&params).unwrap();
        assert_eq!(c.interval_ms, 300_000);
        assert!(c.from_klines);
        assert_eq!(c.warmup_bars, 0);
        assert_eq!(c.series().max_period(), 20);
        assert_eq!(c.builder().interval_ms(), 300_000);
    }

    #[test]
    fn merge_grid_covers_every_trial() {
        let grid = vec![
            Params::new(),
            Params::new().set("maPeriod", 10.0).set("maIntervalSec", 300.0),
            Params::new().set("maPeriod", 200.0),
        ];
        let c = MaConfig::merge_grid(&grid).unwrap();
        assert_eq!(c.max_period, 200, "the series must answer the largest period");
        assert_eq!(c.warmup_bars, 200, "one start bar for the whole grid");
        assert_eq!(c.interval_ms, 300_000, "bar length from the first configured trial");
        assert_eq!(MaConfig::merge_grid(&[Params::new()]), None);
    }

    #[test]
    fn interval_label_covers_seedable_intervals() {
        let label = |sec: f64| {
            MaConfig::from_params(&Params::new().set("maPeriod", 5.0).set("maIntervalSec", sec))
                .unwrap()
                .interval_label()
        };
        assert_eq!(label(60.0), Some("1m"));
        assert_eq!(label(3600.0), Some("1h"));
        assert_eq!(label(86_400.0), Some("1d"));
        assert_eq!(label(47.0), None, "no venue offers a 47s candle");
    }

    #[test]
    fn config_clamps_capacity_to_the_hard_ceiling() {
        let params = Params::new().set("maPeriod", 5_000.0);
        assert_eq!(MaConfig::from_params(&params).unwrap().max_period, MA_MAX_PERIOD);
    }

    #[test]
    fn builder_closes_a_bar_per_interval() {
        let mut b = CandleBuilder::new(60_000);
        assert_eq!(b.push(0, 100.0), None);
        assert_eq!(b.push(30_000, 101.0), None);
        // Crossing into the next minute closes the first bar at its last price.
        assert_eq!(b.push(60_000, 102.0), Some(ClosedBars { close: 101.0, count: 1 }));
        assert_eq!(b.forming(), Some(102.0));
    }

    #[test]
    fn builder_fills_empty_intervals_with_flat_bars() {
        let mut b = CandleBuilder::new(60_000);
        b.push(0, 100.0);
        // Jump four minutes: the first bar closes plus three silent ones.
        assert_eq!(
            b.push(4 * 60_000, 200.0),
            Some(ClosedBars { close: 100.0, count: 4 })
        );
    }

    #[test]
    fn builder_ignores_out_of_order_trades() {
        let mut b = CandleBuilder::new(60_000);
        b.push(120_000, 100.0);
        assert_eq!(b.push(60_000, 999.0), None);
        assert_eq!(b.forming(), Some(100.0), "the bar clock did not rewind");
    }

    #[test]
    fn builder_gap_fill_is_bounded() {
        let mut b = CandleBuilder::new(1);
        b.push(0, 100.0);
        let closed = b.push(u64::MAX / 2, 200.0).unwrap();
        assert_eq!(closed.count, MA_MAX_PERIOD as u32 + 1);
    }

    #[test]
    fn feed_wires_builder_to_series() {
        let mut b = CandleBuilder::new(60_000);
        let mut ma = MaSeries::new(10, 60_000);
        for min in 0..5 {
            b.feed(min * 60_000, 10.0 + min as f64, &mut ma);
        }
        // Bars 0..3 closed at 10, 11, 12, 13; minute 4 is still forming at 14.
        assert_eq!(ma.bars(), 4);
        assert_eq!(ma.value(4), Some(11.5));
        assert_eq!(ma.value_live(5), Some(12.0));
    }

    #[test]
    fn builder_bars_match_an_independent_bucketing() {
        // The parity guarantee rests on the builder being the only thing that
        // decides bar boundaries, so check it against a straightforward
        // last-price-per-bucket grouping rather than against itself.
        let trades: Vec<TradeEvent> = (0..500)
            .map(|i| TradeEvent {
                price: 100.0 + (i % 37) as f64,
                quantity: 1.0,
                timestamp_ms: i * 7_000,
                is_buyer_maker: false,
            })
            .collect();

        let interval = 60_000_u64;
        let mut want: Vec<f64> = Vec::new();
        let mut bucket = trades[0].timestamp_ms / interval;
        let mut close = trades[0].price;
        for t in &trades[1..] {
            let b = t.timestamp_ms / interval;
            if b > bucket {
                // The forming bar closes, then one flat bar per silent interval.
                for _ in 0..(b - bucket) {
                    want.push(close);
                }
                bucket = b;
            }
            close = t.price;
        }

        let mut builder = CandleBuilder::new(interval as u32);
        let mut ma = MaSeries::new(50, interval as u32);
        for t in &trades {
            builder.feed(t.timestamp_ms, t.price, &mut ma);
        }

        assert_eq!(ma.bars(), want.len() as u64);
        assert_eq!(ma.last_close(), want.last().copied());
        for p in 1..=50 {
            let expect = want[want.len() - p..].iter().sum::<f64>() / p as f64;
            assert!((ma.value(p).unwrap() - expect).abs() < 1e-9, "p={p}");
        }
    }
}
