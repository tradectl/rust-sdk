use crate::types::enums::MarketType;

/// `PartialEq` so a metadata refresh can ask "did anything about this symbol
/// change?" in one comparison. Field-by-field checks at the call site go stale
/// the moment a field is added here — a `contract_size` change was invisible to
/// one such check, which is six orders of magnitude of notional on an inverse
/// pair.
#[derive(Debug, Clone, PartialEq)]
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
