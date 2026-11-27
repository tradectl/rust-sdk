//! Reusable SoA batch exchange engine for shadow/sweep execution.
//!
//! Manages positions, fills, TP/SL, PnL calculation, and metrics for N trials
//! with up to `max_positions` concurrent positions each. Strategy implementations
//! compose this: they own a `BatchExchange` and set entry prices via
//! `process_ticker()`. Everything else — fill scheduling, exit checks, metrics —
//! is handled here.
//!
//! The rules are the scalar engine's (`tradectl-backtest` `InMemoryExchange`),
//! so a sweep and a backtest of the same params give the same trades and PnL:
//! entries sized by the strategy's order size, an amend keeps the old price on
//! the book until it lands, exits armed after the entry's REST round trip, a
//! stop priced at the worst of the book side and the trade print, and flat
//! trades counted as neither win nor loss. `backtest/tests/batch_parity.rs`
//! holds the two engines to it.

use crate::types::{IndicatorValue, TradeEvent, TickerEvent, MarketType};
use super::batch::{BatchConfig, BatchResult, BatchDiagnostics};

/// SoA batch exchange engine. Owns all position state, fill logic, and metrics.
///
/// Strategies access entry/position arrays directly for hot-loop performance.
pub struct BatchExchange {
    pub n: usize,
    pub max_positions: usize,
    pub initial_balance: f64,
    maker_fee: f64,
    taker_fee: f64,
    slippage_pct: f64,
    pub leverage: f64,
    pub margin_per_pos: f64,
    pub latency_ms: u64,
    pub market_type: MarketType,
    pub contract_size: f64,
    /// Limit price tick (0 = none). See [`round_price`](Self::round_price).
    pub price_step: f64,

    // ── Indicators: ONE set for all N trials ─────────────────────────
    /// Readings for the requests this batch declared, in declaration order.
    ///
    /// An indicator is a function of market data only — never of trial
    /// parameters — so every trial reads the same values and the cost is O(1)
    /// in trial count rather than O(N). The driver owns the instances (they
    /// live in `tradectl-indicators`, which this crate cannot depend on) and
    /// pushes the readings in through [`set_indicators`](Self::set_indicators)
    /// before each pass. Read as `self.exchange.indicator(i)` in the per-trial
    /// loop.
    ///
    /// Empty when the batch declared none.
    pub indicators: Vec<IndicatorValue>,

    // ── Book: ONE for all N trials ───────────────────────────────────
    /// Last book ticker, published by the driver ([`update_book`](Self::update_book)).
    /// Stop exits are priced against it. 0 = no book yet.
    last_bid: f64,
    last_ask: f64,

    // ── Per-trial sizing and exit timing (set by the strategy) ───────
    /// Order size per entry, as the strategy's `orderSize` (USD for linear,
    /// base asset for inverse). NaN (the default) = not set: size by
    /// `margin_per_pos * leverage`.
    pub order_size: Vec<f64>,
    /// Stop activation delay after the entry fill, per trial.
    pub sl_delay: Vec<u64>,
    /// The strategy waits on a request until then (the runner's
    /// `busy_until`): ticks are not handled, and a fill is not delivered, so
    /// its exits are not placed before it.
    pub busy_until: Vec<u64>,
    /// Positions whose fill the strategy has not been told yet, per trial.
    unarmed: Vec<u8>,
    unarmed_total: usize,

    // ── Pending entry state (per trial) ──────────────────────────────
    /// Latest entry price the strategy asked for (rounded). 0.0 = no pending
    /// entry. While an amend is in flight the book still holds the old price.
    pub entry_price: Vec<f64>,
    /// TP/SL for `entry_price`, applied if that price fills.
    pending_tp_price: Vec<f64>,
    pending_sl_price: Vec<f64>,
    /// Pending entry side: true = short, false = long.
    entry_is_short: Vec<bool>,
    /// When `entry_price` reaches the book (placement or amend round trip).
    /// 0 = it is there already.
    pub entry_active_at: Vec<u64>,
    /// The order on the book now (0.0 = none yet), with its own TP/SL.
    live_price: Vec<f64>,
    live_tp_price: Vec<f64>,
    live_sl_price: Vec<f64>,

    // ── Position state: n * max_positions elements ───────────────────
    pub pos_active: Vec<u8>,
    pos_entry_price: Vec<f64>,
    pos_quantity: Vec<f64>,
    pos_tp_price: Vec<f64>,
    pos_sl_price: Vec<f64>,
    pos_tp_active_at: Vec<u64>,
    pos_sl_active_at: Vec<u64>,
    /// Position side: true = short, false = long.
    pos_is_short: Vec<bool>,

    // ── Per-trial counters ───────────────────────────────────────────
    pub active_count: Vec<u8>,
    pub balance: Vec<f64>,

    // ── Metrics accumulators (per trial) ─────────────────────────────
    trade_count: Vec<u32>,
    winning_trades: Vec<u32>,
    losing_trades: Vec<u32>,
    total_pnl: Vec<f64>,
    sum_wins: Vec<f64>,
    sum_losses: Vec<f64>,
    sum_returns: Vec<f64>,
    sum_returns_sq: Vec<f64>,
    sum_neg_returns_sq: Vec<f64>,
    peak_equity: Vec<f64>,
    max_drawdown: Vec<f64>,
    max_drawdown_abs: Vec<f64>,
    /// Timestamp of first completed trade per trial (for burst detection).
    first_trade_at: Vec<u64>,
    /// Timestamp of most recent completed trade per trial.
    last_trade_at: Vec<u64>,

    // ── Fill counters (for position tracking) ────────────────────────
    total_entries: u32,
    total_exits: u32,

    // ── Latency jitter ───────────────────────────────────────────────
    jitter_ms: u64,
    /// Simple fast RNG state for jitter (xorshift32).
    rng_state: u32,
}

impl BatchExchange {
    pub fn new(n: usize, config: &BatchConfig, max_positions: usize) -> Self {
        let total_slots = n * max_positions;
        let margin_per_pos = config.initial_balance / max_positions as f64;

        Self {
            n,
            max_positions,
            initial_balance: config.initial_balance,
            maker_fee: config.maker_fee,
            taker_fee: config.taker_fee,
            slippage_pct: config.slippage_pct,
            leverage: config.leverage,
            margin_per_pos,
            latency_ms: config.latency_ms,
            market_type: config.market_type,
            contract_size: config.contract_size,
            price_step: config.price_step,
            indicators: Vec::new(),
            last_bid: 0.0,
            last_ask: 0.0,
            order_size: vec![f64::NAN; n],
            sl_delay: vec![config.sl_delay_ms; n],
            busy_until: vec![0; n],
            unarmed: vec![0; n],
            unarmed_total: 0,
            entry_price: vec![0.0; n],
            pending_tp_price: vec![0.0; n],
            pending_sl_price: vec![0.0; n],
            entry_is_short: vec![false; n],
            entry_active_at: vec![0; n],
            live_price: vec![0.0; n],
            live_tp_price: vec![0.0; n],
            live_sl_price: vec![0.0; n],
            pos_active: vec![0; total_slots],
            pos_entry_price: vec![0.0; total_slots],
            pos_quantity: vec![0.0; total_slots],
            pos_tp_price: vec![0.0; total_slots],
            pos_sl_price: vec![0.0; total_slots],
            pos_tp_active_at: vec![0; total_slots],
            pos_sl_active_at: vec![0; total_slots],
            pos_is_short: vec![false; total_slots],
            active_count: vec![0; n],
            balance: vec![config.initial_balance; n],
            trade_count: vec![0; n],
            winning_trades: vec![0; n],
            losing_trades: vec![0; n],
            total_pnl: vec![0.0; n],
            sum_wins: vec![0.0; n],
            sum_losses: vec![0.0; n],
            sum_returns: vec![0.0; n],
            sum_returns_sq: vec![0.0; n],
            sum_neg_returns_sq: vec![0.0; n],
            peak_equity: vec![config.initial_balance; n],
            max_drawdown: vec![0.0; n],
            max_drawdown_abs: vec![0.0; n],
            first_trade_at: vec![0; n],
            last_trade_at: vec![0; n],
            total_entries: 0,
            total_exits: 0,
            jitter_ms: config.jitter_ms,
            rng_state: 0xDEAD_BEEF,
        }
    }

    /// Fast xorshift32 RNG for latency jitter. Returns value in `[0, max)`.
    #[inline(always)]
    fn rand_u64(&mut self, max: u64) -> u64 {
        if max == 0 { return 0; }
        let mut x = self.rng_state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng_state = x;
        (x as u64) % max
    }

    /// Publish the latest book ticker. The driver calls this before
    /// `process_ticker`, like the scalar engine's `update_book`.
    /// Also delivers fills that waited for a busy window to end.
    #[inline(always)]
    pub fn update_book(&mut self, ticker: &TickerEvent) {
        self.last_bid = ticker.bid_price;
        self.last_ask = ticker.ask_price;
        if self.unarmed_total > 0 {
            for i in 0..self.n {
                self.arm_due(i, ticker.timestamp_ms);
            }
        }
    }

    /// Place the exits of positions filled while the strategy was busy, once
    /// it is free: the runner delivers `on_fill` at the first event after the
    /// busy window, and the strategy's `SetExits` is sent then.
    #[inline(always)]
    fn arm_due(&mut self, trial: usize, now: u64) {
        if self.unarmed[trial] == 0 || now < self.busy_until[trial] {
            return;
        }
        let base = trial * self.max_positions;
        for slot in base..base + self.max_positions {
            if self.pos_active[slot] != 0 && self.pos_tp_active_at[slot] == u64::MAX {
                self.arm_exits(trial, slot, now);
            }
        }
        self.unarmed_total -= self.unarmed[trial] as usize;
        self.unarmed[trial] = 0;
    }

    /// Exits go out at `now`: one round trip, and the stop also waits out its
    /// delay. Sending them keeps the strategy busy for the round trip.
    #[inline(always)]
    fn arm_exits(&mut self, trial: usize, slot: usize, now: u64) {
        let lat = self.latency_ms;
        self.pos_tp_active_at[slot] = now + lat;
        self.pos_sl_active_at[slot] = now + self.sl_delay[trial] + lat;
        self.busy_until[trial] = self.busy_until[trial].max(now + lat);
    }

    /// Round a limit price to the tick, as the scalar engine does before it
    /// stores an entry. Unchanged when `price_step` is 0.
    #[inline(always)]
    pub fn round_price(&self, price: f64) -> f64 {
        let s = self.price_step;
        if s > 0.0 { (price / s).round() * s } else { price }
    }

    /// Put the asked-for entry on the book once its round trip has landed.
    #[inline(always)]
    fn land(&mut self, trial: usize, now: u64) {
        if self.entry_price[trial] > 0.0 && self.entry_active_at[trial] != 0 && now >= self.entry_active_at[trial] {
            self.entry_active_at[trial] = 0;
        }
        if self.entry_active_at[trial] == 0 && self.entry_price[trial] > 0.0 {
            self.live_price[trial] = self.entry_price[trial];
            self.live_tp_price[trial] = self.pending_tp_price[trial];
            self.live_sl_price[trial] = self.pending_sl_price[trial];
        }
    }

    /// Place or amend a pending entry order with exit prices. Returns whether
    /// it sent a request (the strategy is then busy for `latency_ms`).
    ///
    /// The scalar engine's rules: the price is rounded to `price_step`; the
    /// same price again is skipped; a new price on the same side is an amend,
    /// so the old order stays on the book (and can fill, with its own TP/SL)
    /// until the amend lands; the other side replaces the order outright.
    #[inline(always)]
    pub fn set_entry(&mut self, trial: usize, price: f64, tp_price: f64, sl_price: f64, ts: u64, is_short: bool) -> bool {
        // Indicator warmup is enforced here rather than left to each strategy,
        // and it covers the whole declared set — which is shared by the grid, so
        // every trial in a sweep starts trading on the same bar. Per-trial
        // warmup would let short periods trade hours earlier, and
        // `compute_score` multiplies by trade count, so they would win the
        // sweep on warmup rather than on edge.
        if !self.indicators_warm() {
            return false;
        }
        let price = self.round_price(price);
        let amend = self.entry_price[trial] > 0.0 && self.entry_is_short[trial] == is_short;
        if amend {
            if self.entry_price[trial] == price {
                return false;
            }
            // Whatever was in flight lands first; it stays on the book until
            // this amend does.
            self.land(trial, ts);
        } else {
            self.live_price[trial] = 0.0;
        }
        self.entry_price[trial] = price;
        self.pending_tp_price[trial] = tp_price;
        self.pending_sl_price[trial] = sl_price;
        self.entry_is_short[trial] = is_short;
        let jitter = self.rand_u64(self.jitter_ms + 1);
        let delay = self.latency_ms + jitter;
        self.entry_active_at[trial] = if delay == 0 { 0 } else { ts + delay };
        // Busy until the reply, which is when the order lands.
        self.busy_until[trial] = self.busy_until[trial].max(ts + delay);
        self.land(trial, ts);
        true
    }

    /// Position quantity for an entry at `entry_price`, like the scalar
    /// engine's `size_to_quantity`. Contracts on inverse.
    #[inline(always)]
    pub fn entry_qty_for(&self, trial: usize, entry_price: f64) -> f64 {
        // Any size the strategy set, as the scalar engine takes it: 0 is no
        // trade on linear and one contract on inverse.
        let size = self.order_size[trial];
        if !size.is_nan() {
            return match self.market_type {
                MarketType::Inverse => (size * entry_price / self.contract_size).floor().max(1.0),
                _ => size / entry_price,
            };
        }
        self.entry_qty(entry_price)
    }

    /// Quantity when the strategy gives no order size: the trial's whole
    /// margin slot times leverage.
    #[inline(always)]
    pub fn entry_qty(&self, entry_price: f64) -> f64 {
        let notional = self.margin_per_pos * self.leverage;
        match self.market_type {
            MarketType::Inverse => (notional / self.contract_size).round().max(1.0),
            _ => notional / entry_price,
        }
    }

    /// Process a trade event: exits of open positions, then the entry.
    ///
    /// Exits go first so a position filled by this very print is not checked
    /// against it: the scalar engine arms a position's exits from `on_fill`,
    /// after this trade has been matched.
    pub fn check_trade(&mut self, trade: &TradeEvent) {
        let price = trade.price;
        let ts = trade.timestamp_ms;

        let n = self.n;
        let mp = self.max_positions;

        for i in 0..n {
            // Step 0: a fill that waited for the busy window is delivered.
            self.arm_due(i, ts);

            // Step 1: TP/SL for all position slots.
            let base = i * mp;
            for j in 0..mp {
                let slot = base + j;
                if self.pos_active[slot] != 0 {
                    let is_short = self.pos_is_short[slot];
                    // Long TP: price rises to TP. Short TP: price drops to TP.
                    let tp_hit = if is_short { price <= self.pos_tp_price[slot] } else { price >= self.pos_tp_price[slot] };
                    // Long SL: price drops to SL. Short SL: price rises to SL.
                    let sl_hit = if is_short { price >= self.pos_sl_price[slot] } else { price <= self.pos_sl_price[slot] };
                    if tp_hit && ts >= self.pos_tp_active_at[slot] {
                        let tp = self.pos_tp_price[slot];
                        self.close_position_at(i, slot, tp, true, ts);
                        self.total_exits += 1;
                    } else if sl_hit && ts >= self.pos_sl_active_at[slot] {
                        let exit = self.stop_fill_price(is_short, price);
                        self.close_position_at(i, slot, exit, false, ts);
                        self.total_exits += 1;
                    }
                }
            }

            // Step 2: the amend or placement in flight lands, then the order
            // on the book fills if this print crosses it. Long limit fills when
            // price drops to it; short when price rises to it.
            if self.entry_price[i] > 0.0 {
                self.land(i, ts);
                let live = self.live_price[i];
                if live > 0.0 {
                    let fills = if self.entry_is_short[i] { price >= live } else { price <= live };
                    if fills {
                        self.fill_entry(i, ts);
                    }
                }
            }
        }
    }

    /// A stop's fill: the worst of the book side and the trade print, then
    /// `slippage_pct` if asked for. Before any book, the print.
    #[inline(always)]
    fn stop_fill_price(&self, is_short: bool, price: f64) -> f64 {
        if is_short {
            // Buying back: the higher of ask and print.
            let p = if self.last_ask > 0.0 { self.last_ask.max(price) } else { price };
            p * (1.0 + self.slippage_pct)
        } else {
            // Selling: the lower of bid and print.
            let p = if self.last_bid > 0.0 { self.last_bid.min(price) } else { price };
            p * (1.0 - self.slippage_pct)
        }
    }

    #[inline(always)]
    fn fill_entry(&mut self, trial: usize, ts: u64) {
        let entry = self.live_price[trial];
        let qty = self.entry_qty_for(trial, entry);
        let is_short = self.entry_is_short[trial];
        let tp = self.live_tp_price[trial];
        let sl = self.live_sl_price[trial];
        // The order is gone, and with it any amend still in flight.
        self.entry_active_at[trial] = 0;
        self.entry_price[trial] = 0.0;
        self.live_price[trial] = 0.0;
        if qty <= 0.0 {
            return;
        }
        let mp = self.max_positions;
        let base = trial * mp;
        for j in 0..mp {
            let slot = base + j;
            if self.pos_active[slot] == 0 {
                self.pos_active[slot] = 1;
                self.pos_entry_price[slot] = entry;
                self.pos_quantity[slot] = qty;
                self.pos_tp_price[slot] = tp;
                self.pos_sl_price[slot] = sl;
                self.pos_is_short[slot] = is_short;
                if ts >= self.busy_until[trial] {
                    self.arm_exits(trial, slot, ts);
                } else {
                    // Not armed until the strategy hears of the fill.
                    self.pos_tp_active_at[slot] = u64::MAX;
                    self.pos_sl_active_at[slot] = u64::MAX;
                    self.unarmed[trial] += 1;
                    self.unarmed_total += 1;
                }
                self.active_count[trial] += 1;
                self.total_entries += 1;
                break;
            }
        }
    }

    #[inline(always)]
    fn close_position_at(&mut self, trial: usize, slot: usize, exit_price: f64, is_tp: bool, timestamp_ms: u64) {
        let qty = self.pos_quantity[slot];
        let entry = self.pos_entry_price[slot];
        let dir: f64 = if self.pos_is_short[slot] { -1.0 } else { 1.0 };
        let exit_fee_rate = if is_tp { self.maker_fee } else { self.taker_fee };

        let net_pnl = match self.market_type {
            MarketType::Inverse => {
                let cs = self.contract_size;
                let pnl_coin = dir * cs * qty * (1.0 / entry - 1.0 / exit_price);
                let notional_entry = cs * qty / entry;
                let notional_exit = cs * qty / exit_price;
                let entry_fee = notional_entry * self.maker_fee;
                let exit_fee = notional_exit * exit_fee_rate;
                let net_coin = pnl_coin - entry_fee - exit_fee;
                net_coin * exit_price
            }
            _ => {
                let gross_pnl = dir * qty * (exit_price - entry);
                let entry_fee = qty * entry * self.maker_fee;
                let exit_fee = qty * exit_price * exit_fee_rate;
                gross_pnl - entry_fee - exit_fee
            }
        };

        let ret = if entry > 0.0 { dir * (exit_price - entry) / entry } else { 0.0 };

        self.balance[trial] += net_pnl;
        self.pos_active[slot] = 0;
        self.active_count[trial] -= 1;
        self.trade_count[trial] += 1;
        if self.first_trade_at[trial] == 0 {
            self.first_trade_at[trial] = timestamp_ms;
        }
        self.last_trade_at[trial] = timestamp_ms;
        self.total_pnl[trial] += net_pnl;

        if net_pnl > 0.0 {
            self.winning_trades[trial] += 1;
            self.sum_wins[trial] += net_pnl;
        } else if net_pnl < 0.0 {
            self.losing_trades[trial] += 1;
            self.sum_losses[trial] += net_pnl.abs();
        }

        self.sum_returns[trial] += ret;
        self.sum_returns_sq[trial] += ret * ret;
        if ret < 0.0 {
            self.sum_neg_returns_sq[trial] += ret * ret;
        }

        let equity = self.balance[trial];
        if equity > self.peak_equity[trial] {
            self.peak_equity[trial] = equity;
        }
        let dd = self.peak_equity[trial] - equity;
        if dd > self.max_drawdown_abs[trial] {
            self.max_drawdown_abs[trial] = dd;
        }
        if self.peak_equity[trial] > 0.0 {
            let dd_pct = dd / self.peak_equity[trial] * 100.0;
            if dd_pct > self.max_drawdown[trial] {
                self.max_drawdown[trial] = dd_pct;
            }
        }
    }

    /// Force-close all positions against the given book.
    ///
    /// Side-aware: a long is closed by selling, which happens at the **bid**; a
    /// short is closed by buying, which happens at the **ask**. Pricing every
    /// position at the bid hands each open short the full spread, and since
    /// this runs at the end of every sweep trial that bias accumulates into the
    /// rankings rather than into a single trade. Matches the scalar engine's
    /// `market_close_position` (`backtest/src/exchange.rs`, `Long => bid,
    /// Short => ask`).
    pub fn force_close_all(&mut self, bid_price: f64, ask_price: f64) {
        let mp = self.max_positions;
        for i in 0..self.n {
            self.entry_price[i] = 0.0;
            self.live_price[i] = 0.0;
            self.entry_active_at[i] = 0;
            self.unarmed[i] = 0;
            let base = i * mp;
            for j in 0..mp {
                let slot = base + j;
                if self.pos_active[slot] != 0 {
                    let exit_price = if self.pos_is_short[slot] { ask_price } else { bid_price };
                    self.close_position_at(i, slot, exit_price, false, 0);
                }
            }
        }
        self.unarmed_total = 0;
    }

    /// Collect results for all trials. The ratios follow the scalar engine's
    /// `BacktestMetrics` (standard deviation floored at 1e-4, an all-win
    /// profit factor is infinite).
    pub fn results(&self) -> Vec<BatchResult> {
        (0..self.n)
            .map(|i| {
                let tc = self.trade_count[i];
                let total_pnl = self.total_pnl[i];
                let total_pnl_pct = total_pnl / self.initial_balance * 100.0;

                let win_rate = if tc > 0 {
                    self.winning_trades[i] as f64 / tc as f64 * 100.0
                } else {
                    0.0
                };

                let profit_factor = if tc == 0 {
                    0.0
                } else if self.sum_losses[i] > 0.0 {
                    self.sum_wins[i] / self.sum_losses[i]
                } else {
                    f64::INFINITY
                };

                let (sharpe_ratio, sortino_ratio) = if tc >= 2 {
                    let n = tc as f64;
                    let mean = self.sum_returns[i] / n;
                    let variance = (self.sum_returns_sq[i] - n * mean * mean) / (n - 1.0);
                    let std_dev = variance.max(0.0).sqrt().max(1e-4);
                    let downside_dev = (self.sum_neg_returns_sq[i] / (n - 1.0)).sqrt().max(1e-4);
                    (mean / std_dev, mean / downside_dev)
                } else {
                    (0.0, 0.0)
                };

                BatchResult {
                    total_pnl,
                    total_pnl_pct,
                    final_balance: self.balance[i],
                    trade_count: tc,
                    winning_trades: self.winning_trades[i],
                    losing_trades: self.losing_trades[i],
                    win_rate,
                    profit_factor,
                    max_drawdown: self.max_drawdown_abs[i],
                    max_drawdown_pct: self.max_drawdown[i],
                    sharpe_ratio,
                    sortino_ratio,
                    calmar_ratio: 0.0,
                }
            })
            .collect()
    }

    /// Diagnostic snapshot: pending entries + open positions.
    pub fn diagnostics(&self) -> BatchDiagnostics {
        let entries_active = self.entry_price.iter().filter(|&&p| p > 0.0).count();
        let positions_open = (self.total_entries - self.total_exits) as usize;
        BatchDiagnostics { entries_active, positions_open }
    }

    /// Publish this pass's indicator readings, in the order the batch declared
    /// its requests. Called by the driver before the per-trial loop, so every
    /// trial in the pass reads identical values.
    #[inline]
    pub fn set_indicators(&mut self, values: &[IndicatorValue]) {
        self.indicators.clear();
        self.indicators.extend_from_slice(values);
    }

    /// The `i`th declared indicator's value, or `None` while it is cold.
    #[inline(always)]
    pub fn indicator(&self, i: usize) -> Option<f64> {
        self.indicators.get(i).and_then(IndicatorValue::get)
    }

    /// Whether every declared indicator has warmed up. Vacuously true when the
    /// batch declared none.
    #[inline(always)]
    pub fn indicators_warm(&self) -> bool {
        self.indicators.iter().all(|v| v.ready)
    }

    /// Estimated heap memory usage in bytes for all trial arrays.
    pub fn estimated_ram_bytes(&self) -> usize {
        let n = self.n;
        let slots = n * self.max_positions;
        // Per-trial vectors (n elements each)
        let per_trial_f64 = 8; // order_size, entry_price, pending_tp, pending_sl, live_price, live_tp, live_sl, balance
        let per_trial_f64_metrics = 9; // total_pnl, sum_wins, sum_losses, sum_returns, sum_returns_sq, sum_neg_returns_sq, peak_equity, max_drawdown, max_drawdown_abs
        let per_trial_u64 = 5; // sl_delay, busy_until, entry_active_at, first_trade_at, last_trade_at
        let per_trial_u32 = 3; // trade_count, winning_trades, losing_trades
        let per_trial_u8 = 3; // active_count, entry_is_short, unarmed
        // Per-slot vectors (n * max_positions elements each)
        let per_slot_f64 = 4; // pos_entry_price, pos_quantity, pos_tp_price, pos_sl_price
        let per_slot_u64 = 2; // pos_tp_active_at, pos_sl_active_at
        let per_slot_u8 = 2; // pos_active, pos_is_short

        // Indicator readings are shared by all trials — a handful of values
        // counted once, not per trial. The instances themselves live in the
        // driver, not here.
        let indicator_bytes = self.indicators.len() * std::mem::size_of::<IndicatorValue>();

        n * (per_trial_f64 + per_trial_f64_metrics) * 8
            + n * per_trial_u64 * 8
            + n * per_trial_u32 * 4
            + n * per_trial_u8
            + slots * per_slot_f64 * 8
            + slots * per_slot_u64 * 8
            + slots * per_slot_u8
            + indicator_bytes
    }

    /// Reset all *trial* state for a new evaluation window (shadow mode).
    ///
    /// Deliberately leaves [`indicators`](Self::indicators) alone: they hold
    /// market history, not trial state, and clearing them every window would
    /// re-run the warmup on each one — which biases scoring toward short
    /// periods, since an indicator that is not ready yet produces no trades and
    /// `compute_score` multiplies by the trade count. (The instances live in the
    /// driver regardless; this only concerns the published readings.)
    pub fn reset(&mut self) {
        self.entry_price.fill(0.0);
        self.pending_tp_price.fill(0.0);
        self.pending_sl_price.fill(0.0);
        self.entry_is_short.fill(false);
        self.entry_active_at.fill(0);
        self.busy_until.fill(0);
        self.unarmed.fill(0);
        self.unarmed_total = 0;
        self.live_price.fill(0.0);
        self.live_tp_price.fill(0.0);
        self.live_sl_price.fill(0.0);
        self.pos_active.fill(0);
        self.pos_entry_price.fill(0.0);
        self.pos_quantity.fill(0.0);
        self.pos_tp_price.fill(0.0);
        self.pos_sl_price.fill(0.0);
        self.pos_tp_active_at.fill(0);
        self.pos_sl_active_at.fill(0);
        self.pos_is_short.fill(false);
        self.active_count.fill(0);
        self.balance.fill(self.initial_balance);
        self.trade_count.fill(0);
        self.winning_trades.fill(0);
        self.losing_trades.fill(0);
        self.total_pnl.fill(0.0);
        self.sum_wins.fill(0.0);
        self.sum_losses.fill(0.0);
        self.sum_returns.fill(0.0);
        self.sum_returns_sq.fill(0.0);
        self.sum_neg_returns_sq.fill(0.0);
        self.peak_equity.fill(self.initial_balance);
        self.max_drawdown.fill(0.0);
        self.max_drawdown_abs.fill(0.0);
        self.first_trade_at.fill(0);
        self.last_trade_at.fill(0);
        self.total_entries = 0;
        self.total_exits = 0;
    }

    /// Timestamp of first completed trade for a trial (0 if no trades).
    pub fn first_trade_at(&self, trial: usize) -> u64 { self.first_trade_at[trial] }

    /// Timestamp of most recent completed trade for a trial (0 if no trades).
    pub fn last_trade_at(&self, trial: usize) -> u64 { self.last_trade_at[trial] }

    /// Current trade count for a trial.
    pub fn trade_count(&self, trial: usize) -> u32 { self.trade_count[trial] }
}
