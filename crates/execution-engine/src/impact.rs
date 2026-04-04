use alloy::{
    primitives::{Address, U256},
    providers::ProviderBuilder,
    sol,
};
use anyhow::{Context, Result};
use common::types::PoolType;

sol! {
    #[sol(rpc)]
    interface IUniswapV2Pair {
        function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
        function token0() external view returns (address);
        function token1() external view returns (address);
    }

    #[sol(rpc)]
    interface IUniswapV3Pool {
        function token0() external view returns (address);
        function token1() external view returns (address);
    }

    #[sol(rpc)]
    interface IERC20 {
        function balanceOf(address account) external view returns (uint256);
    }
}

/// Calculate price impact from live on-chain pool balances/reserves.
///
/// Uses a constant-product approximation:
///   price_impact_bps = (sell_amount * 10000) / (reserve_x + sell_amount)
/// where `reserve_x` is the pool-side reserve/balance of the sell token.
pub async fn calculate_price_impact(
    chain: &str,
    pool_address: &str,
    pool_type: PoolType,
    sell_token: &str,
    sell_amount: U256,
) -> Result<u32> {
    let settings = liquifier_config::Settings::global();
    let chain_cfg = settings
        .chains
        .get(chain)
        .with_context(|| format!("Missing chain config for {chain}"))?;
    let rpc_url = chain_cfg.rpc_url.trim();
    if rpc_url.is_empty() {
        anyhow::bail!("Missing rpc_url for chain {chain}");
    }

    let rpc_url: alloy::transports::http::reqwest::Url = rpc_url
        .parse()
        .with_context(|| format!("Invalid rpc_url configured for {chain}"))?;
    let provider = ProviderBuilder::new().connect_http(rpc_url);

    let pool: Address = pool_address
        .parse()
        .with_context(|| format!("Invalid pool address: {pool_address}"))?;
    let sell: Address = sell_token
        .parse()
        .with_context(|| format!("Invalid sell token address: {sell_token}"))?;

    let (token0, token1, reserve0, reserve1) = match pool_type {
        PoolType::V2 => {
            let pair = IUniswapV2Pair::new(pool, &provider);
            let token0 = Address::from(pair.token0().call().await?.0);
            let token1 = Address::from(pair.token1().call().await?.0);
            let reserves = pair.getReserves().call().await?;
            let reserve0 =
                U256::from_str_radix(&reserves.reserve0.to_string(), 10).unwrap_or(U256::ZERO);
            let reserve1 =
                U256::from_str_radix(&reserves.reserve1.to_string(), 10).unwrap_or(U256::ZERO);
            (token0, token1, reserve0, reserve1)
        }
        PoolType::V3 => {
            let pool_contract = IUniswapV3Pool::new(pool, &provider);
            let token0 = Address::from(pool_contract.token0().call().await?.0);
            let token1 = Address::from(pool_contract.token1().call().await?.0);

            let erc20_0 = IERC20::new(token0, &provider);
            let erc20_1 = IERC20::new(token1, &provider);

            let reserve0 = erc20_0
                .balanceOf(pool)
                .call()
                .await
                .context("Failed to read V3 token0 balance")?;
            let reserve1 = erc20_1
                .balanceOf(pool)
                .call()
                .await
                .context("Failed to read V3 token1 balance")?;
            (token0, token1, reserve0, reserve1)
        }
    };

    let (reserve_x, reserve_y) = if sell == token0 {
        (reserve0, reserve1)
    } else if sell == token1 {
        (reserve1, reserve0)
    } else {
        anyhow::bail!(
            "Sell token {} does not match pool tokens {} / {}",
            sell,
            token0,
            token1
        );
    };

    Ok(calculate_impact_from_reserves(
        reserve_x,
        reserve_y,
        sell_amount,
    ))
}

/// Pure calculation: price impact in basis points from reserves.
///
/// For constant-product AMM (x * y = k):
///   price_impact_bps = (sell_amount * 10000) / (reserve_x + sell_amount)
pub fn calculate_impact_from_reserves(reserve_x: U256, reserve_y: U256, sell_amount: U256) -> u32 {
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
        assert!(
            impact > 800 && impact < 1000,
            "Expected ~909 bps, got {impact}"
        );
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

    #[test]
    fn test_zero_reserve_x() {
        let impact = calculate_impact_from_reserves(
            U256::ZERO,
            U256::from(500_000u64),
            U256::from(1_000u64),
        );
        assert_eq!(impact, 0);
    }

    #[test]
    fn test_zero_reserve_y() {
        let impact = calculate_impact_from_reserves(
            U256::from(1_000_000u64),
            U256::ZERO,
            U256::from(1_000u64),
        );
        assert_eq!(impact, 0);
    }

    #[test]
    fn test_very_large_trade() {
        // Selling 100x the reserve → ~9901 bps (99.01%)
        let impact = calculate_impact_from_reserves(
            U256::from(1_000u64),
            U256::from(1_000u64),
            U256::from(100_000u64),
        );
        assert!(impact > 9800, "Expected >9800 bps, got {impact}");
    }

    #[test]
    fn test_tiny_trade() {
        // 1 wei into a huge pool
        let impact = calculate_impact_from_reserves(
            U256::from(10u64).pow(U256::from(18)),
            U256::from(10u64).pow(U256::from(18)),
            U256::from(1u64),
        );
        assert_eq!(impact, 0);
    }
}
