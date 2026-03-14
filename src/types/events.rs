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
pub enum MarketEvent {
    Ticker(TickerEvent),
    Trade(TradeEvent),
}

impl MarketEvent {
    pub fn timestamp_ms(&self) -> u64 {
        match self {
            MarketEvent::Ticker(t) => t.timestamp_ms,
            MarketEvent::Trade(t) => t.timestamp_ms,
        }
    }
}
