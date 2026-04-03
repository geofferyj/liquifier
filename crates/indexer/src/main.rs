use alloy::{
    primitives::{address, b256, Address, B256, U256},
    providers::{Provider, ProviderBuilder, WsConnect},
    rpc::types::{Filter, Log},
};
use anyhow::{Context, Result};
use common::types::{DexSwapEvent, SUBJECT_DEX_SWAPS};
use futures::StreamExt;
use std::str::FromStr;
use tracing::{error, info, warn};

mod parser;

// ─────────────────────────────────────────────────────────────
// Well-known event signatures
// ─────────────────────────────────────────────────────────────

/// UniswapV2 Swap(address,uint256,uint256,uint256,uint256,address)
const UNISWAP_V2_SWAP_SIG: B256 =
    b256!("d78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822");

/// UniswapV3 Swap(address,address,int256,int256,uint160,uint128,int24)
const UNISWAP_V3_SWAP_SIG: B256 =
    b256!("c42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67");

// ─────────────────────────────────────────────────────────────
// Chain configuration
// ─────────────────────────────────────────────────────────────

struct IndexerChain {
    name: String,
    ws_url: String,
}

// ─────────────────────────────────────────────────────────────
// Main
// ─────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize global config (loads config/default.yml + env overrides)
    liquifier_config::Settings::init().expect("Failed to load config");
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .json()
        .init();

    let cfg = liquifier_config::Settings::global();
    let nats_url = &cfg.nats.url;

    // Connect to NATS with JetStream
    let nats_client =
        common::retry::retry("NATS", 10, || async { async_nats::connect(nats_url).await })
            .await
            .context("Failed to connect to NATS")?;
    let jetstream = async_nats::jetstream::new(nats_client);

    // Ensure the stream exists
    jetstream
        .get_or_create_stream(async_nats::jetstream::stream::Config {
            name: "DEX_SWAPS".to_string(),
            subjects: vec![SUBJECT_DEX_SWAPS.to_string()],
            retention: async_nats::jetstream::stream::RetentionPolicy::WorkQueue,
            max_age: std::time::Duration::from_secs(3600), // 1 hour retention
            ..Default::default()
        })
        .await
        .context("Failed to create NATS stream")?;

    // Build chain list from config — only enabled chains with a ws_url
    let chains: Vec<IndexerChain> = cfg
        .chains
        .iter()
        .filter(|(_, c)| c.enabled && !c.ws_url.is_empty())
        .map(|(name, c)| IndexerChain {
            name: name.clone(),
            ws_url: c.ws_url.clone(),
        })
        .collect();

    info!(
        chain_count = chains.len(),
        "Starting indexer for EVM chains"
    );

    // Spawn one task per chain
    let mut handles = Vec::new();
    for chain in chains {
        let js = jetstream.clone();
        let handle = tokio::spawn(async move {
            loop {
                match index_chain(&chain.name, &chain.ws_url, &js).await {
                    Ok(()) => {
                        warn!(chain = %chain.name, "Indexer stream ended, reconnecting in 5s...");
                    }
                    Err(e) => {
                        error!(chain = %chain.name, error = %e, "Indexer error, reconnecting in 5s...");
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });
        handles.push(handle);
    }

    // Wait for all indexers (they run forever)
    futures::future::join_all(handles).await;
    Ok(())
}

// ─────────────────────────────────────────────────────────────
// Per-chain indexing loop
// ─────────────────────────────────────────────────────────────

async fn index_chain(
    chain_name: &str,
    ws_url: &str,
    jetstream: &async_nats::jetstream::Context,
) -> Result<()> {
    info!(chain = %chain_name, "Connecting to EVM WebSocket...");

    let ws = WsConnect::new(ws_url);
    let provider = ProviderBuilder::new().connect_ws(ws).await?;

    // Subscribe to Swap logs from all addresses
    let filter = Filter::new().event_signature(vec![UNISWAP_V2_SWAP_SIG, UNISWAP_V3_SWAP_SIG]);

    let sub = provider.subscribe_logs(&filter).await?;
    let mut stream = sub.into_stream();

    info!(chain = %chain_name, "Subscribed to DEX swap events");

    while let Some(log) = stream.next().await {
        match parser::parse_swap_log(chain_name, &log) {
            Ok(Some(event)) => {
                let payload = serde_json::to_vec(&event)?;
                if let Err(e) = jetstream.publish(SUBJECT_DEX_SWAPS, payload.into()).await {
                    error!(chain = %chain_name, error = %e, "Failed to publish to NATS");
                }
            }
            Ok(None) => {
                // Unrecognized log, skip
            }
            Err(e) => {
                warn!(chain = %chain_name, error = %e, "Failed to parse swap log");
            }
        }
    }

    Ok(())
}
