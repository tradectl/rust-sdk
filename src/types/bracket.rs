/// One tier of a venue's notional ladder: the largest position notional the
/// venue allows while the symbol's leverage is at most `max_leverage`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BracketTier {
    pub max_leverage: u32,
    pub notional_cap: f64,
}

/// The notional cap in force at `leverage`: the cap of the tightest tier whose
/// `max_leverage` still covers it. `None` for an empty ladder. Above the top
/// tier the top tier's cap is returned — the venue refuses every order there
/// and the leverage clamp is the remedy, not fit.
pub fn notional_cap_at(ladder: &[BracketTier], leverage: f64) -> Option<f64> {
    let lev = leverage.max(1.0).ceil() as u32;
    ladder
        .iter()
        .filter(|t| t.max_leverage >= lev)
        .min_by_key(|t| t.max_leverage)
        .or_else(|| ladder.iter().max_by_key(|t| t.max_leverage))
        .map(|t| t.notional_cap)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ROBOUSDT, 2026-09-02 (public ladder): 25x $5k, 20x $10k, 10x $20k …
    fn robo() -> Vec<BracketTier> {
        [(25, 5_000.0), (20, 10_000.0), (10, 20_000.0), (5, 50_000.0), (4, 100_000.0),
         (3, 250_000.0), (2, 2_500_000.0), (1, 5_000_000.0)]
            .into_iter()
            .map(|(l, c)| BracketTier { max_leverage: l, notional_cap: c })
            .collect()
    }

    #[test]
    fn cap_is_the_tightest_tier_that_covers_the_leverage() {
        let l = robo();
        assert_eq!(notional_cap_at(&l, 20.0), Some(10_000.0));
        assert_eq!(notional_cap_at(&l, 25.0), Some(5_000.0));
        assert_eq!(notional_cap_at(&l, 15.0), Some(10_000.0), "15x sits in the 20x tier");
        assert_eq!(notional_cap_at(&l, 10.0), Some(20_000.0));
        assert_eq!(notional_cap_at(&l, 1.0), Some(5_000_000.0));
    }

    #[test]
    fn above_the_top_tier_is_the_top_tier() {
        assert_eq!(notional_cap_at(&robo(), 30.0), Some(5_000.0));
    }

    #[test]
    fn order_of_tiers_does_not_matter() {
        let mut l = robo();
        l.reverse();
        assert_eq!(notional_cap_at(&l, 20.0), Some(10_000.0));
    }

    #[test]
    fn empty_ladder_is_no_cap() {
        assert_eq!(notional_cap_at(&[], 20.0), None);
    }
}
