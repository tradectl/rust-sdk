//! Shared fill price logic for emulated order execution.
//!
//! Uses both bookTicker (bid/ask) and aggTrade price to simulate realistic fills.
//! Limit orders fill at their posted price. Stop/market orders use pessimistic
//! pricing — the worse of bid/ask vs trade price for the trader.
//!
//! Used by both `InMemoryExchange` (backtest) and `PaperAdapter` (live paper).

use crate::types::OrderSide;

/// Pessimistic fill price for market/stop orders.
///
/// Uses both bookTicker and trade data, picking the **worse** price for the
/// trader — simulates real exchange behavior where market orders fill at
/// bid (sells) or ask (buys), whichever is worse.
///
/// Buy (closing short): `max(ask, trade_price)` — higher is worse for buyer.
/// Sell (closing long): `min(bid, trade_price)` — lower is worse for seller.
pub fn pessimistic_fill_price(side: OrderSide, bid: f64, ask: f64, trade_price: f64) -> f64 {
    match side {
        OrderSide::Buy => ask.max(trade_price),
        OrderSide::Sell => bid.min(trade_price),
    }
}

/// Check if a limit order triggers and return the fill price.
///
/// Limit buy triggers when `trade_price <= limit_price`.
/// Limit sell triggers when `trade_price >= limit_price`.
///
/// Fills at the **limit price** — a resting limit order on the exchange
/// fills at its posted price, not the crossing trade price.
///
/// Returns `Some(limit_price)` if triggered, `None` otherwise.
pub fn check_limit_fill(
    side: OrderSide,
    limit_price: f64,
    trade_price: f64,
) -> Option<f64> {
    let triggered = match side {
        OrderSide::Buy => trade_price <= limit_price,
        OrderSide::Sell => trade_price >= limit_price,
    };
    if !triggered {
        return None;
    }
    Some(limit_price)
}

/// Check if a stop-market order triggers and compute its fill price.
///
/// Stop buy triggers when `trade_price >= stop_price`.
/// Stop sell triggers when `trade_price <= stop_price`.
///
/// Uses pessimistic fill — the **worse** price for the trader, simulating
/// real exchange behavior where a triggered stop becomes a market order
/// that fills at bid (sells) or ask (buys).
///
/// Returns `Some(fill_price)` if triggered, `None` otherwise.
pub fn check_stop_fill(
    side: OrderSide,
    stop_price: f64,
    bid: f64,
    ask: f64,
    trade_price: f64,
) -> Option<f64> {
    let triggered = match side {
        OrderSide::Buy => trade_price >= stop_price,
        OrderSide::Sell => trade_price <= stop_price,
    };
    if !triggered {
        return None;
    }
    Some(pessimistic_fill_price(side, bid, ask, trade_price))
}

/// Fill price for a market order. Uses pessimistic fill — worse for the trader.
pub fn market_fill_price(side: OrderSide, bid: f64, ask: f64, trade_price: f64) -> f64 {
    pessimistic_fill_price(side, bid, ask, trade_price)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pessimistic_buy_takes_max() {
        // ask=101, trade=99 → fill at 101 (worse for buyer = higher)
        assert_eq!(pessimistic_fill_price(OrderSide::Buy, 100.0, 101.0, 99.0), 101.0);
        // ask=101, trade=103 → fill at 103 (worse for buyer = higher)
        assert_eq!(pessimistic_fill_price(OrderSide::Buy, 100.0, 101.0, 103.0), 103.0);
    }

    #[test]
    fn pessimistic_sell_takes_min() {
        // bid=100, trade=102 → fill at 100 (worse for seller = lower)
        assert_eq!(pessimistic_fill_price(OrderSide::Sell, 100.0, 101.0, 102.0), 100.0);
        // bid=100, trade=98 → fill at 98 (worse for seller = lower)
        assert_eq!(pessimistic_fill_price(OrderSide::Sell, 100.0, 101.0, 98.0), 98.0);
    }

    #[test]
    fn limit_buy_triggers_at_or_below() {
        // Trade at limit → triggers
        assert!(check_limit_fill(OrderSide::Buy, 100.0, 100.0).is_some());
        // Trade below limit → triggers
        assert!(check_limit_fill(OrderSide::Buy, 100.0, 99.0).is_some());
        // Trade above limit → no trigger
        assert!(check_limit_fill(OrderSide::Buy, 100.0, 101.0).is_none());
    }

    #[test]
    fn limit_sell_triggers_at_or_above() {
        // Trade at limit → triggers
        assert!(check_limit_fill(OrderSide::Sell, 101.0, 101.0).is_some());
        // Trade above limit → triggers
        assert!(check_limit_fill(OrderSide::Sell, 101.0, 102.0).is_some());
        // Trade below limit → no trigger
        assert!(check_limit_fill(OrderSide::Sell, 101.0, 100.0).is_none());
    }

    #[test]
    fn stop_buy_triggers_at_or_above() {
        let bid = 100.0;
        let ask = 101.0;
        // Trade at stop → triggers
        assert!(check_stop_fill(OrderSide::Buy, 105.0, bid, ask, 105.0).is_some());
        // Trade above stop → triggers
        assert!(check_stop_fill(OrderSide::Buy, 105.0, bid, ask, 106.0).is_some());
        // Trade below stop → no trigger
        assert!(check_stop_fill(OrderSide::Buy, 105.0, bid, ask, 104.0).is_none());
    }

    #[test]
    fn stop_sell_triggers_at_or_below() {
        let bid = 100.0;
        let ask = 101.0;
        // Trade at stop → triggers
        assert!(check_stop_fill(OrderSide::Sell, 95.0, bid, ask, 95.0).is_some());
        // Trade below stop → triggers
        assert!(check_stop_fill(OrderSide::Sell, 95.0, bid, ask, 94.0).is_some());
        // Trade above stop → no trigger
        assert!(check_stop_fill(OrderSide::Sell, 95.0, bid, ask, 96.0).is_none());
    }

    #[test]
    fn limit_fill_uses_limit_price() {
        // Limit sell at 50000, trade hits 50100 → fills at limit price 50000
        let price = check_limit_fill(OrderSide::Sell, 50000.0, 50100.0).unwrap();
        assert_eq!(price, 50000.0);

        // Limit buy at 50000, trade drops to 49900 → fills at limit price 50000
        let price = check_limit_fill(OrderSide::Buy, 50000.0, 49900.0).unwrap();
        assert_eq!(price, 50000.0);
    }

    #[test]
    fn stop_fill_uses_pessimistic_price() {
        // Stop sell at 49000, trade drops to 48900, bid=48800
        // Pessimistic sell: min(bid=48800, trade=48900) = 48800 (worse for seller)
        let price = check_stop_fill(OrderSide::Sell, 49000.0, 48800.0, 49050.0, 48900.0).unwrap();
        assert_eq!(price, 48800.0);

        // Stop buy at 51000, trade rises to 51100, ask=51200
        // Pessimistic buy: max(ask=51200, trade=51100) = 51200 (worse for buyer)
        let price = check_stop_fill(OrderSide::Buy, 51000.0, 51050.0, 51200.0, 51100.0).unwrap();
        assert_eq!(price, 51200.0);
    }
}
