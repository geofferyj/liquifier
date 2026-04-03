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

/// Special placeholder address used by many UIs to represent the native gas token.
pub const NATIVE_TOKEN_PLACEHOLDER: &str = "0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

/// Wrapped native token address for BSC.
pub const BSC_WBNB_ADDRESS: &str = "0xbb4cdB9CBd36B01bD1cBaEBF2De08d9173bc095c";

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

/// Parse pool type string as stored in API/proto payloads.
pub fn pool_type_from_str(value: &str) -> Option<PoolType> {
    match value.trim().to_ascii_lowercase().as_str() {
        "v2" => Some(PoolType::V2),
        "v3" => Some(PoolType::V3),
        _ => None,
    }
}

/// Resolve configured DEX entry for a chain, DEX name, and pool type.
pub fn dex_config_for_chain(
    chain: &str,
    dex_name: &str,
    pool_type: PoolType,
) -> Option<DexFactoryConfig> {
    let settings = Settings::global();
    settings
        .chains
        .get(chain)
        .filter(|c| c.enabled)
        .and_then(|c| {
            c.dex_factories
                .iter()
                .find(|d| d.name.eq_ignore_ascii_case(dex_name) && d.pool_type == pool_type)
                .cloned()
        })
}

/// Resolve the configured router for a chain/DEX/pool-type triple.
pub fn dex_router_for_chain(chain: &str, dex_name: &str, pool_type: PoolType) -> Option<String> {
    let dex = dex_config_for_chain(chain, dex_name, pool_type)?;
    let router = dex.router_address.trim();
    if router.is_empty() {
        None
    } else {
        Some(router.to_string())
    }
}

/// Returns true when the address is the native-token placeholder.
pub fn is_native_token_placeholder(address: &str) -> bool {
    address
        .trim()
        .eq_ignore_ascii_case(NATIVE_TOKEN_PLACEHOLDER)
}

/// Normalize user-facing token addresses into process-safe addresses.
/// For now, only BSC native placeholder is mapped to WBNB.
pub fn normalize_token_for_chain(chain: &str, token_address: &str) -> String {
    let token = token_address.trim();
    if chain.eq_ignore_ascii_case("bsc") && is_native_token_placeholder(token) {
        return BSC_WBNB_ADDRESS.to_string();
    }
    token.to_string()
}

/// Convert process-safe token addresses back to UI-facing representation.
/// For now, only WBNB on BSC is mapped back to the native placeholder.
pub fn display_token_for_chain(chain: &str, token_address: &str) -> String {
    let token = token_address.trim();
    if chain.eq_ignore_ascii_case("bsc") && token.eq_ignore_ascii_case(BSC_WBNB_ADDRESS) {
        return NATIVE_TOKEN_PLACEHOLDER.to_string();
    }
    token.to_string()
}
