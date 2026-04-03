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
