//! Central Indexer Service
//!
//! Subscribes to an EVM node via WebSocket, decodes DEX Swap events from
//! all known Uniswap-V2-style and Uniswap-V3-style pools, and publishes
//! normalised payloads to the `evm.dex.swaps` Kafka topic.

use alloy::{
    eips::BlockNumberOrTag,
    primitives::{Address, B256, I256, U256},
    providers::{Provider, ProviderBuilder, WsConnect},
    rpc::types::{Filter, Log},
    sol_types::{SolEvent, sol},
};
use anyhow::Context;
use rdkafka::{
    config::ClientConfig,
    producer::{FutureProducer, FutureRecord},
};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{error, info, warn};

// ─── Solidity event signatures (via alloy::sol!) ───────────────────────────

// Uniswap V2 Swap event
sol! {
    event SwapV2(
        address indexed sender,
        uint256 amount0In,
        uint256 amount1In,
        uint256 amount0Out,
        uint256 amount1Out,
        address indexed to
    );
}

// Uniswap V3 Swap event
sol! {
    event SwapV3(
        address indexed sender,
        address indexed recipient,
        int256  amount0,
        int256  amount1,
        uint160 sqrtPriceX96,
        uint128 liquidity,
        int24   tick
    );
}

// ─── Normalised swap event ─────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DexSwapEvent {
    pub block_number:  u64,
    pub tx_hash:       String,
    pub log_index:     u64,
    pub pool_address:  String,
    pub chain_id:      u64,
    pub version:       u8,       // 2 or 3
    /// The token that was bought (received by the pool — i.e., the sell side)
    pub token_in:      String,
    /// The token that was sold by the pool (the buy side from user perspective)
    pub token_out:     String,
    /// Raw amount of token bought (from pool perspective)
    pub amount_in:     String,
    /// Raw amount of token sold
    pub amount_out:    String,
    pub sender:        String,
    pub recipient:     String,
}

// ─── Main ──────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let ws_url     = std::env::var("EVM_WS_URL").context("EVM_WS_URL not set")?;
    let brokers    = std::env::var("KAFKA_BROKERS").context("KAFKA_BROKERS not set")?;
    let topic      = std::env::var("KAFKA_TOPIC_DEX_SWAPS")
                        .unwrap_or_else(|_| "evm.dex.swaps".into());
    let chain_id: u64 = std::env::var("CHAIN_ID")
                        .unwrap_or_else(|_| "1".into())
                        .parse()
                        .context("Invalid CHAIN_ID")?;

    // ── Kafka producer ─────────────────────────────────────────────────────
    let producer: FutureProducer = ClientConfig::new()
        .set("bootstrap.servers", &brokers)
        .set("message.timeout.ms", "5000")
        .set("compression.type", "lz4")
        .create()
        .context("Failed to create Kafka producer")?;

    info!("Connecting to EVM node: {ws_url}");

    loop {
        match run_indexer(&ws_url, &producer, &topic, chain_id).await {
            Ok(()) => info!("Indexer exited cleanly; restarting…"),
            Err(e) => {
                error!("Indexer error: {e:#}; reconnecting in 5s…");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

async fn run_indexer(
    ws_url: &str,
    producer: &FutureProducer,
    topic: &str,
    chain_id: u64,
) -> anyhow::Result<()> {
    let provider = ProviderBuilder::new()
        .on_ws(WsConnect::new(ws_url))
        .await
        .context("WS connect")?;

    // Subscribe to both V2 and V3 Swap events across all addresses
    let filter = Filter::new()
        .from_block(BlockNumberOrTag::Latest)
        .event_signature(vec![SwapV2::SIGNATURE_HASH, SwapV3::SIGNATURE_HASH]);

    let sub = provider
        .subscribe_logs(&filter)
        .await
        .context("subscribe_logs")?;

    info!("Indexer subscribed — chain_id={chain_id}");

    let mut stream = sub.into_stream();

    while let Some(log) = tokio_stream::StreamExt::next(&mut stream).await {
        if let Err(e) = process_log(&log, producer, topic, chain_id).await {
            warn!("Failed to process log: {e:#}");
        }
    }

    Ok(())
}

async fn process_log(
    log: &Log,
    producer: &FutureProducer,
    topic: &str,
    chain_id: u64,
) -> anyhow::Result<()> {
    let pool_address = format!("{:#x}", log.address());
    let tx_hash      = log
        .transaction_hash
        .map(|h| format!("{h:#x}"))
        .unwrap_or_default();
    let block_number = log.block_number.unwrap_or(0);
    let log_index    = log.log_index.unwrap_or(0);

    // Distinguish V2 vs V3 by topic[0]
    let topic0: B256 = log
        .topics()
        .first()
        .copied()
        .unwrap_or_default();

    let event = if topic0 == SwapV2::SIGNATURE_HASH {
        let decoded = SwapV2::decode_log(log, true)
            .context("Decode SwapV2")?;

        // Determine direction: if amount0In > 0 → token0 was the input
        let (token_in, token_out, amount_in, amount_out) =
            if decoded.amount0In > U256::ZERO {
                ("token0", "token1", decoded.amount0In, decoded.amount1Out)
            } else {
                ("token1", "token0", decoded.amount1In, decoded.amount0Out)
            };

        DexSwapEvent {
            block_number,
            tx_hash,
            log_index,
            pool_address,
            chain_id,
            version:   2,
            token_in:  token_in.into(),
            token_out: token_out.into(),
            amount_in: amount_in.to_string(),
            amount_out: amount_out.to_string(),
            sender:    format!("{:#x}", decoded.sender),
            recipient: format!("{:#x}", decoded.to),
        }
    } else if topic0 == SwapV3::SIGNATURE_HASH {
        let decoded = SwapV3::decode_log(log, true)
            .context("Decode SwapV3")?;

        // V3: negative amount means the pool sends that token out
        let (token_in, token_out, amount_in, amount_out) =
            if decoded.amount0 > I256::ZERO {
                (
                    "token0",
                    "token1",
                    decoded.amount0.unsigned_abs(),
                    decoded.amount1.unsigned_abs(),
                )
            } else {
                (
                    "token1",
                    "token0",
                    decoded.amount1.unsigned_abs(),
                    decoded.amount0.unsigned_abs(),
                )
            };

        DexSwapEvent {
            block_number,
            tx_hash,
            log_index,
            pool_address,
            chain_id,
            version:   3,
            token_in:  token_in.into(),
            token_out: token_out.into(),
            amount_in: amount_in.to_string(),
            amount_out: amount_out.to_string(),
            sender:    format!("{:#x}", decoded.sender),
            recipient: format!("{:#x}", decoded.recipient),
        }
    } else {
        return Ok(()); // unknown event
    };

    let payload = serde_json::to_string(&event)?;
    let key     = format!("{chain_id}:{}", event.pool_address);

    producer
        .send(
            FutureRecord::to(topic)
                .key(&key)
                .payload(&payload),
            Duration::from_secs(5),
        )
        .await
        .map_err(|(e, _)| anyhow::anyhow!("Kafka send: {e}"))?;

    Ok(())
}
