use crate::types::enums::OrderSide;

#[derive(Debug, Clone, Copy)]
pub struct MarketFees {
    pub maker_rate: f64,
    pub taker_rate: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProfitResult {
    /// ROI % accounting for leverage (net_pnl / initial_margin).
    pub profit: f64,
    /// ROI % without leverage (net_pnl / notional).
    pub profit_raw: f64,
    pub profit_usd: f64,
    pub fees: f64,
}

pub struct LinearProfitParams {
    pub side: OrderSide,
    pub entry_price: f64,
    pub exit_price: f64,
    pub quantity: f64,
    pub leverage: f64,
    pub fees: MarketFees,
}

pub struct InverseProfitParams {
    pub side: OrderSide,
    pub entry_price: f64,
    pub exit_price: f64,
    pub quantity: f64,
    pub leverage: f64,
    pub contract_size: f64,
    pub fees: MarketFees,
}

pub struct SpotProfitParams {
    pub side: OrderSide,
    pub entry_price: f64,
    pub exit_price: f64,
    pub quantity: f64,
    pub fees: MarketFees,
}

fn direction(side: OrderSide) -> f64 {
    match side {
        OrderSide::Buy => 1.0,
        OrderSide::Sell => -1.0,
    }
}

/// Calculate profit for USDT-margined (linear) futures.
///
/// Returns ROI % as `profit`, net PnL in USD as `profit_usd`, and total fees.
pub fn calculate_linear_profit(p: &LinearProfitParams) -> ProfitResult {
    let dir = direction(p.side);
    let pnl = dir * p.quantity * (p.exit_price - p.entry_price);

    let entry_fee = (p.quantity * p.entry_price).abs() * p.fees.maker_rate;
    let exit_fee = (p.quantity * p.exit_price).abs() * p.fees.taker_rate;
    let total_fees = entry_fee + exit_fee;

    let net_pnl = pnl - total_fees;
    let notional = p.quantity * p.entry_price;
    let initial_margin = notional / p.leverage;
    let roi = if initial_margin == 0.0 {
        0.0
    } else {
        net_pnl / initial_margin * 100.0
    };
    let roi_raw = if notional == 0.0 {
        0.0
    } else {
        net_pnl / notional * 100.0
    };

    ProfitResult {
        profit: roi,
        profit_raw: roi_raw,
        profit_usd: net_pnl,
        fees: total_fees,
    }
}

/// Calculate profit for coin-margined (inverse) futures.
///
/// PnL is computed in coin terms, then converted to USD at exit price.
pub fn calculate_inverse_profit(p: &InverseProfitParams) -> ProfitResult {
    if p.entry_price <= 0.0 || p.exit_price <= 0.0 {
        return ProfitResult::default();
    }
    let dir = direction(p.side);
    let inv_entry = 1.0 / p.entry_price;
    let inv_exit = 1.0 / p.exit_price;
    let pnl_coin = dir * p.contract_size * p.quantity * (inv_entry - inv_exit);

    let notional_entry = p.contract_size * p.quantity / p.entry_price;
    let notional_exit = p.contract_size * p.quantity / p.exit_price;
    let entry_fee = notional_entry.abs() * p.fees.maker_rate;
    let exit_fee = notional_exit.abs() * p.fees.taker_rate;
    let total_fees_coin = entry_fee + exit_fee;

    let net_pnl_coin = pnl_coin - total_fees_coin;
    let net_pnl_usd = net_pnl_coin * p.exit_price;
    let total_fees_usd = total_fees_coin * p.exit_price;

    let notional_margin = p.contract_size * p.quantity / p.entry_price;
    let initial_margin = notional_margin / p.leverage;
    let roi = if initial_margin == 0.0 {
        0.0
    } else {
        net_pnl_coin / initial_margin * 100.0
    };
    let roi_raw = if notional_margin == 0.0 {
        0.0
    } else {
        net_pnl_coin / notional_margin * 100.0
    };

    ProfitResult {
        profit: roi,
        profit_raw: roi_raw,
        profit_usd: net_pnl_usd,
        fees: total_fees_usd,
    }
}

/// Calculate profit for spot trades.
///
/// ROI is based on initial position value (quantity * entry_price).
pub fn calculate_spot_profit(p: &SpotProfitParams) -> ProfitResult {
    let dir = direction(p.side);
    let pnl = dir * p.quantity * (p.exit_price - p.entry_price);

    let entry_fee = (p.quantity * p.entry_price).abs() * p.fees.maker_rate;
    let exit_fee = (p.quantity * p.exit_price).abs() * p.fees.taker_rate;
    let total_fees = entry_fee + exit_fee;

    let net_pnl = pnl - total_fees;
    let initial_value = p.quantity * p.entry_price;
    let roi = if initial_value == 0.0 {
        0.0
    } else {
        net_pnl / initial_value * 100.0
    };

    ProfitResult {
        profit: roi,
        profit_raw: roi,
        profit_usd: net_pnl,
        fees: total_fees,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_FEES: MarketFees = MarketFees {
        maker_rate: 0.0002,
        taker_rate: 0.0004,
    };

    #[test]
    fn linear_long_profit() {
        let r = calculate_linear_profit(&LinearProfitParams {
            side: OrderSide::Buy,
            entry_price: 50000.0,
            exit_price: 51000.0,
            quantity: 1.0,
            leverage: 10.0,
            fees: TEST_FEES,
        });
        assert!((r.profit_usd - 969.6).abs() < 0.01);
        assert!((r.profit - 19.392).abs() < 0.01);
        assert!((r.fees - 30.4).abs() < 0.01);
    }

    #[test]
    fn linear_short_profit() {
        let r = calculate_linear_profit(&LinearProfitParams {
            side: OrderSide::Sell,
            entry_price: 50000.0,
            exit_price: 49000.0,
            quantity: 1.0,
            leverage: 10.0,
            fees: TEST_FEES,
        });
        assert!((r.profit_usd - 970.4).abs() < 0.01);
        assert!(r.profit > 0.0);
    }

    #[test]
    fn linear_long_loss() {
        let r = calculate_linear_profit(&LinearProfitParams {
            side: OrderSide::Buy,
            entry_price: 50000.0,
            exit_price: 49000.0,
            quantity: 1.0,
            leverage: 10.0,
            fees: TEST_FEES,
        });
        assert!((r.profit_usd - (-1029.6)).abs() < 0.01);
        assert!(r.profit < 0.0);
    }

    #[test]
    fn inverse_long_profit() {
        let r = calculate_inverse_profit(&InverseProfitParams {
            side: OrderSide::Buy,
            entry_price: 50000.0,
            exit_price: 51000.0,
            quantity: 100.0,
            leverage: 10.0,
            contract_size: 100.0,
            fees: TEST_FEES,
        });
        assert!(r.profit_usd > 0.0);
        assert!(r.profit > 0.0);
        assert!(r.fees > 0.0);
    }

    #[test]
    fn inverse_short_loss() {
        let r = calculate_inverse_profit(&InverseProfitParams {
            side: OrderSide::Sell,
            entry_price: 50000.0,
            exit_price: 51000.0,
            quantity: 100.0,
            leverage: 10.0,
            contract_size: 100.0,
            fees: TEST_FEES,
        });
        assert!(r.profit_usd < 0.0);
        assert!(r.profit < 0.0);
    }

    #[test]
    fn spot_buy_profit() {
        let r = calculate_spot_profit(&SpotProfitParams {
            side: OrderSide::Buy,
            entry_price: 100.0,
            exit_price: 110.0,
            quantity: 10.0,
            fees: TEST_FEES,
        });
        assert!((r.profit_usd - 99.36).abs() < 0.01);
        assert!((r.profit - 9.936).abs() < 0.01);
        assert!((r.fees - 0.64).abs() < 0.01);
    }

    #[test]
    fn zero_quantity_returns_zero() {
        let r = calculate_linear_profit(&LinearProfitParams {
            side: OrderSide::Buy,
            entry_price: 50000.0,
            exit_price: 51000.0,
            quantity: 0.0,
            leverage: 10.0,
            fees: TEST_FEES,
        });
        assert_eq!(r.profit, 0.0);
        assert_eq!(r.profit_usd, 0.0);
        assert_eq!(r.fees, 0.0);
    }
}
