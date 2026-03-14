use crate::types::enums::MarketType;

#[derive(Debug, Clone)]
pub struct PairInfo {
    pub symbol: String,
    pub display_name: String,
    pub market_type: MarketType,
    pub price_step: f64,
    pub quantity_step: f64,
    pub price_precision: u32,
    pub quantity_precision: u32,
    pub min_quantity: f64,
    pub max_quantity: f64,
    pub min_notional: f64,
    pub contract_size: f64,
}
