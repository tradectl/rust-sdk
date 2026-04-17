//! Market event types used across the SDK, live runner, collector, backtest,
//! and prepared-binary (`.bin`) format.
//!
//! Every struct here is `#[repr(C)]` with a compile-time size assertion so the
//! on-disk segmented format in `backtest/crates/backtest` can mmap arrays of
//! them as zero-copy `&[T]`. Adding or reordering fields is a breaking change.

use std::mem::size_of;

use crate::types::market_data::DepthLevel;

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct TickerEvent {
    pub bid_price: f64,
    pub bid_qty: f64,
    pub ask_price: f64,
    pub ask_qty: f64,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct TradeEvent {
    pub price: f64,
    pub quantity: f64,
    pub timestamp_ms: u64,
    pub is_buyer_maker: bool,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct KlineEvent {
    pub open_time_ms: u64,
    pub close_time_ms: u64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub quote_volume: f64,
    pub trade_count: u64,
    pub interval_ms: u32,
    pub closed: u8,
    pub _pad: [u8; 3],
}

impl KlineEvent {
    /// Timestamp used for merge ordering. Klines are anchored on their
    /// close time so they merge naturally after the trades in their window.
    #[inline]
    pub fn timestamp_ms(&self) -> u64 {
        self.close_time_ms
    }
}

/// Maximum depth levels per side stored per `DepthEvent`. Chosen to match the
/// largest Binance depth subscription used by the collector (`@depth20@100ms`).
pub const DEPTH_MAX_LEVELS: usize = 20;

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct DepthEvent {
    pub timestamp_ms: u64,
    pub bid_count: u8,
    pub ask_count: u8,
    pub _pad: [u8; 6],
    pub bids: [DepthLevel; DEPTH_MAX_LEVELS],
    pub asks: [DepthLevel; DEPTH_MAX_LEVELS],
}

impl DepthEvent {
    #[inline]
    pub fn bids(&self) -> &[DepthLevel] {
        &self.bids[..self.bid_count as usize]
    }

    #[inline]
    pub fn asks(&self) -> &[DepthLevel] {
        &self.asks[..self.ask_count as usize]
    }
}

impl Default for DepthEvent {
    fn default() -> Self {
        Self {
            timestamp_ms: 0,
            bid_count: 0,
            ask_count: 0,
            _pad: [0; 6],
            bids: [DepthLevel::default(); DEPTH_MAX_LEVELS],
            asks: [DepthLevel::default(); DEPTH_MAX_LEVELS],
        }
    }
}

/// Runtime-only tagged union yielded by the prepared-file merge iterator.
///
/// Not serialized. On-disk segments store the underlying `#[repr(C)]` structs
/// contiguously; the enum tag is synthesized during iteration.
#[derive(Debug, Clone, Copy)]
pub enum MarketEvent {
    Ticker(TickerEvent),
    Trade(TradeEvent),
    Kline(KlineEvent),
    Depth(DepthEvent),
}

impl MarketEvent {
    #[inline]
    pub fn timestamp_ms(&self) -> u64 {
        match self {
            MarketEvent::Ticker(t) => t.timestamp_ms,
            MarketEvent::Trade(t) => t.timestamp_ms,
            MarketEvent::Kline(k) => k.timestamp_ms(),
            MarketEvent::Depth(d) => d.timestamp_ms,
        }
    }
}

// ─── Size & layout guards ───────────────────────────────────────
// These asserts keep the on-disk format stable. Changing a field size trips
// compilation rather than silently corrupting mmapped data.

const _: () = assert!(size_of::<TickerEvent>() == 40);
const _: () = assert!(size_of::<TradeEvent>() == 32);
const _: () = assert!(size_of::<KlineEvent>() == 80);
const _: () = assert!(size_of::<DepthEvent>() == 656);
