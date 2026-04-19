//! Batch processing types for SoA (Structure of Arrays) shadow/sweep execution.
//!
//! Strategies that provide a [`BatchStrategy`] implementation can process
//! thousands of parameter variants per event in a single pass, enabling
//! ~1000x throughput improvement over per-variant [`Strategy`] instances.

use crate::types::{TickerEvent, TradeEvent, Params, MarketType};
use super::batch_exchange::BatchExchange;

// ---------------------------------------------------------------------------
// Config + Result
// ---------------------------------------------------------------------------

/// Configuration for batch strategy execution.
#[derive(Debug, Clone)]
pub struct BatchConfig {
    pub initial_balance: f64,
    pub taker_fee: f64,
    pub maker_fee: f64,
    pub slippage_pct: f64,
    pub leverage: f64,
    /// Base simulated exchange round-trip latency in milliseconds (0 = instant).
    pub latency_ms: u64,
    /// Random jitter range in milliseconds. Actual latency = `latency_ms + rand(0, jitter_ms)`.
    pub jitter_ms: u64,
    /// Stop-loss activation delay in milliseconds after entry fill.
    /// Matches the live runner's SL_DELAY (typically 3000ms).
    pub sl_delay_ms: u64,
    /// Market type — determines PnL calculation (linear vs inverse).
    pub market_type: MarketType,
    /// Contract size for inverse contracts (e.g. 10 for BNBUSD_PERP).
    /// Ignored for linear/spot.
    pub contract_size: f64,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            initial_balance: 10_000.0,
            taker_fee: 0.0004,
            maker_fee: 0.0002,
            slippage_pct: 0.0001,
            leverage: 1.0,
            latency_ms: 0,
            jitter_ms: 0,
            sl_delay_ms: 0,
            market_type: MarketType::Linear,
            contract_size: 0.0,
        }
    }
}

/// Diagnostic snapshot for shadow engine reporting.
#[derive(Debug, Clone, Default)]
pub struct BatchDiagnostics {
    /// Number of trials with a pending entry (entry_price > 0).
    pub entries_active: usize,
    /// Number of trials with at least one open position.
    pub positions_open: usize,
}

/// Per-trial result from a batch strategy run.
#[derive(Debug, Clone)]
pub struct BatchResult {
    pub total_pnl: f64,
    pub total_pnl_pct: f64,
    pub final_balance: f64,
    pub trade_count: u32,
    pub winning_trades: u32,
    pub win_rate: f64,
    pub profit_factor: f64,
    pub max_drawdown_pct: f64,
    pub sharpe_ratio: f64,
    pub sortino_ratio: f64,
    pub calmar_ratio: f64,
}

// ---------------------------------------------------------------------------
// BatchStrategy trait
// ---------------------------------------------------------------------------

/// SoA batch strategy trait.
///
/// Implementations store all trial state in parallel arrays and process
/// events in tight loops across all trials simultaneously. This enables
/// cache-friendly, vectorizable execution at ~0.002µs per variant per event.
///
/// # Plugin export
///
/// Strategies provide a batch implementation via [`declare_batch_strategy!`]:
/// ```rust,ignore
/// declare_batch_strategy!("bounce-back", BounceBack::new, BounceBackBatch::new);
/// ```
///
/// TODO(depth-in-batch): `BatchStrategy` currently sees only tickers and
/// trades — no `on_depth` hook, no `ctx.depth`. Batch runs power sweep/shadow,
/// so depth-aware strategies (e.g. sling-shot, when its depth signal is
/// enabled) are evaluated without the depth signal and will score differently
/// from a generic `Strategy` backtest on the same `.bin`. Closing this gap
/// needs (a) an `on_depth(&DepthEvent)` trait method (or SoA depth snapshot in
/// `BatchExchange`) and (b) the batch driver in
/// `backtest/src/batch.rs` to dispatch `MarketEvent::Depth`.
/// Tracked alongside the TCTL v3 format rollout (see memory
/// project_tctl_v3_format.md).
pub trait BatchStrategy: Send {
    /// Access the underlying exchange engine.
    fn exchange(&self) -> &BatchExchange;
    /// Mutable access to the underlying exchange engine.
    fn exchange_mut(&mut self) -> &mut BatchExchange;

    /// Process a book ticker event across all trials (strategy-specific).
    fn process_ticker(&mut self, ticker: &TickerEvent);

    /// Check a trade event for entry fills and exit triggers across all trials.
    fn check_trade(&mut self, trade: &TradeEvent) {
        self.exchange_mut().check_trade(trade);
    }
    /// Force-close all open positions at the given bid price.
    fn force_close_all(&mut self, bid_price: f64) {
        self.exchange_mut().force_close_all(bid_price);
    }
    /// Collect results for all trials.
    fn results(&self) -> Vec<BatchResult> {
        self.exchange().results()
    }
    /// Number of trials in this batch.
    fn trial_count(&self) -> usize {
        self.exchange().n
    }
    /// Reset all trial state for a new evaluation window (shadow mode).
    fn reset(&mut self) {
        self.exchange_mut().reset();
    }
    /// Return diagnostic counters for shadow engine reporting.
    fn diagnostics(&self) -> BatchDiagnostics {
        self.exchange().diagnostics()
    }
    /// Estimated heap memory usage in bytes for exchange arrays.
    fn estimated_ram_bytes(&self) -> usize {
        self.exchange().estimated_ram_bytes()
    }
}

// ---------------------------------------------------------------------------
// BatchFactory type
// ---------------------------------------------------------------------------

/// Factory function for creating a batch strategy from parameter sets.
///
/// Arguments: `(params_per_trial, config, max_positions_per_trial)`.
pub type BatchFactory = fn(&[Params], &BatchConfig, usize) -> Box<dyn BatchStrategy>;

// ---------------------------------------------------------------------------
// Score function
// ---------------------------------------------------------------------------

/// Composite score: `pnl% / (1 + max_dd%) * trade_factor`.
///
/// Rewards return, penalizes drawdown, requires sufficient sample size.
pub fn compute_score(
    total_pnl_pct: f64,
    trade_count: usize,
    max_drawdown_pct: f64,
    min_trades: usize,
) -> f64 {
    if trade_count == 0 {
        return f64::NEG_INFINITY;
    }

    let trade_factor = if min_trades == 0 {
        1.0
    } else {
        (trade_count as f64 / min_trades as f64).min(1.0)
    };
    let dd_penalty = 1.0 + max_drawdown_pct;

    total_pnl_pct * trade_factor / dd_penalty
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_no_trades() {
        assert!(compute_score(0.0, 0, 0.0, 5).is_infinite());
        assert!(compute_score(0.0, 0, 0.0, 5).is_sign_negative());
    }

    #[test]
    fn score_basic() {
        // 10% pnl, 5% dd, 10 trades, min 5
        // trade_factor = min(10/5, 1.0) = 1.0
        // score = 10.0 * 1.0 / (1 + 5) = 10/6 ≈ 1.667
        let s = compute_score(10.0, 10, 5.0, 5);
        assert!((s - 10.0 / 6.0).abs() < 1e-10);
    }

    #[test]
    fn score_trade_penalty() {
        let full = compute_score(10.0, 10, 5.0, 10);
        let half = compute_score(10.0, 5, 5.0, 10);
        assert!((half - full * 0.5).abs() < 1e-10);
    }

    #[test]
    fn score_negative_pnl() {
        assert!(compute_score(-10.0, 10, 5.0, 5) < 0.0);
    }

    #[test]
    fn score_min_trades_zero() {
        // min_trades=0 → trade_factor=1.0 (no NaN)
        let s = compute_score(5.0, 1, 0.0, 0);
        assert!((s - 5.0).abs() < 1e-10);
    }
}
