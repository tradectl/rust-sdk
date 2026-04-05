//! Reusable SoA batch exchange engine for shadow/sweep execution.
//!
//! Manages positions, fills, TP/SL, PnL calculation, and metrics for N trials
//! with up to `max_positions` concurrent positions each. Strategy implementations
//! compose this: they own a `BatchExchange` and set entry prices via
//! `process_ticker()`. Everything else — fill scheduling, exit checks, metrics —
//! is handled here.

use crate::types::{TradeEvent, MarketType};
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
    sl_delay_ms: u64,
    pub market_type: MarketType,
    pub contract_size: f64,

    // ── Pending entry state (per trial) ──────────────────────────────
    /// Pending entry price. 0.0 = no pending entry.
    pub entry_price: Vec<f64>,
    /// Pending TP/SL prices — set alongside entry, applied on fill.
    pending_tp_price: Vec<f64>,
    pending_sl_price: Vec<f64>,
    /// Timestamp when entry becomes active on the exchange (placement latency).
    /// 0 = entry is active and fillable. >0 = entry placed but not yet on the book.
    pub entry_active_at: Vec<u64>,

    // ── Position state: n * max_positions elements ───────────────────
    pub pos_active: Vec<u8>,
    pos_entry_price: Vec<f64>,
    pos_quantity: Vec<f64>,
    pos_tp_price: Vec<f64>,
    pos_sl_price: Vec<f64>,
    pos_sl_active_at: Vec<u64>,

    // ── Per-trial counters ───────────────────────────────────────────
    pub active_count: Vec<u8>,
    pub balance: Vec<f64>,

    // ── Metrics accumulators (per trial) ─────────────────────────────
    trade_count: Vec<u32>,
    winning_trades: Vec<u32>,
    total_pnl: Vec<f64>,
    sum_wins: Vec<f64>,
    sum_losses: Vec<f64>,
    sum_returns: Vec<f64>,
    sum_returns_sq: Vec<f64>,
    sum_neg_returns_sq: Vec<f64>,
    peak_equity: Vec<f64>,
    max_drawdown: Vec<f64>,
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
            sl_delay_ms: config.sl_delay_ms,
            market_type: config.market_type,
            contract_size: config.contract_size,
            entry_price: vec![0.0; n],
            pending_tp_price: vec![0.0; n],
            pending_sl_price: vec![0.0; n],
            entry_active_at: vec![0; n],
            pos_active: vec![0; total_slots],
            pos_entry_price: vec![0.0; total_slots],
            pos_quantity: vec![0.0; total_slots],
            pos_tp_price: vec![0.0; total_slots],
            pos_sl_price: vec![0.0; total_slots],
            pos_sl_active_at: vec![0; total_slots],
            active_count: vec![0; n],
            balance: vec![config.initial_balance; n],
            trade_count: vec![0; n],
            winning_trades: vec![0; n],
            total_pnl: vec![0.0; n],
            sum_wins: vec![0.0; n],
            sum_losses: vec![0.0; n],
            sum_returns: vec![0.0; n],
            sum_returns_sq: vec![0.0; n],
            sum_neg_returns_sq: vec![0.0; n],
            peak_equity: vec![config.initial_balance; n],
            max_drawdown: vec![0.0; n],
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

    /// Place or edit a pending entry order with exit prices.
    /// Applies placement latency + jitter. Strategy computes TP/SL from its params.
    #[inline(always)]
    pub fn set_entry(&mut self, trial: usize, price: f64, tp_price: f64, sl_price: f64, ts: u64) {
        self.entry_price[trial] = price;
        self.pending_tp_price[trial] = tp_price;
        self.pending_sl_price[trial] = sl_price;
        let jitter = self.rand_u64(self.jitter_ms + 1);
        self.entry_active_at[trial] = ts + self.latency_ms + jitter;
    }

    /// Compute position quantity from entry price.
    #[inline(always)]
    pub fn entry_qty(&self, entry_price: f64) -> f64 {
        let notional = self.margin_per_pos * self.leverage;
        match self.market_type {
            MarketType::Inverse => (notional / self.contract_size).round().max(1.0),
            _ => notional / entry_price,
        }
    }

    /// Process a trade event: check delayed fills, entry crosses, and TP/SL exits.
    pub fn check_trade(&mut self, trade: &TradeEvent) {
        let price = trade.price;
        let ts = trade.timestamp_ms;
        let n = self.n;
        let mp = self.max_positions;
        let _latency = self.latency_ms;
        let sl_delay = self.sl_delay_ms;

        for i in 0..n {
            // Step 1: Placement cooldown expired → entry is now on the book
            if self.entry_active_at[i] > 0 && ts >= self.entry_active_at[i] {
                self.entry_active_at[i] = 0;
            }

            // Step 2: Trade crosses active entry → fill immediately
            if self.entry_active_at[i] == 0 && self.entry_price[i] > 0.0 && price <= self.entry_price[i] {
                self.fill_entry(i, ts, sl_delay);
            }

            // Step 3: Check TP/SL for all position slots
            let base = i * mp;
            for j in 0..mp {
                let slot = base + j;
                if self.pos_active[slot] != 0 {
                    if price >= self.pos_tp_price[slot] {
                        let tp = self.pos_tp_price[slot];
                        self.close_position_at(i, slot, tp, true, ts);
                        self.total_exits += 1;
                    } else if price <= self.pos_sl_price[slot] && ts >= self.pos_sl_active_at[slot] {
                        let exit = price * (1.0 - self.slippage_pct);
                        self.close_position_at(i, slot, exit, false, ts);
                        self.total_exits += 1;
                    }
                }
            }
        }
    }

    #[inline(always)]
    fn fill_entry(&mut self, trial: usize, ts: u64, sl_delay: u64) {
        let entry = self.entry_price[trial];
        let qty = self.entry_qty(entry);
        let mp = self.max_positions;
        let base = trial * mp;
        for j in 0..mp {
            let slot = base + j;
            if self.pos_active[slot] == 0 {
                self.pos_active[slot] = 1;
                self.pos_entry_price[slot] = entry;
                self.pos_quantity[slot] = qty;
                self.pos_tp_price[slot] = self.pending_tp_price[trial];
                self.pos_sl_price[slot] = self.pending_sl_price[trial];
                self.pos_sl_active_at[slot] = ts + sl_delay;
                break;
            }
        }
        self.active_count[trial] += 1;
        self.entry_active_at[trial] = 0;
        self.entry_price[trial] = 0.0;
        self.total_entries += 1;
    }

    #[inline(always)]
    fn close_position_at(&mut self, trial: usize, slot: usize, exit_price: f64, is_tp: bool, timestamp_ms: u64) {
        let qty = self.pos_quantity[slot];
        let entry = self.pos_entry_price[slot];
        let exit_fee_rate = if is_tp { self.maker_fee } else { self.taker_fee };

        let (net_pnl, margin) = match self.market_type {
            MarketType::Inverse => {
                let cs = self.contract_size;
                let pnl_coin = cs * qty * (1.0 / entry - 1.0 / exit_price);
                let notional_entry = cs * qty / entry;
                let notional_exit = cs * qty / exit_price;
                let entry_fee = notional_entry * self.maker_fee;
                let exit_fee = notional_exit * exit_fee_rate;
                let net_coin = pnl_coin - entry_fee - exit_fee;
                let net_usd = net_coin * exit_price;
                let margin_coin = notional_entry / self.leverage;
                (net_usd, margin_coin * exit_price)
            }
            _ => {
                let gross_pnl = qty * (exit_price - entry);
                let entry_fee = qty * entry * self.maker_fee;
                let exit_fee = qty * exit_price * exit_fee_rate;
                let net = gross_pnl - entry_fee - exit_fee;
                let margin = qty * entry / self.leverage;
                (net, margin)
            }
        };

        let ret = if margin > 0.0 { net_pnl / margin } else { 0.0 };

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
        if self.peak_equity[trial] > 0.0 {
            let dd_pct = dd / self.peak_equity[trial] * 100.0;
            if dd_pct > self.max_drawdown[trial] {
                self.max_drawdown[trial] = dd_pct;
            }
        }
    }

    /// Force-close all positions at the given bid price.
    pub fn force_close_all(&mut self, bid_price: f64) {
        let mp = self.max_positions;
        for i in 0..self.n {
            self.entry_price[i] = 0.0;
            let base = i * mp;
            for j in 0..mp {
                let slot = base + j;
                if self.pos_active[slot] != 0 {
                    self.close_position_at(i, slot, bid_price, false, 0);
                }
            }
        }
    }

    /// Collect results for all trials.
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

                let profit_factor = if self.sum_losses[i] > 0.0 {
                    self.sum_wins[i] / self.sum_losses[i]
                } else if self.sum_wins[i] > 0.0 {
                    f64::INFINITY
                } else {
                    0.0
                };

                let (sharpe_ratio, sortino_ratio) = if tc >= 2 {
                    let n = tc as f64;
                    let mean = self.sum_returns[i] / n;
                    let variance = (self.sum_returns_sq[i] - n * mean * mean) / (n - 1.0);
                    let std_dev = variance.max(0.0).sqrt();
                    let sharpe = if std_dev < 1e-10 {
                        if mean > 0.0 { f64::INFINITY } else { 0.0 }
                    } else {
                        mean / std_dev
                    };
                    let downside_variance = self.sum_neg_returns_sq[i] / (n - 1.0);
                    let downside_dev = downside_variance.sqrt();
                    let sortino = if downside_dev < 1e-10 {
                        if mean > 0.0 { f64::INFINITY } else { 0.0 }
                    } else {
                        mean / downside_dev
                    };
                    (sharpe, sortino)
                } else {
                    (0.0, 0.0)
                };

                BatchResult {
                    total_pnl,
                    total_pnl_pct,
                    final_balance: self.balance[i],
                    trade_count: tc,
                    winning_trades: self.winning_trades[i],
                    win_rate,
                    profit_factor,
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

    /// Estimated heap memory usage in bytes for all trial arrays.
    pub fn estimated_ram_bytes(&self) -> usize {
        let n = self.n;
        let slots = n * self.max_positions;
        // Per-trial vectors (n elements each)
        let per_trial_f64 = 4; // entry_price, pending_tp, pending_sl, balance
        let per_trial_f64_metrics = 8; // total_pnl, sum_wins, sum_losses, sum_returns, sum_returns_sq, sum_neg_returns_sq, peak_equity, max_drawdown
        let per_trial_u64 = 3; // entry_active_at, first_trade_at, last_trade_at
        let per_trial_u32 = 2; // trade_count, winning_trades
        let per_trial_u8 = 1; // active_count
        // Per-slot vectors (n * max_positions elements each)
        let per_slot_f64 = 4; // pos_entry_price, pos_quantity, pos_tp_price, pos_sl_price
        let per_slot_u64 = 1; // pos_sl_active_at
        let per_slot_u8 = 1; // pos_active

        n * (per_trial_f64 + per_trial_f64_metrics) * 8
            + n * per_trial_u64 * 8
            + n * per_trial_u32 * 4
            + n * per_trial_u8
            + slots * per_slot_f64 * 8
            + slots * per_slot_u64 * 8
            + slots * per_slot_u8
    }

    /// Reset all state for a new evaluation window.
    pub fn reset(&mut self) {
        self.entry_price.fill(0.0);
        self.pending_tp_price.fill(0.0);
        self.pending_sl_price.fill(0.0);
        self.entry_active_at.fill(0);
        self.pos_active.fill(0);
        self.pos_entry_price.fill(0.0);
        self.pos_quantity.fill(0.0);
        self.pos_tp_price.fill(0.0);
        self.pos_sl_price.fill(0.0);
        self.pos_sl_active_at.fill(0);
        self.active_count.fill(0);
        self.balance.fill(self.initial_balance);
        self.trade_count.fill(0);
        self.winning_trades.fill(0);
        self.total_pnl.fill(0.0);
        self.sum_wins.fill(0.0);
        self.sum_losses.fill(0.0);
        self.sum_returns.fill(0.0);
        self.sum_returns_sq.fill(0.0);
        self.sum_neg_returns_sq.fill(0.0);
        self.peak_equity.fill(self.initial_balance);
        self.max_drawdown.fill(0.0);
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
