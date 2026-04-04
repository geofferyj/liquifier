use alloy::{
    primitives::{Address, B256, U256},
    rpc::types::Log,
};
use anyhow::Result;
use common::types::DexSwapEvent;

/// UniswapV2 Swap(address sender, uint256 amount0In, uint256 amount1In, uint256 amount0Out, uint256 amount1Out, address to)
const UNISWAP_V2_SWAP_SIG: B256 =
    alloy::primitives::b256!("d78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822");

/// UniswapV3 Swap(address sender, address recipient, int256 amount0, int256 amount1, uint160 sqrtPriceX96, uint128 liquidity, int24 tick)
const UNISWAP_V3_SWAP_SIG: B256 =
    alloy::primitives::b256!("c42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67");

/// Parse a raw EVM log into a standardized DexSwapEvent.
/// Returns Ok(None) for unrecognized log topics.
pub fn parse_swap_log(chain_name: &str, log: &Log) -> Result<Option<DexSwapEvent>> {
    let Some(topic0) = log.topic0() else {
        return Ok(None);
    };

    let tx_hash = log
        .transaction_hash
        .map(|h| format!("{h:?}"))
        .unwrap_or_default();
    let block_number = log.block_number.unwrap_or(0);
    let log_index = log.log_index.unwrap_or(0) as u32;
    let pool_address = format!("{:?}", log.address());

    if *topic0 == UNISWAP_V2_SWAP_SIG {
        return parse_v2_swap(
            chain_name,
            log,
            &tx_hash,
            block_number,
            log_index,
            &pool_address,
        );
    }

    if *topic0 == UNISWAP_V3_SWAP_SIG {
        return parse_v3_swap(
            chain_name,
            log,
            &tx_hash,
            block_number,
            log_index,
            &pool_address,
        );
    }

    Ok(None)
}

fn parse_v2_swap(
    chain: &str,
    log: &Log,
    tx_hash: &str,
    block_number: u64,
    log_index: u32,
    pool_address: &str,
) -> Result<Option<DexSwapEvent>> {
    // Topics: [sig, sender(indexed), to(indexed)]
    // Data: amount0In, amount1In, amount0Out, amount1Out
    if log.topics().len() < 3 {
        return Ok(None);
    }
    let sender = format!("{:?}", Address::from_word(log.topics()[1]));
    let recipient = format!("{:?}", Address::from_word(log.topics()[2]));

    let data = log.data().data.as_ref();
    if data.len() < 128 {
        return Ok(None);
    }

    let amount0_in = U256::from_be_slice(&data[0..32]);
    let amount1_in = U256::from_be_slice(&data[32..64]);
    let amount0_out = U256::from_be_slice(&data[64..96]);
    let amount1_out = U256::from_be_slice(&data[96..128]);

    // Determine direction: if amount0In > 0 => token0 is being sold (incoming), token1 is output
    let (amount_in, amount_out) = if !amount0_in.is_zero() {
        (amount0_in, amount1_out)
    } else {
        (amount1_in, amount0_out)
    };

    Ok(Some(DexSwapEvent {
        chain: chain.to_string(),
        block_number,
        tx_hash: tx_hash.to_string(),
        log_index,
        pool_address: pool_address.to_string(),
        dex_type: "uniswap_v2".to_string(),
        token_in: String::new(), // Resolved by Execution Engine from pool state
        token_out: String::new(), // Resolved by Execution Engine from pool state
        amount_in: amount_in.to_string(),
        amount_out: amount_out.to_string(),
        sender,
        recipient,
        timestamp: 0, // Set from block timestamp if needed
    }))
}

fn parse_v3_swap(
    chain: &str,
    log: &Log,
    tx_hash: &str,
    block_number: u64,
    log_index: u32,
    pool_address: &str,
) -> Result<Option<DexSwapEvent>> {
    // Topics: [sig, sender(indexed), recipient(indexed)]
    // Data: int256 amount0, int256 amount1, uint160 sqrtPriceX96, uint128 liquidity, int24 tick
    if log.topics().len() < 3 {
        return Ok(None);
    }
    let sender = format!("{:?}", Address::from_word(log.topics()[1]));
    let recipient = format!("{:?}", Address::from_word(log.topics()[2]));

    let data = log.data().data.as_ref();
    if data.len() < 160 {
        return Ok(None);
    }

    // amount0 and amount1 are int256 — negative means tokens leaving the pool (output)
    let amount0_raw = U256::from_be_slice(&data[0..32]);
    let amount1_raw = U256::from_be_slice(&data[32..64]);

    // Check sign bit (bit 255) for int256 negative
    let amount0_negative = amount0_raw.bit(255);
    let amount1_negative = amount1_raw.bit(255);

    // Determine amounts: positive = input, negative = output
    let (amount_in, amount_out) = if !amount0_negative {
        // amount0 is positive (token0 in), amount1 is negative (token1 out)
        let out = if amount1_negative {
            // Two's complement for absolute value
            (!amount1_raw).wrapping_add(U256::from(1))
        } else {
            amount1_raw
        };
        (amount0_raw, out)
    } else {
        // amount1 is positive (token1 in), amount0 is negative (token0 out)
        let out = (!amount0_raw).wrapping_add(U256::from(1));
        (amount1_raw, out)
    };

    Ok(Some(DexSwapEvent {
        chain: chain.to_string(),
        block_number,
        tx_hash: tx_hash.to_string(),
        log_index,
        pool_address: pool_address.to_string(),
        dex_type: "uniswap_v3".to_string(),
        token_in: String::new(),
        token_out: String::new(),
        amount_in: amount_in.to_string(),
        amount_out: amount_out.to_string(),
        sender,
        recipient,
        timestamp: 0,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{Address, LogData, B256, U256};

    fn make_log(
        address: Address,
        topics: Vec<B256>,
        data: Vec<u8>,
        block: Option<u64>,
        tx_hash: Option<B256>,
        log_index: Option<u64>,
    ) -> Log {
        Log {
            inner: alloy::primitives::Log {
                address,
                data: LogData::new_unchecked(topics, data.into()),
            },
            block_hash: None,
            block_number: block,
            block_timestamp: None,
            transaction_hash: tx_hash,
            transaction_index: None,
            log_index,
            removed: false,
        }
    }

    fn zero_padded_address(addr: Address) -> B256 {
        let mut buf = [0u8; 32];
        buf[12..32].copy_from_slice(addr.as_slice());
        B256::from(buf)
    }

    // ── Unrecognized topic → None ───────────────────────────
    #[test]
    fn test_parse_swap_log_unrecognized_topic() {
        let log = make_log(
            Address::ZERO,
            vec![B256::ZERO],
            vec![0u8; 128],
            Some(1),
            None,
            Some(0),
        );
        let result = parse_swap_log("bsc", &log).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_swap_log_no_topics() {
        let log = make_log(Address::ZERO, vec![], vec![], Some(1), None, Some(0));
        let result = parse_swap_log("bsc", &log).unwrap();
        assert!(result.is_none());
    }

    // ── V2 Swap ─────────────────────────────────────────────
    #[test]
    fn test_parse_v2_swap_token0_in() {
        let sender = Address::from([0x01; 20]);
        let recipient = Address::from([0x02; 20]);

        // amount0In=1000, amount1In=0, amount0Out=0, amount1Out=500
        let mut data = vec![0u8; 128];
        let amount0_in = U256::from(1000u64);
        let amount1_out = U256::from(500u64);
        data[0..32].copy_from_slice(&amount0_in.to_be_bytes::<32>());
        // amount1In = 0 (already zero)
        // amount0Out = 0 (already zero)
        data[96..128].copy_from_slice(&amount1_out.to_be_bytes::<32>());

        let log = make_log(
            Address::from([0xAA; 20]),
            vec![
                UNISWAP_V2_SWAP_SIG,
                zero_padded_address(sender),
                zero_padded_address(recipient),
            ],
            data,
            Some(100),
            Some(B256::from([0xBB; 32])),
            Some(3),
        );

        let event = parse_swap_log("bsc", &log).unwrap().unwrap();
        assert_eq!(event.chain, "bsc");
        assert_eq!(event.dex_type, "uniswap_v2");
        assert_eq!(event.block_number, 100);
        assert_eq!(event.log_index, 3);
        assert_eq!(event.amount_in, "1000");
        assert_eq!(event.amount_out, "500");
    }

    #[test]
    fn test_parse_v2_swap_token1_in() {
        let sender = Address::from([0x01; 20]);
        let recipient = Address::from([0x02; 20]);

        // amount0In=0, amount1In=2000, amount0Out=800, amount1Out=0
        let mut data = vec![0u8; 128];
        let amount1_in = U256::from(2000u64);
        let amount0_out = U256::from(800u64);
        data[32..64].copy_from_slice(&amount1_in.to_be_bytes::<32>());
        data[64..96].copy_from_slice(&amount0_out.to_be_bytes::<32>());

        let log = make_log(
            Address::from([0xCC; 20]),
            vec![
                UNISWAP_V2_SWAP_SIG,
                zero_padded_address(sender),
                zero_padded_address(recipient),
            ],
            data,
            Some(200),
            None,
            Some(0),
        );

        let event = parse_swap_log("ethereum", &log).unwrap().unwrap();
        assert_eq!(event.amount_in, "2000");
        assert_eq!(event.amount_out, "800");
    }

    #[test]
    fn test_parse_v2_swap_insufficient_topics() {
        let log = make_log(
            Address::ZERO,
            vec![UNISWAP_V2_SWAP_SIG, B256::ZERO], // only 2, need 3
            vec![0u8; 128],
            Some(1),
            None,
            Some(0),
        );
        let result = parse_swap_log("bsc", &log).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_v2_swap_insufficient_data() {
        let sender = Address::from([0x01; 20]);
        let recipient = Address::from([0x02; 20]);
        let log = make_log(
            Address::ZERO,
            vec![
                UNISWAP_V2_SWAP_SIG,
                zero_padded_address(sender),
                zero_padded_address(recipient),
            ],
            vec![0u8; 64], // only 64 bytes, need 128
            Some(1),
            None,
            Some(0),
        );
        let result = parse_swap_log("bsc", &log).unwrap();
        assert!(result.is_none());
    }

    // ── V3 Swap ─────────────────────────────────────────────
    #[test]
    fn test_parse_v3_swap_positive_amount0() {
        let sender = Address::from([0x01; 20]);
        let recipient = Address::from([0x02; 20]);

        // amount0 = +500 (positive, token0 in), amount1 = -300 (negative, token1 out)
        let mut data = vec![0u8; 160];
        let amount0 = U256::from(500u64);
        // amount1 = -300: twos complement of 300
        let amount1 = (!U256::from(300u64)).wrapping_add(U256::from(1));
        data[0..32].copy_from_slice(&amount0.to_be_bytes::<32>());
        data[32..64].copy_from_slice(&amount1.to_be_bytes::<32>());
        // sqrtPriceX96, liquidity, tick can be anything
        data[64..96].copy_from_slice(&U256::from(1u64).to_be_bytes::<32>());
        data[96..128].copy_from_slice(&U256::from(1u64).to_be_bytes::<32>());
        data[128..160].copy_from_slice(&U256::from(1u64).to_be_bytes::<32>());

        let log = make_log(
            Address::from([0xDD; 20]),
            vec![
                UNISWAP_V3_SWAP_SIG,
                zero_padded_address(sender),
                zero_padded_address(recipient),
            ],
            data,
            Some(300),
            None,
            Some(5),
        );

        let event = parse_swap_log("bsc", &log).unwrap().unwrap();
        assert_eq!(event.dex_type, "uniswap_v3");
        assert_eq!(event.amount_in, "500");
        assert_eq!(event.amount_out, "300");
    }

    #[test]
    fn test_parse_v3_swap_positive_amount1() {
        let sender = Address::from([0x01; 20]);
        let recipient = Address::from([0x02; 20]);

        // amount0 = -700 (negative, token0 out), amount1 = +1000 (positive, token1 in)
        let mut data = vec![0u8; 160];
        let amount0 = (!U256::from(700u64)).wrapping_add(U256::from(1)); // -700
        let amount1 = U256::from(1000u64);
        data[0..32].copy_from_slice(&amount0.to_be_bytes::<32>());
        data[32..64].copy_from_slice(&amount1.to_be_bytes::<32>());
        data[64..96].copy_from_slice(&U256::from(1u64).to_be_bytes::<32>());
        data[96..128].copy_from_slice(&U256::from(1u64).to_be_bytes::<32>());
        data[128..160].copy_from_slice(&U256::from(1u64).to_be_bytes::<32>());

        let log = make_log(
            Address::from([0xEE; 20]),
            vec![
                UNISWAP_V3_SWAP_SIG,
                zero_padded_address(sender),
                zero_padded_address(recipient),
            ],
            data,
            Some(400),
            None,
            Some(1),
        );

        let event = parse_swap_log("ethereum", &log).unwrap().unwrap();
        assert_eq!(event.amount_in, "1000");
        assert_eq!(event.amount_out, "700");
    }

    #[test]
    fn test_parse_v3_swap_insufficient_topics() {
        let log = make_log(
            Address::ZERO,
            vec![UNISWAP_V3_SWAP_SIG, B256::ZERO],
            vec![0u8; 160],
            Some(1),
            None,
            Some(0),
        );
        let result = parse_swap_log("bsc", &log).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_v3_swap_insufficient_data() {
        let sender = Address::from([0x01; 20]);
        let recipient = Address::from([0x02; 20]);
        let log = make_log(
            Address::ZERO,
            vec![
                UNISWAP_V3_SWAP_SIG,
                zero_padded_address(sender),
                zero_padded_address(recipient),
            ],
            vec![0u8; 100], // only 100 bytes, need 160
            Some(1),
            None,
            Some(0),
        );
        let result = parse_swap_log("bsc", &log).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_swap_log_captures_pool_address() {
        let sender = Address::from([0x01; 20]);
        let recipient = Address::from([0x02; 20]);
        let pool = Address::from([0xAB; 20]);

        let mut data = vec![0u8; 128];
        data[0..32].copy_from_slice(&U256::from(1u64).to_be_bytes::<32>());
        data[96..128].copy_from_slice(&U256::from(1u64).to_be_bytes::<32>());

        let log = make_log(
            pool,
            vec![
                UNISWAP_V2_SWAP_SIG,
                zero_padded_address(sender),
                zero_padded_address(recipient),
            ],
            data,
            Some(1),
            None,
            Some(0),
        );

        let event = parse_swap_log("bsc", &log).unwrap().unwrap();
        assert!(event.pool_address.contains("abababababababababab"));
    }
}
