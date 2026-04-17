#[derive(Debug, Clone)]
pub struct BookTicker {
    pub symbol: String,
    pub bid_price: f64,
    pub bid_quantity: f64,
    pub ask_price: f64,
    pub ask_quantity: f64,
    pub timestamp: u64,
}

#[derive(Debug, Clone)]
pub struct KlineData {
    pub symbol: String,
    pub interval: String,
    pub open_time: u64,
    pub close_time: u64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub quote_volume: f64,
    pub trades: u32,
    pub is_closed: bool,
}

#[derive(Debug, Clone)]
pub struct TradeData {
    pub symbol: String,
    pub price: f64,
    pub quantity: f64,
    pub timestamp: u64,
    pub is_buyer_maker: bool,
}

#[derive(Debug, Clone)]
pub struct Ticker24hr {
    pub symbol: String,
    pub price_change_percent: f64,
    pub last_price: f64,
    pub volume: f64,
    pub quote_volume: f64,
}

/// A single price level in the order book (L2 depth).
///
/// `#[repr(C)]` with `f64 + f64` layout so it is bit-compatible with the
/// fixed-size arrays stored in `DepthEvent` (the on-disk prepared format).
/// This lets the backtest runner refresh a `Vec<DepthLevel>` from a
/// `&[DepthLevel]` memcpy with no conversion cost.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C, align(8))]
pub struct DepthLevel {
    pub price: f64,
    pub quantity: f64,
}

const _: () = assert!(std::mem::size_of::<DepthLevel>() == 16);

/// L2 order book depth snapshot.
/// Bids sorted descending by price (best bid first),
/// asks sorted ascending by price (best ask first).
#[derive(Debug, Clone)]
pub struct OrderBookDepth {
    pub bids: Vec<DepthLevel>,
    pub asks: Vec<DepthLevel>,
    pub timestamp_ms: u64,
}
