#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderType {
    Market,
    Limit,
    StopMarket,
    StopLimit,
    TrailingStopMarket,
    /// Exchange-initiated liquidation (e.g. Binance `autoclose-*` orders).
    Liquidation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderStatus {
    New,
    Filled,
    PartiallyFilled,
    Canceled,
    Closed,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarketType {
    Spot,
    Linear,
    Inverse,
}

impl MarketType {
    /// Stable lowercase string used for SQL serialization (e.g. `trades.db.market_type`).
    /// Do not change — bound by on-disk schema in `~/.tradectl/trades.db`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spot => "spot",
            Self::Linear => "linear",
            Self::Inverse => "inverse",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimeInForce {
    Gtc,
    Ioc,
    Fok,
    Gtx,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub enum Side {
    Long,
    Short,
}

impl<'de> serde::Deserialize<'de> for Side {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.to_uppercase().as_str() {
            "LONG" => Ok(Side::Long),
            "SHORT" => Ok(Side::Short),
            _ => Err(serde::de::Error::unknown_variant(&s, &["LONG", "SHORT"])),
        }
    }
}

impl std::fmt::Display for Side {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Side::Long => write!(f, "LONG"),
            Side::Short => write!(f, "SHORT"),
        }
    }
}

impl Default for Side {
    fn default() -> Self {
        Side::Long
    }
}

