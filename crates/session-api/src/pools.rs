use alloy::{primitives::Address, providers::Provider, sol};
use anyhow::Result;
use common::types::{base_tokens_for_chain, dex_factories_for_chain, DexFactoryConfig, PoolType};
use tracing::{info, warn};

// ABI fragments for V2 and V3 factory getPair/getPool calls
sol! {
    #[sol(rpc)]
    interface IUniswapV2Factory {
        function getPair(address tokenA, address tokenB) external view returns (address pair);
    }

    #[sol(rpc)]
    interface IUniswapV3Factory {
        function getPool(address tokenA, address tokenB, uint24 fee) external view returns (address pool);
    }

    #[sol(rpc)]
    interface IUniswapV2Pair {
        function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
        function token0() external view returns (address);
        function token1() external view returns (address);
    }

    #[sol(rpc)]
    interface IUniswapV3Pool {
        function liquidity() external view returns (uint128);
    }

    #[sol(rpc)]
    interface IERC20 {
        function balanceOf(address account) external view returns (uint256);
        function decimals() external view returns (uint8);
    }
}

#[derive(Debug, Clone)]
pub struct DiscoveredPool {
    pub pool_address: String,
    pub pool_type: PoolType,
    pub dex_name: String,
    pub token0: String,
    pub token1: String,
    pub fee_tier: u32,
    pub reserve0: String,
    pub reserve1: String,
    pub liquidity: String,
    pub balance0: String,
    pub balance1: String,
    pub token0_price_usd: f64,
    pub token1_price_usd: f64,
}

/// Discover all pools containing `token_address` across pre-configured DEX factories for the chain.
/// Pairs with common base tokens (WETH, USDC, USDT, WBTC, DAI) to maximize coverage.
pub async fn discover_pools<P: Provider + Clone>(
    provider: P,
    chain: &str,
    token_address: &str,
) -> Result<Vec<DiscoveredPool>> {
    let token: Address = token_address.parse()?;
    let base_tokens = base_tokens_for_chain(chain);
    let factories = dex_factories_for_chain(chain);

    let mut pools = Vec::new();

    for factory in &factories {
        let factory_addr: Address = factory.factory_address.parse()?;
        // Query each base token pairing
        for base in &base_tokens {
            if base.eq_ignore_ascii_case(token_address) {
                continue; // skip pairing token with itself
            }
            let base_addr: Address = base.parse()?;

            match factory.pool_type {
                PoolType::V2 => {
                    match discover_v2_pool(&provider, factory_addr, token, base_addr, factory).await
                    {
                        Ok(Some(pool)) => pools.push(pool),
                        Ok(None) => {}
                        Err(e) => {
                            warn!(
                                factory = factory.name,
                                base = %base,
                                error = %e,
                                "V2 pool discovery failed"
                            );
                        }
                    }
                }
                PoolType::V3 => {
                    for &fee in &factory.fee_tiers {
                        match discover_v3_pool(
                            &provider,
                            factory_addr,
                            token,
                            base_addr,
                            fee,
                            factory,
                        )
                        .await
                        {
                            Ok(Some(pool)) => pools.push(pool),
                            Ok(None) => {}
                            Err(e) => {
                                warn!(
                                    factory = factory.name,
                                    fee_tier = fee,
                                    base = %base,
                                    error = %e,
                                    "V3 pool discovery failed"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    // Deduplicate by pool address (case-insensitive)
    pools.sort_by(|a, b| {
        a.pool_address
            .to_lowercase()
            .cmp(&b.pool_address.to_lowercase())
    });
    pools.dedup_by(|a, b| a.pool_address.eq_ignore_ascii_case(&b.pool_address));

    info!(
        chain = chain,
        token = token_address,
        count = pools.len(),
        "Pool discovery complete"
    );

    Ok(pools)
}

async fn discover_v2_pool<P: Provider + Clone>(
    provider: &P,
    factory_addr: Address,
    token_a: Address,
    token_b: Address,
    factory: &DexFactoryConfig,
) -> Result<Option<DiscoveredPool>> {
    let factory_contract = IUniswapV2Factory::new(factory_addr, provider.clone());
    let pair = factory_contract.getPair(token_a, token_b).call().await?;
    let pair_addr = Address::from(pair.0);

    if pair_addr == Address::ZERO {
        return Ok(None);
    }

    // Fetch actual token0/token1 ordering from the pair contract
    let pair_contract = IUniswapV2Pair::new(pair_addr, provider.clone());
    let actual_token0 = Address::from(pair_contract.token0().call().await?.0);
    let actual_token1 = Address::from(pair_contract.token1().call().await?.0);

    // Fetch reserves
    let (reserve0, reserve1) = match pair_contract.getReserves().call().await {
        Ok(res) => (res.reserve0.to_string(), res.reserve1.to_string()),
        Err(e) => {
            warn!(pool = %pair_addr, error = %e, "Failed to fetch V2 reserves");
            (String::new(), String::new())
        }
    };

    Ok(Some(DiscoveredPool {
        pool_address: format!("{pair_addr:?}"),
        pool_type: PoolType::V2,
        dex_name: factory.name.clone(),
        token0: format!("{actual_token0:?}"),
        token1: format!("{actual_token1:?}"),
        fee_tier: 0,
        reserve0,
        reserve1,
        liquidity: String::new(),
        balance0: String::new(),
        balance1: String::new(),
    }))
}

async fn discover_v3_pool<P: Provider + Clone>(
    provider: &P,
    factory_addr: Address,
    token_a: Address,
    token_b: Address,
    fee: u32,
    factory: &DexFactoryConfig,
) -> Result<Option<DiscoveredPool>> {
    let factory_contract = IUniswapV3Factory::new(factory_addr, provider.clone());
    let pool = factory_contract
        .getPool(token_a, token_b, fee.try_into()?)
        .call()
        .await?;
    let pool_addr = Address::from(pool.0);

    if pool_addr == Address::ZERO {
        return Ok(None);
    }

    // Fetch liquidity
    let liquidity = match IUniswapV3Pool::new(pool_addr, provider.clone())
        .liquidity()
        .call()
        .await
    {
        Ok(liq) => liq.to_string(),
        Err(e) => {
            warn!(pool = %pool_addr, error = %e, "Failed to fetch V3 liquidity");
            String::new()
        }
    };

    // Fetch token balances in pool
    let balance0 = match IERC20::new(token_a, provider.clone())
        .balanceOf(Address::from(pool_addr))
        .call()
        .await
    {
        Ok(b) => b.to_string(),
        Err(_) => String::new(),
    };
    let balance1 = match IERC20::new(token_b, provider.clone())
        .balanceOf(Address::from(pool_addr))
        .call()
        .await
    {
        Ok(b) => b.to_string(),
        Err(_) => String::new(),
    };

    Ok(Some(DiscoveredPool {
        pool_address: format!("{pool_addr:?}"),
        pool_type: PoolType::V3,
        dex_name: factory.name.clone(),
        token0: format!("{token_a:?}"),
        token1: format!("{token_b:?}"),
        fee_tier: fee,
        reserve0: String::new(),
        reserve1: String::new(),
        liquidity,
        balance0,
        balance1,
    }))
}
