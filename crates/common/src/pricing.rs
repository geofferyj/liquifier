use std::collections::HashMap;
use std::sync::Arc;

use alloy::primitives::{Address, I256};
use alloy::providers::ProviderBuilder;
use alloy::sol;
use async_trait::async_trait;
use dashmap::DashMap;
use tokio::time::{interval, Duration};
use tracing::{debug, warn};

use liquifier_config::Settings;

// ─────────────────────────────────────────────────────────────
// Chainlink AggregatorV3 interface (only latestRoundData + decimals)
// ─────────────────────────────────────────────────────────────

sol! {
    #[sol(rpc)]
    interface IAggregatorV3 {
        function latestRoundData() external view returns (
            uint80 roundId,
            int256 answer,
            uint256 startedAt,
            uint256 updatedAt,
            uint80 answeredInRound
        );
        function decimals() external view returns (uint8);
    }
}

// ─────────────────────────────────────────────────────────────
// Price fetcher trait (for testability)
// ─────────────────────────────────────────────────────────────

/// Trait for fetching USD prices for a set of token addresses on a chain.
#[async_trait]
pub trait PriceFetcher: Send + Sync {
    /// Fetch USD prices for the given (token_address, chainlink_oracle) pairs.
    /// Returns a map of lowercase(token_address) → USD price.
    async fn fetch_usd_prices(
        &self,
        chain: &str,
        rpc_url: &str,
        tokens: &[(String, String)], // (token_address, oracle_address)
    ) -> anyhow::Result<HashMap<String, f64>>;
}

// ─────────────────────────────────────────────────────────────
// Price cache
// ─────────────────────────────────────────────────────────────

/// Thread-safe cache of token USD prices, keyed by (chain, token_address).
#[derive(Clone)]
pub struct PriceCache {
    /// (chain_name, lowercase_address) → USD price per token
    prices: Arc<DashMap<(String, String), f64>>,
}

impl PriceCache {
    pub fn new() -> Self {
        Self {
            prices: Arc::new(DashMap::new()),
        }
    }

    /// Set the USD price for a token.
    pub fn set_price(&self, chain: &str, token_address: &str, usd_price: f64) {
        self.prices.insert(
            (chain.to_lowercase(), token_address.to_lowercase()),
            usd_price,
        );
    }

    /// Look up the cached USD price of a base token.
    /// Returns `None` if the token has no cached price.
    pub fn get_base_token_price(&self, chain: &str, token_address: &str) -> Option<f64> {
        self.prices
            .get(&(chain.to_lowercase(), token_address.to_lowercase()))
            .map(|v| *v)
    }

    /// Check whether a token is a known base token with a cached price.
    pub fn is_base_token(&self, chain: &str, token_address: &str) -> bool {
        self.prices
            .contains_key(&(chain.to_lowercase(), token_address.to_lowercase()))
    }

    /// Compute the USD value of `token_amount` of `token_address` on `chain`.
    ///
    /// Strategy:
    /// 1. If `token_address` is a base token → `amount × base_price`.
    /// 2. Otherwise, use the pool's other token (`other_token`) which must be a
    ///    base token. Derive token's USD price from `exchange_rate` (units of
    ///    `other_token` per 1 unit of `token_address`), then multiply.
    ///
    /// `exchange_rate` = how many `other_token` you get for 1 unit of `token_address`.
    /// e.g. if swapping 100 TOKEN for 0.05 WETH, exchange_rate = 0.05/100 = 0.0005
    pub fn token_amount_usd(
        &self,
        chain: &str,
        token_address: &str,
        token_amount: f64,
        other_token: &str,
        exchange_rate: f64,
    ) -> Option<f64> {
        // Case 1: token itself is a base token with a known price
        if let Some(price) = self.get_base_token_price(chain, token_address) {
            return Some(token_amount * price);
        }

        // Case 2: derive price from the other token in the pool
        let other_price = self.get_base_token_price(chain, other_token)?;
        let derived_price = exchange_rate * other_price;
        Some(token_amount * derived_price)
    }

    /// Convenience: compute USD value directly from swap amounts.
    ///
    /// Given a pool with `token_a` / `token_b`, and amounts swapped,
    /// tries to price whichever side has a known base-token price.
    ///
    /// Returns `(usd_value_a, usd_value_b)` if at least one side is a base token.
    pub fn swap_usd_values(
        &self,
        chain: &str,
        token_a: &str,
        amount_a: f64,
        token_b: &str,
        amount_b: f64,
    ) -> Option<(f64, f64)> {
        // Try token_a as base token
        if let Some(price_a) = self.get_base_token_price(chain, token_a) {
            let usd_a = amount_a * price_a;
            // Derive token_b price from the swap ratio
            let usd_b = if amount_b > 0.0 {
                let price_b = (amount_a / amount_b) * price_a;
                amount_b * price_b
            } else {
                0.0
            };
            return Some((usd_a, usd_b));
        }

        // Try token_b as base token
        if let Some(price_b) = self.get_base_token_price(chain, token_b) {
            let usd_b = amount_b * price_b;
            let usd_a = if amount_a > 0.0 {
                let price_a = (amount_b / amount_a) * price_b;
                amount_a * price_a
            } else {
                0.0
            };
            return Some((usd_a, usd_b));
        }

        None
    }

    /// Spawn a background task that refreshes base-token prices every
    /// `interval_secs` seconds using the provided `PriceFetcher`.
    ///
    /// Reads the enabled chains and their base_tokens from global config.
    pub fn start_updater(
        self,
        fetcher: Arc<dyn PriceFetcher>,
        interval_secs: u64,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut tick = interval(Duration::from_secs(interval_secs));
            loop {
                tick.tick().await;
                self.refresh_all_prices(&fetcher).await;
            }
        })
    }

    /// One-shot refresh of all base-token prices from config.
    pub async fn refresh_all_prices(&self, fetcher: &Arc<dyn PriceFetcher>) {
        let settings = Settings::global();
        for (chain_name, chain_cfg) in &settings.chains {
            if !chain_cfg.enabled || chain_cfg.base_tokens.is_empty() {
                continue;
            }
            // Only include tokens that have a chainlink oracle configured
            let tokens: Vec<(String, String)> = chain_cfg
                .base_tokens
                .iter()
                .filter(|t| !t.chainlink_oracle.is_empty())
                .map(|t| (t.address.clone(), t.chainlink_oracle.clone()))
                .collect();

            if tokens.is_empty() {
                continue;
            }

            let rpc_url = &chain_cfg.rpc_url;
            if rpc_url.is_empty() {
                warn!(chain = %chain_name, "no rpc_url configured, skipping price update");
                continue;
            }

            match fetcher.fetch_usd_prices(chain_name, rpc_url, &tokens).await {
                Ok(prices) => {
                    for (addr, price) in prices {
                        debug!(chain = %chain_name, token = %addr, price, "updated price");
                        self.set_price(chain_name, &addr, price);
                    }
                }
                Err(e) => {
                    warn!(chain = %chain_name, error = %e, "failed to fetch prices");
                }
            }
        }
    }
}

impl Default for PriceCache {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────
// Chainlink on-chain price fetcher
// ─────────────────────────────────────────────────────────────

/// Reads USD prices from Chainlink AggregatorV3 price feeds on-chain.
pub struct ChainlinkPriceFetcher;

impl ChainlinkPriceFetcher {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl PriceFetcher for ChainlinkPriceFetcher {
    async fn fetch_usd_prices(
        &self,
        chain: &str,
        rpc_url: &str,
        tokens: &[(String, String)],
    ) -> anyhow::Result<HashMap<String, f64>> {
        let provider = ProviderBuilder::new().connect_http(rpc_url.parse()?);
        let mut result = HashMap::new();

        for (token_address, oracle_address) in tokens {
            let oracle_addr: Address = oracle_address.parse().map_err(|_| {
                anyhow::anyhow!(
                    "invalid oracle address {} for token {} on {}",
                    oracle_address,
                    token_address,
                    chain
                )
            })?;

            let feed = IAggregatorV3::new(oracle_addr, &provider);

            let decimals = match feed.decimals().call().await {
                Ok(d) => d,
                Err(e) => {
                    warn!(
                        chain = %chain,
                        oracle = %oracle_address,
                        token = %token_address,
                        error = %e,
                        "failed to read oracle decimals"
                    );
                    continue;
                }
            };

            let round_data = match feed.latestRoundData().call().await {
                Ok(d) => d,
                Err(e) => {
                    warn!(
                        chain = %chain,
                        oracle = %oracle_address,
                        token = %token_address,
                        error = %e,
                        "failed to read latestRoundData"
                    );
                    continue;
                }
            };

            let answer = round_data.answer;
            if answer <= I256::ZERO {
                warn!(
                    chain = %chain,
                    token = %token_address,
                    "chainlink returned non-positive price, skipping"
                );
                continue;
            }

            let divisor = 10_f64.powi(decimals as i32);
            // Convert I256 → i64 (safe for prices that fit)
            let price_raw: i64 = answer.as_i64();
            let price = price_raw as f64 / divisor;
            result.insert(token_address.to_lowercase(), price);
        }

        Ok(result)
    }
}

// ─────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A mock price fetcher for testing.
    struct MockPriceFetcher {
        prices: HashMap<String, f64>,
    }

    #[allow(dead_code)]
    impl MockPriceFetcher {
        fn new(prices: Vec<(&str, f64)>) -> Self {
            Self {
                prices: prices
                    .into_iter()
                    .map(|(a, p)| (a.to_lowercase(), p))
                    .collect(),
            }
        }
    }

    #[async_trait]
    impl PriceFetcher for MockPriceFetcher {
        async fn fetch_usd_prices(
            &self,
            _chain: &str,
            _rpc_url: &str,
            tokens: &[(String, String)],
        ) -> anyhow::Result<HashMap<String, f64>> {
            let mut result = HashMap::new();
            for (addr, _oracle) in tokens {
                if let Some(&price) = self.prices.get(&addr.to_lowercase()) {
                    result.insert(addr.to_lowercase(), price);
                }
            }
            Ok(result)
        }
    }

    #[test]
    fn test_set_and_get_price() {
        let cache = PriceCache::new();
        cache.set_price("ethereum", "0xABCD", 1500.0);
        assert_eq!(
            cache.get_base_token_price("ethereum", "0xabcd"),
            Some(1500.0)
        );
        // Case-insensitive
        assert_eq!(
            cache.get_base_token_price("ETHEREUM", "0xAbCd"),
            Some(1500.0)
        );
    }

    #[test]
    fn test_is_base_token() {
        let cache = PriceCache::new();
        cache.set_price("ethereum", "0xWETH", 1500.0);
        assert!(cache.is_base_token("ethereum", "0xweth"));
        assert!(!cache.is_base_token("ethereum", "0xRandom"));
    }

    #[test]
    fn test_token_amount_usd_base_token() {
        let cache = PriceCache::new();
        cache.set_price("ethereum", "0xWETH", 2000.0);

        // Direct base token pricing
        let usd = cache.token_amount_usd("ethereum", "0xWETH", 1.5, "0xOther", 0.0);
        assert_eq!(usd, Some(3000.0));
    }

    #[test]
    fn test_token_amount_usd_derived() {
        let cache = PriceCache::new();
        cache.set_price("ethereum", "0xWETH", 2000.0);

        // Unknown token, but other side is WETH
        // exchange_rate = 0.0005 WETH per TOKEN → token price = 0.0005 * 2000 = $1.0
        let usd = cache.token_amount_usd("ethereum", "0xTOKEN", 100.0, "0xWETH", 0.0005);
        assert_eq!(usd, Some(100.0 * 0.0005 * 2000.0));
    }

    #[test]
    fn test_token_amount_usd_no_base_token() {
        let cache = PriceCache::new();
        // Neither token is a base token
        let usd = cache.token_amount_usd("ethereum", "0xA", 100.0, "0xB", 1.0);
        assert_eq!(usd, None);
    }

    #[test]
    fn test_swap_usd_values_token_a_is_base() {
        let cache = PriceCache::new();
        cache.set_price("ethereum", "0xUSDC", 1.0);

        // Swap: 1000 USDC for 0.5 WETH
        let result = cache.swap_usd_values("ethereum", "0xUSDC", 1000.0, "0xWETH", 0.5);
        let (usd_a, usd_b) = result.unwrap();
        assert!((usd_a - 1000.0).abs() < 0.001);
        // WETH price derived: (1000/0.5) * 1.0 = 2000 per WETH → 0.5 * 2000 = 1000
        assert!((usd_b - 1000.0).abs() < 0.001);
    }

    #[test]
    fn test_swap_usd_values_token_b_is_base() {
        let cache = PriceCache::new();
        cache.set_price("ethereum", "0xWETH", 2000.0);

        // Swap: 500 TOKEN for 0.25 WETH
        let result = cache.swap_usd_values("ethereum", "0xTOKEN", 500.0, "0xWETH", 0.25);
        let (usd_a, usd_b) = result.unwrap();
        assert!((usd_b - 500.0).abs() < 0.001);
        // TOKEN price derived: (0.25/500) * 2000 = 1.0 → 500 * 1.0 = 500
        assert!((usd_a - 500.0).abs() < 0.001);
    }

    #[test]
    fn test_swap_usd_values_neither_base() {
        let cache = PriceCache::new();
        let result = cache.swap_usd_values("ethereum", "0xA", 100.0, "0xB", 200.0);
        assert!(result.is_none());
    }
}
