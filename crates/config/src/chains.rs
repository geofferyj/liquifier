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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_chains() -> HashMap<String, ChainConfig> {
        let mut chains = HashMap::new();
        chains.insert(
            "bsc".to_string(),
            ChainConfig {
                enabled: true,
                chain_id: 56,
                rpc_url: "https://bsc-rpc.example.com".to_string(),
                ws_url: String::new(),
                base_tokens: vec![
                    BaseToken {
                        address: "0xbb4CdB9CBd36B01bD1cBaEBF2De08d9173bc095c".to_string(),
                        symbol: "WBNB".to_string(),
                        chainlink_oracle: "0xoracle".to_string(),
                    },
                    BaseToken {
                        address: "0xUSDT".to_string(),
                        symbol: "USDT".to_string(),
                        chainlink_oracle: String::new(),
                    },
                ],
                dex_factories: vec![DexFactoryConfig {
                    name: "pancakeswap".to_string(),
                    factory_address: "0xfactory".to_string(),
                    router_address: "0xrouter".to_string(),
                    pool_type: PoolTypeConfig::V2,
                    fee_tiers: vec![],
                }],
            },
        );
        chains.insert(
            "ethereum".to_string(),
            ChainConfig {
                enabled: false,
                chain_id: 1,
                rpc_url: "https://eth-rpc.example.com".to_string(),
                ws_url: String::new(),
                base_tokens: vec![],
                dex_factories: vec![],
            },
        );
        chains
    }

    // ── PoolTypeConfig Display ──────────────────────────────
    #[test]
    fn test_pool_type_display_v2() {
        assert_eq!(format!("{}", PoolTypeConfig::V2), "v2");
    }

    #[test]
    fn test_pool_type_display_v3() {
        assert_eq!(format!("{}", PoolTypeConfig::V3), "v3");
    }

    #[test]
    fn test_pool_type_equality() {
        assert_eq!(PoolTypeConfig::V2, PoolTypeConfig::V2);
        assert_ne!(PoolTypeConfig::V2, PoolTypeConfig::V3);
    }

    #[test]
    fn test_pool_type_serialize() {
        let v2 = serde_json::to_string(&PoolTypeConfig::V2).unwrap();
        assert_eq!(v2, "\"v2\"");
        let v3 = serde_json::to_string(&PoolTypeConfig::V3).unwrap();
        assert_eq!(v3, "\"v3\"");
    }

    #[test]
    fn test_pool_type_deserialize() {
        let v2: PoolTypeConfig = serde_json::from_str("\"v2\"").unwrap();
        assert_eq!(v2, PoolTypeConfig::V2);
        let v3: PoolTypeConfig = serde_json::from_str("\"v3\"").unwrap();
        assert_eq!(v3, PoolTypeConfig::V3);
    }

    // ── ChainConfig::base_token_addresses ───────────────────
    #[test]
    fn test_base_token_addresses() {
        let chains = sample_chains();
        let bsc = chains.get("bsc").unwrap();
        let addrs = bsc.base_token_addresses();
        assert_eq!(addrs.len(), 2);
        assert_eq!(addrs[0], "0xbb4CdB9CBd36B01bD1cBaEBF2De08d9173bc095c");
        assert_eq!(addrs[1], "0xUSDT");
    }

    #[test]
    fn test_base_token_addresses_empty() {
        let chains = sample_chains();
        let eth = chains.get("ethereum").unwrap();
        assert!(eth.base_token_addresses().is_empty());
    }

    // ── get_chain ───────────────────────────────────────────
    #[test]
    fn test_get_chain_enabled() {
        let chains = sample_chains();
        let bsc = get_chain(&chains, "bsc");
        assert!(bsc.is_some());
        assert_eq!(bsc.unwrap().chain_id, 56);
    }

    #[test]
    fn test_get_chain_disabled_returns_none() {
        let chains = sample_chains();
        assert!(get_chain(&chains, "ethereum").is_none());
    }

    #[test]
    fn test_get_chain_nonexistent_returns_none() {
        let chains = sample_chains();
        assert!(get_chain(&chains, "polygon").is_none());
    }

    // ── enabled_chains ──────────────────────────────────────
    #[test]
    fn test_enabled_chains() {
        let chains = sample_chains();
        let enabled = enabled_chains(&chains);
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].0, "bsc");
    }

    #[test]
    fn test_enabled_chains_empty_map() {
        let chains = HashMap::new();
        assert!(enabled_chains(&chains).is_empty());
    }

    // ── chain_name_to_id ────────────────────────────────────
    #[test]
    fn test_chain_name_to_id_known() {
        let chains = sample_chains();
        assert_eq!(chain_name_to_id(&chains, "bsc"), 56);
        // disabled chains still resolve their chain_id
        assert_eq!(chain_name_to_id(&chains, "ethereum"), 1);
    }

    #[test]
    fn test_chain_name_to_id_unknown_defaults_to_1() {
        let chains = sample_chains();
        assert_eq!(chain_name_to_id(&chains, "polygon"), 1);
    }

    // ── ChainConfig deserialization ─────────────────────────
    #[test]
    fn test_chain_config_deserialize_minimal() {
        let json = r#"{"enabled": true, "chain_id": 137}"#;
        let cfg: ChainConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.chain_id, 137);
        assert!(cfg.rpc_url.is_empty());
        assert!(cfg.base_tokens.is_empty());
        assert!(cfg.dex_factories.is_empty());
    }

    #[test]
    fn test_dex_factory_config_deserialize() {
        let json = r#"{
            "name": "uniswap",
            "factory_address": "0xfact",
            "router_address": "0xrout",
            "pool_type": "v3",
            "fee_tiers": [500, 3000, 10000]
        }"#;
        let dex: DexFactoryConfig = serde_json::from_str(json).unwrap();
        assert_eq!(dex.name, "uniswap");
        assert_eq!(dex.pool_type, PoolTypeConfig::V3);
        assert_eq!(dex.fee_tiers, vec![500, 3000, 10000]);
    }
}
