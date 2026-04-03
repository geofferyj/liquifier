use alloy::primitives::U256;

/// Calculate price impact for a Uniswap V2-style constant product pool.
///
/// Uses the x * y = k formula:
///   - Given reserves (x, y) and sell amount Δx
///   - Output Δy = (y * Δx) / (x + Δx)
///   - Price impact = 1 - (Δy / Δx) / (y / x)
///     = Δx / (x + Δx)  (simplified)
///
/// Returns price impact in basis points (1 bps = 0.01%).
pub async fn calculate_price_impact_v2(
    _pool_address: &str,
    sell_amount: U256,
) -> u32 {
    // In production, fetch actual reserves from the pool contract:
    //   let (reserve_x, reserve_y) = pool.getReserves().call().await;
    //
    // Placeholder reserves for scaffolding — production reads on-chain
    let reserve_x = U256::from(1_000_000u64) * U256::from(10u64).pow(U256::from(18u64)); // 1M tokens
    let reserve_y = U256::from(500_000u64) * U256::from(10u64).pow(U256::from(18u64));   // 500K tokens

    calculate_impact_from_reserves(reserve_x, reserve_y, sell_amount)
}

/// Pure calculation: price impact in basis points from reserves.
///
/// For constant-product AMM (x * y = k):
///   price_impact_bps = (sell_amount * 10000) / (reserve_x + sell_amount)
pub fn calculate_impact_from_reserves(
    reserve_x: U256,
    reserve_y: U256,
    sell_amount: U256,
) -> u32 {
    if reserve_x.is_zero() || reserve_y.is_zero() || sell_amount.is_zero() {
        return 0;
    }

    // impact_bps = sell_amount * 10000 / (reserve_x + sell_amount)
    let denominator = reserve_x.saturating_add(sell_amount);
    if denominator.is_zero() {
        return 0;
    }

    let numerator = sell_amount.saturating_mul(U256::from(10_000u64));
    let impact = numerator / denominator;

    // Clamp to u32 (max 10000 bps = 100%)
    impact.try_into().unwrap_or(10_000u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zero_sell_amount() {
        let impact = calculate_impact_from_reserves(
            U256::from(1_000_000u64),
            U256::from(500_000u64),
            U256::ZERO,
        );
        assert_eq!(impact, 0);
    }

    #[test]
    fn test_small_trade_low_impact() {
        // Selling 1000 into a 1M pool → ~10 bps
        let impact = calculate_impact_from_reserves(
            U256::from(1_000_000u64),
            U256::from(500_000u64),
            U256::from(1_000u64),
        );
        assert!(impact <= 10, "Expected ≤10 bps, got {impact}");
    }

    #[test]
    fn test_large_trade_high_impact() {
        // Selling 100K into a 1M pool → ~909 bps (9.09%)
        let impact = calculate_impact_from_reserves(
            U256::from(1_000_000u64),
            U256::from(500_000u64),
            U256::from(100_000u64),
        );
        assert!(impact > 800 && impact < 1000, "Expected ~909 bps, got {impact}");
    }

    #[test]
    fn test_equal_to_reserves() {
        // Selling 1M into a 1M pool → 5000 bps (50%)
        let impact = calculate_impact_from_reserves(
            U256::from(1_000_000u64),
            U256::from(500_000u64),
            U256::from(1_000_000u64),
        );
        assert_eq!(impact, 5000);
    }
}
