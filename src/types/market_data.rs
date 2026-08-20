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

/// One account income event — a funding payment above all.
///
/// The bot's realized P&L is built from order fills, and funding is not a
/// fill: it is charged against the *netted* position of a symbol, on the
/// venue's own schedule, with no order behind it. Nothing in the fill stream
/// reports it, which is why it needs its own path from the venue.
#[derive(Debug, Clone, PartialEq)]
pub struct FundingEntry {
    /// Symbol the venue attributed the payment to. May be empty for
    /// account-wide income.
    pub symbol: String,
    /// Signed, in `asset`. Negative = the account paid.
    pub amount: f64,
    /// Settlement currency — USDT on a linear account, the base coin on an
    /// inverse one. Carried rather than assumed: summing a coin-denominated
    /// amount into a USD total invents a number.
    pub asset: String,
    /// The venue's own transaction id. The dedup key that lets a poller
    /// re-read an overlapping window safely, so it must be stable and unique
    /// per account — never synthesized from the row's contents.
    pub txn_id: String,
    /// When the venue booked it, epoch millis.
    pub time_ms: i64,
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
