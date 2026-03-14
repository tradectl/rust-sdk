use crate::types::enums::Side;

#[derive(Debug, Clone)]
pub struct Trade {
    pub entry_price: f64,
    pub exit_price: f64,
    pub entry_time: u64,
    pub exit_time: u64,
    pub side: Side,
    pub quantity: f64,
    pub pnl: f64,
    pub pnl_pct: f64,
    pub fees: f64,
}
