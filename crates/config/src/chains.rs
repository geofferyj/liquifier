use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
pub struct ChainConfig {
    pub enabled: bool,
    pub chain_id: u64,
    #[serde(default)]
    pub rpc_url: String,
    #[serde(default)]
    pub ws_url: String,
    #[serde(default)]
    pub base_tokens: Vec<BaseToken>,
    #[serde(default)]
    pub dex_factories: Vec<DexFactoryConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BaseToken {
    pub address: String,
    #[serde(default)]
    pub symbol: String,
    /// Chainlink price feed contract address (token/USD).
    /// A base token MUST have a valid oracle — if empty, it is skipped.
    #[serde(default)]
    pub chainlink_oracle: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DexFactoryConfig {
    pub name: String,
    pub factory_address: String,
    pub router_address: String,
    pub pool_type: PoolTypeConfig,
    #[serde(default)]
    pub fee_tiers: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PoolTypeConfig {
    V2,
    V3,
}

impl std::fmt::Display for PoolTypeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PoolTypeConfig::V2 => write!(f, "v2"),
            PoolTypeConfig::V3 => write!(f, "v3"),
        }
    }
}

impl ChainConfig {
    /// Get base token addresses as a simple Vec<&str>
    pub fn base_token_addresses(&self) -> Vec<&str> {
        self.base_tokens
            .iter()
            .map(|t| t.address.as_str())
            .collect()
    }
}

/// Helper: get a chain config by name from a chains map.
pub fn get_chain<'a>(
    chains: &'a HashMap<String, ChainConfig>,
    name: &str,
) -> Option<&'a ChainConfig> {
    chains.get(name).filter(|c| c.enabled)
}

/// Get all enabled chains.
pub fn enabled_chains(chains: &HashMap<String, ChainConfig>) -> Vec<(&str, &ChainConfig)> {
    chains
        .iter()
        .filter(|(_, c)| c.enabled)
        .map(|(name, c)| (name.as_str(), c))
        .collect()
}

/// Map chain name → numeric chain ID (from config).
pub fn chain_name_to_id(chains: &HashMap<String, ChainConfig>, name: &str) -> u64 {
    chains.get(name).map(|c| c.chain_id).unwrap_or(1)
}
