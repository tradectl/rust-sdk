use std::collections::VecDeque;

/// Per-symbol volume profile computed by the runner/backtest engine.
///
/// Provides relative volume metrics: how current volume compares to a
/// rolling baseline, plus buy/sell ratio from the trade stream.
/// Strategies use this for adaptive threshold detection (e.g., trigger
/// when volume is 3x normal instead of a fixed USDT amount).
///
/// Architecture matches upstream's Worker.c → CoinToStrategyTransfer pattern:
/// the runner computes this per-symbol and passes it via StrategyContext.
#[derive(Debug, Clone, Copy, Default)]
pub struct VolumeProfile {
    /// Current window volume / baseline (1.0 = normal, 3.0 = 3x spike).
    /// Capped at 1000.0.
    pub ratio: f64,
    /// Average quote volume per minute from baseline window.
    pub baseline_per_min: f64,
    /// Current quote volume per minute (from detection window).
    pub current_per_min: f64,
    /// Buy-side ratio in current window (0.0 = all sells, 1.0 = all buys).
    /// Derived from is_buyer_maker field on trades.
    pub buy_ratio: f64,
    /// False during warmup when insufficient data for baseline.
    pub baseline_ready: bool,
}

/// Per-symbol volume baseline tracker.
///
/// Accumulates per-minute quote volume in a circular buffer, computes a
/// rolling baseline (avg vol/min), and tracks short-window volume with
/// buy/sell split for the current ratio.
///
/// TODO: move to runner-level (per-symbol shared infrastructure).
/// Currently usable from both backtest and live runner.
pub struct VolumeTracker {
    // -- Baseline (long-term, per-minute buckets) --
    minutes: VecDeque<f64>,
    current_minute_vol: f64,
    current_minute_ms: u64,
    max_minutes: usize,
    skip_recent: usize,

    // -- Short window (for current ratio + buy/sell) --
    window: VecDeque<TradeEntry>,
    window_vol: f64,
    window_buy_vol: f64,
    window_ms: u64,
}

#[derive(Debug, Clone, Copy)]
struct TradeEntry {
    timestamp_ms: u64,
    quote_volume: f64,
    is_sell: bool,
}

impl VolumeTracker {
    /// Create a new tracker.
    ///
    /// - `window_ms`: short window for current ratio (e.g., 5000 = 5s)
    /// - `max_minutes`: baseline history length (e.g., 180 = 3 hours)
    /// - `skip_recent`: minutes to exclude from baseline (e.g., 60 = 1 hour)
    pub fn new(window_ms: u64, max_minutes: usize, skip_recent: usize) -> Self {
        Self {
            minutes: VecDeque::with_capacity(max_minutes),
            current_minute_vol: 0.0,
            current_minute_ms: 0,
            max_minutes,
            skip_recent,
            window: VecDeque::with_capacity(512),
            window_vol: 0.0,
            window_buy_vol: 0.0,
            window_ms,
        }
    }

    /// Feed a trade into the tracker.
    pub fn push(&mut self, timestamp_ms: u64, price: f64, quantity: f64, is_buyer_maker: bool) {
        let qv = price * quantity;

        // -- Update per-minute baseline --
        let minute_boundary = timestamp_ms / 60_000;
        if self.current_minute_ms == 0 {
            self.current_minute_ms = minute_boundary;
        }
        if minute_boundary != self.current_minute_ms {
            // Rotate: push completed minute, start new one
            self.minutes.push_back(self.current_minute_vol);
            if self.minutes.len() > self.max_minutes {
                self.minutes.pop_front();
            }
            // Handle gaps (minutes with no trades)
            let gap = (minute_boundary - self.current_minute_ms).min(self.max_minutes as u64);
            for _ in 1..gap {
                self.minutes.push_back(0.0);
                if self.minutes.len() > self.max_minutes {
                    self.minutes.pop_front();
                }
            }
            self.current_minute_vol = 0.0;
            self.current_minute_ms = minute_boundary;
        }
        self.current_minute_vol += qv;

        // -- Update short window --
        self.window_vol += qv;
        if !is_buyer_maker {
            self.window_buy_vol += qv;
        }
        self.window.push_back(TradeEntry {
            timestamp_ms,
            quote_volume: qv,
            is_sell: is_buyer_maker,
        });
        self.expire_window(timestamp_ms);
    }

    fn expire_window(&mut self, now_ms: u64) {
        let cutoff = now_ms.saturating_sub(self.window_ms);
        while let Some(front) = self.window.front() {
            if front.timestamp_ms < cutoff {
                self.window_vol -= front.quote_volume;
                if !front.is_sell {
                    self.window_buy_vol -= front.quote_volume;
                }
                self.window.pop_front();
            } else {
                break;
            }
        }
        if self.window_vol < 0.0 {
            self.window_vol = 0.0;
        }
        if self.window_buy_vol < 0.0 {
            self.window_buy_vol = 0.0;
        }
    }

    /// Compute the baseline: average quote volume per minute,
    /// excluding the most recent `skip_recent` minutes.
    pub fn baseline_per_min(&self) -> f64 {
        let total = self.minutes.len();
        if total <= self.skip_recent {
            return 0.0;
        }
        let usable = total - self.skip_recent;
        let sum: f64 = self.minutes.iter().take(usable).sum();
        if usable > 0 {
            sum / usable as f64
        } else {
            0.0
        }
    }

    /// Whether enough data exists for a meaningful baseline.
    pub fn baseline_ready(&self) -> bool {
        self.minutes.len() > self.skip_recent
    }

    /// Current volume profile snapshot.
    pub fn profile(&self) -> VolumeProfile {
        let baseline = self.baseline_per_min();
        let baseline_ready = self.baseline_ready();

        // Scale baseline to match the detection window duration
        let baseline_for_window = if self.window_ms > 0 && baseline > 0.0 {
            baseline * self.window_ms as f64 / 60_000.0
        } else {
            0.0
        };

        let ratio = if baseline_for_window > 0.0 && baseline_ready {
            (self.window_vol / baseline_for_window).min(1000.0)
        } else {
            0.0
        };

        let current_per_min = if self.window_ms > 0 {
            self.window_vol * 60_000.0 / self.window_ms as f64
        } else {
            0.0
        };

        let buy_ratio = if self.window_vol > 0.0 {
            self.window_buy_vol / self.window_vol
        } else {
            0.5
        };

        VolumeProfile {
            ratio,
            baseline_per_min: baseline,
            current_per_min,
            buy_ratio,
            baseline_ready,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_accumulates_minutes() {
        let mut vt = VolumeTracker::new(5000, 180, 0); // no skip for test
        // Simulate 5 minutes of trades at $1000/min
        for min in 0..5 {
            let ts = (min + 1) * 60_000;
            vt.push(ts, 100.0, 10.0, false); // $1000 per minute
        }
        assert_eq!(vt.minutes.len(), 4); // 4 completed minutes (5th is current)
        assert!((vt.baseline_per_min() - 1000.0).abs() < 1.0);
    }

    #[test]
    fn baseline_skips_recent() {
        let mut vt = VolumeTracker::new(5000, 180, 2); // skip 2 recent
        for min in 0..5 {
            let ts = (min + 1) * 60_000;
            vt.push(ts, 100.0, 10.0, false);
        }
        // 4 completed minutes, skip 2 recent → use first 2
        assert_eq!(vt.minutes.len(), 4);
        assert!((vt.baseline_per_min() - 1000.0).abs() < 1.0);
    }

    #[test]
    fn baseline_not_ready_during_warmup() {
        let mut vt = VolumeTracker::new(5000, 180, 60);
        // Only 10 minutes of data, need >60 for baseline
        for min in 0..10 {
            vt.push((min + 1) * 60_000, 100.0, 10.0, false);
        }
        assert!(!vt.baseline_ready());
        assert!((vt.baseline_per_min()).abs() < 0.01);
    }

    #[test]
    fn ratio_detects_spike() {
        let mut vt = VolumeTracker::new(5000, 180, 0);
        // Build 10 minutes of baseline at $1000/min
        for min in 0..10 {
            vt.push((min + 1) * 60_000, 100.0, 10.0, false);
        }
        // Now spike: $5000 in 5 seconds (= $60K/min vs $1K/min baseline)
        let spike_ts = 11 * 60_000;
        vt.push(spike_ts, 100.0, 50.0, false);
        let p = vt.profile();
        assert!(p.ratio > 10.0); // way above baseline
        assert!(p.baseline_ready);
    }

    #[test]
    fn buy_ratio_tracks_direction() {
        let mut vt = VolumeTracker::new(5000, 180, 0);
        let ts = 60_000;
        vt.push(ts, 100.0, 7.0, false);  // buy: $700
        vt.push(ts + 1, 100.0, 3.0, true); // sell: $300
        let p = vt.profile();
        assert!((p.buy_ratio - 0.7).abs() < 0.01);
    }

    #[test]
    fn window_expires_old_trades() {
        let mut vt = VolumeTracker::new(3000, 180, 0); // 3s window
        vt.push(1000, 100.0, 10.0, false);  // $1000 at t=1s
        vt.push(2000, 100.0, 5.0, false);   // $500 at t=2s
        vt.push(5000, 100.0, 1.0, false);   // $100 at t=5s, expires t=1s
        let p = vt.profile();
        assert!((p.current_per_min - (600.0 * 60.0 / 3.0)).abs() < 100.0);
    }

    #[test]
    fn ratio_capped_at_1000() {
        let mut vt = VolumeTracker::new(5000, 180, 0);
        // Tiny baseline
        for min in 0..5 {
            vt.push((min + 1) * 60_000, 100.0, 0.001, false); // $0.1/min
        }
        // Massive spike
        vt.push(6 * 60_000, 100.0, 1000.0, false);
        let p = vt.profile();
        assert!((p.ratio - 1000.0).abs() < 0.01);
    }
}
