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
    /// true when token0 is the input (sold into pool) and token1 is the output.
    /// false when token1 is the input and token0 is the output.
    /// Used by the execution engine to resolve actual token addresses from pool config.
    #[serde(default)]
    pub token0_is_input: bool,
}

/// ERC-20 Transfer event detected to a known user wallet, published to NATS
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepositEvent {
    pub chain: String,
    pub block_number: u64,
    pub tx_hash: String,
    pub log_index: u32,
    pub token_address: String,
    pub from: String,
    pub to: String,     // wallet address that received the deposit
    pub amount: String, // U256 decimal string
    pub wallet_id: String,
    pub user_id: String,
}

/// Trade completion event published by the execution engine to NATS
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeCompletedEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub trade_id: String,
    pub session_id: String,
    pub chain: String,
    pub sell_amount: String,
    pub received_amount: String,
    pub tx_hash: String,
    pub price_impact_bps: Option<f64>,
    pub executed_at: Option<String>,
    pub status: String,
    pub failure_reason: Option<String>,
}

/// NATS subject constants
pub const SUBJECT_DEX_SWAPS: &str = "evm.dex.swaps";
pub const SUBJECT_TRADES_COMPLETED: &str = "trades.completed";
pub const SUBJECT_SESSION_UPDATES: &str = "session.updates";
pub const SUBJECT_DEPOSITS: &str = "evm.deposits";

/// Special placeholder address used by many UIs to represent the native gas token.
pub const NATIVE_TOKEN_PLACEHOLDER: &str = "0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

/// Wrapped native token address for BSC.
pub const BSC_WBNB_ADDRESS: &str = "0xbb4cdB9CBd36B01bD1cBaEBF2De08d9173bc095c";

const WRAPPED_NATIVE_SYMBOLS: [&str; 11] = [
    "WETH", "WBNB", "WMATIC", "WAVAX", "WFTM", "WONE", "WCELO", "WGLMR", "WKLAY", "WOKT", "WSEI",
];

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

/// Resolve the wrapped-native token address configured for a chain.
pub fn wrapped_native_token_for_chain(chain: &str) -> Option<String> {
    let settings = Settings::global();
    let chain_cfg = settings.chains.get(chain)?;

    chain_cfg
        .base_tokens
        .iter()
        .find(|token| {
            let symbol = token.symbol.trim().to_ascii_uppercase();
            WRAPPED_NATIVE_SYMBOLS.contains(&symbol.as_str())
        })
        .map(|token| token.address.clone())
}

/// Normalize user-facing token addresses into process-safe addresses.
/// Native placeholders are mapped to each chain's configured wrapped-native token.
pub fn normalize_token_for_chain(chain: &str, token_address: &str) -> String {
    let token = token_address.trim();
    if is_native_token_placeholder(token) {
        if let Some(wrapped_native) = wrapped_native_token_for_chain(chain) {
            return wrapped_native;
        }
    }
    token.to_string()
}

/// Convert process-safe token addresses back to UI-facing representation.
/// Wrapped-native addresses are mapped back to the native placeholder.
pub fn display_token_for_chain(chain: &str, token_address: &str) -> String {
    let token = token_address.trim();
    if let Some(wrapped_native) = wrapped_native_token_for_chain(chain) {
        if token.eq_ignore_ascii_case(&wrapped_native) {
            return NATIVE_TOKEN_PLACEHOLDER.to_string();
        }
    }
    token.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── pool_type_from_str ──────────────────────────────────
    #[test]
    fn test_pool_type_from_str_v2() {
        assert_eq!(pool_type_from_str("v2"), Some(PoolType::V2));
    }

    #[test]
    fn test_pool_type_from_str_v3() {
        assert_eq!(pool_type_from_str("v3"), Some(PoolType::V3));
    }

    #[test]
    fn test_pool_type_from_str_uppercase() {
        assert_eq!(pool_type_from_str("V2"), Some(PoolType::V2));
        assert_eq!(pool_type_from_str("V3"), Some(PoolType::V3));
    }

    #[test]
    fn test_pool_type_from_str_with_whitespace() {
        assert_eq!(pool_type_from_str("  v2  "), Some(PoolType::V2));
    }

    #[test]
    fn test_pool_type_from_str_invalid() {
        assert_eq!(pool_type_from_str("v4"), None);
        assert_eq!(pool_type_from_str(""), None);
        assert_eq!(pool_type_from_str("uniswap"), None);
    }

    // ── is_native_token_placeholder ─────────────────────────
    #[test]
    fn test_is_native_placeholder_true() {
        assert!(is_native_token_placeholder(NATIVE_TOKEN_PLACEHOLDER));
    }

    #[test]
    fn test_is_native_placeholder_uppercase() {
        assert!(is_native_token_placeholder(
            "0xEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE"
        ));
    }

    #[test]
    fn test_is_native_placeholder_with_whitespace() {
        assert!(is_native_token_placeholder(
            "  0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee  "
        ));
    }

    #[test]
    fn test_is_native_placeholder_false() {
        assert!(!is_native_token_placeholder(
            "0xbb4CdB9CBd36B01bD1cBaEBF2De08d9173bc095c"
        ));
        assert!(!is_native_token_placeholder(""));
        assert!(!is_native_token_placeholder(
            "0x0000000000000000000000000000000000000000"
        ));
    }

    // ── DexSwapEvent serialization ──────────────────────────
    #[test]
    fn test_dex_swap_event_serialize_deserialize() {
        let event = DexSwapEvent {
            chain: "bsc".to_string(),
            block_number: 12345678,
            tx_hash: "0xabc".to_string(),
            log_index: 0,
            pool_address: "0x123".to_string(),
            dex_type: "uniswap_v2".to_string(),
            token_in: "0xtoken_in".to_string(),
            token_out: "0xtoken_out".to_string(),
            amount_in: "1000".to_string(),
            amount_out: "500".to_string(),
            sender: "0xsender".to_string(),
            recipient: "0xrecipient".to_string(),
            timestamp: 1700000000,
            token0_is_input: true,
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: DexSwapEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.chain, "bsc");
        assert_eq!(deserialized.block_number, 12345678);
        assert_eq!(deserialized.amount_in, "1000");
        assert_eq!(deserialized.timestamp, 1700000000);
    }

    #[test]
    fn test_dex_swap_event_from_json() {
        let json = r#"{
            "chain": "ethereum",
            "block_number": 100,
            "tx_hash": "0x1",
            "log_index": 5,
            "pool_address": "0xpool",
            "dex_type": "uniswap_v3",
            "token_in": "",
            "token_out": "",
            "amount_in": "0",
            "amount_out": "0",
            "sender": "0xs",
            "recipient": "0xr",
            "timestamp": 0
        }"#;
        let event: DexSwapEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.chain, "ethereum");
        assert_eq!(event.log_index, 5);
        assert_eq!(event.dex_type, "uniswap_v3");
    }

    // ── Constants ───────────────────────────────────────────
    #[test]
    fn test_nats_subject_constants() {
        assert_eq!(SUBJECT_DEX_SWAPS, "evm.dex.swaps");
        assert_eq!(SUBJECT_TRADES_COMPLETED, "trades.completed");
        assert_eq!(SUBJECT_SESSION_UPDATES, "session.updates");
    }

    #[test]
    fn test_native_token_placeholder_format() {
        assert!(NATIVE_TOKEN_PLACEHOLDER.starts_with("0x"));
        assert_eq!(NATIVE_TOKEN_PLACEHOLDER.len(), 42);
    }

    #[test]
    fn test_bsc_wbnb_address_format() {
        assert!(BSC_WBNB_ADDRESS.starts_with("0x"));
        assert_eq!(BSC_WBNB_ADDRESS.len(), 42);
    }
}
