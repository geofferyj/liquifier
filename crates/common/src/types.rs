use serde::{Deserialize, Serialize};

/// Standardized DEX swap event published to NATS
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DexSwapEvent {
    pub chain: String,
    pub block_number: u64,
    pub tx_hash: String,
    pub log_index: u32,
    pub pool_address: String,
    pub dex_type: String, // "uniswap_v2", "uniswap_v3", "pancakeswap", etc.
    pub token_in: String,
    pub token_out: String,
    pub amount_in: String,  // U256 decimal string
    pub amount_out: String, // U256 decimal string
    pub sender: String,
    pub recipient: String,
    pub timestamp: u64,
}

/// NATS subject constants
pub const SUBJECT_DEX_SWAPS: &str = "evm.dex.swaps";
pub const SUBJECT_TRADES_COMPLETED: &str = "trades.completed";
pub const SUBJECT_SESSION_UPDATES: &str = "session.updates";

// Re-export config chain types so existing consumers keep compiling
pub use liquifier_config::{
    chain_name_to_id, enabled_chains, get_chain, ChainConfig, DexFactoryConfig,
    PoolTypeConfig as PoolType, Settings,
};

// ─────────────────────────────────────────────────────────────
// Backward-compatible helpers backed by config
// ─────────────────────────────────────────────────────────────

/// Returns DEX factories for a chain from the global config.
/// Equivalent to the old hardcoded `dex_factories_for_chain`.
pub fn dex_factories_for_chain(chain: &str) -> Vec<DexFactoryConfig> {
    let settings = Settings::global();
    settings
        .chains
        .get(chain)
        .filter(|c| c.enabled)
        .map(|c| c.dex_factories.clone())
        .unwrap_or_default()
}

/// Returns base token addresses for a chain from the global config.
/// Equivalent to the old hardcoded `common_base_tokens`.
pub fn base_tokens_for_chain(chain: &str) -> Vec<String> {
    let settings = Settings::global();
    settings
        .chains
        .get(chain)
        .filter(|c| c.enabled)
        .map(|c| c.base_tokens.iter().map(|t| t.address.clone()).collect())
        .unwrap_or_default()
}
