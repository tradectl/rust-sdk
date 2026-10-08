//! What the user's plan allows, as the engine sees it.
//!
//! The CLI verifies the signed license and keeps this up to date over a
//! `watch` channel (`docs/paying.md`, section 3.1). The engine only reads it:
//! per strategy, when an entry is sized. Paper strategies never look at it.

use serde::{Deserialize, Serialize};
use std::fmt;
use tokio::sync::watch;

/// Plan tier. Ordered so `tier >= Tier::Starter` reads naturally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Free,
    Starter,
    Pro,
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tier::Free => write!(f, "Free"),
            Tier::Starter => write!(f, "Starter"),
            Tier::Pro => write!(f, "Pro"),
        }
    }
}

/// The Free list when the server sent none: BTC, ETH, XRP, SOL, BNB perpetuals on
/// both futures markets. A license without `limits` is never more generous than this.
pub const FREE_SYMBOLS: [&str; 10] = [
    "BTCUSD_PERP", "ETHUSD_PERP", "XRPUSD_PERP", "SOLUSD_PERP", "BNBUSD_PERP",
    "BTCUSDT", "ETHUSDT", "XRPUSDT", "SOLUSDT", "BNBUSDT",
];

/// The Free order cap in USD when the server sent none.
pub const FREE_MAX_ORDER_USD: f64 = 20.0;

/// The plan as it applies to THIS bot right now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LicenseState {
    pub tier: Tier,
    /// End of the prepaid period, unix seconds. `None` on Free.
    pub paid_until: Option<i64>,
    /// This bot holds a paid slot: no symbol list, no order cap.
    pub slot: bool,
    /// Live symbols allowed without a slot.
    pub free_symbols: Vec<String>,
    /// Live order value allowed without a slot, USD (the venue minimum wins if larger).
    pub free_max_order_usd: f64,
    /// Why the Free limits apply, for the log and the Lab. `None` with a slot.
    pub reason: Option<String>,
}

impl LicenseState {
    /// Free limits, with the built-in list: what a bot without any license gets.
    pub fn free(reason: impl Into<String>) -> Self {
        Self {
            tier: Tier::Free,
            paid_until: None,
            slot: false,
            free_symbols: FREE_SYMBOLS.iter().map(|s| s.to_string()).collect(),
            free_max_order_usd: FREE_MAX_ORDER_USD,
            reason: Some(reason.into()),
        }
    }

    /// A paid slot with no limits: tests and code-built configs.
    pub fn unlimited(tier: Tier) -> Self {
        Self {
            tier,
            paid_until: None,
            slot: true,
            free_symbols: Vec::new(),
            free_max_order_usd: 0.0,
            reason: None,
        }
    }

    /// Free limits apply to live strategies of this bot.
    pub fn limited(&self) -> bool {
        !self.slot
    }

    /// A live entry on `symbol` may be placed.
    pub fn allows_symbol(&self, symbol: &str) -> bool {
        self.slot || self.free_symbols.iter().any(|s| s == symbol)
    }
}

/// `None` = no license at all (not logged in): live strategies get Free limits.
pub type LicenseRx = watch::Receiver<Option<LicenseState>>;
pub type LicenseTx = watch::Sender<Option<LicenseState>>;

/// "A live strategy is running" — set by the engine as strategies start and stop,
/// read by the CLI's license thread to know when to check in.
pub type LiveTx = watch::Sender<bool>;
pub type LiveRx = watch::Receiver<bool>;

/// A fixed license for tests, replay and code-built configs: the receiver never
/// changes and the live flag goes nowhere.
pub fn fixed(state: Option<LicenseState>) -> (LicenseRx, LiveTx) {
    let (tx, rx) = watch::channel(state);
    std::mem::forget(tx);
    let (live_tx, _live_rx) = watch::channel(false);
    (rx, live_tx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_order_and_names() {
        assert!(Tier::Free < Tier::Starter && Tier::Starter < Tier::Pro);
        assert_eq!(serde_json::to_string(&Tier::Starter).unwrap(), "\"starter\"");
        assert_eq!(Tier::Pro.to_string(), "Pro");
    }

    #[test]
    fn free_state_allows_only_the_list() {
        let s = LicenseState::free("not logged in");
        assert!(s.limited());
        assert!(s.allows_symbol("BTCUSD_PERP"));
        assert!(s.allows_symbol("XRPUSDT"));
        assert!(!s.allows_symbol("ADAUSD_PERP"));
        assert!(!s.allows_symbol("XRPUSD_260925"));
        assert_eq!(s.free_max_order_usd, 20.0);
    }

    #[test]
    fn slot_allows_everything() {
        let s = LicenseState::unlimited(Tier::Starter);
        assert!(!s.limited());
        assert!(s.allows_symbol("ADAUSD_PERP"));
    }

    #[test]
    fn fixed_receiver_holds_its_value() {
        let (rx, live) = fixed(None);
        assert!(rx.borrow().is_none());
        live.send_replace(true); // nobody listens, must not panic
    }
}
