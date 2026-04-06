use alloy::{
    consensus::BlockHeader,
    network::{BlockResponse, Ethereum},
    primitives::{b256, Address, B256, U256},
    providers::{Provider, ProviderBuilder},
    rpc::types::{BlockId, BlockNumberOrTag, Log},
    sol,
};
use anyhow::{Context, Result};
use common::types::{DepositEvent, DexSwapEvent, SUBJECT_DEPOSITS, SUBJECT_DEX_SWAPS};
use sqlx::postgres::PgPoolOptions;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, error, info, warn};

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

/// ERC-20 Transfer(address,address,uint256)
const ERC20_TRANSFER_SIG: B256 =
    b256!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef");

sol! {
    #[sol(rpc)]
    interface IPoolTokens {
        function token0() external view returns (address);
        function token1() external view returns (address);
    }
}

/// Cache of pool address -> Option<(token0, token1)>.
/// `None` means we already tried and the address is not a valid pool.
type TokenCache = Arc<RwLock<HashMap<Address, Option<(Address, Address)>>>>;

/// Cached mapping of lowercased wallet address -> (wallet_id, user_id) for common users.
/// Refreshed periodically from the database.
type WalletLookup = Arc<RwLock<HashMap<Address, (String, String)>>>;

// ─────────────────────────────────────────────────────────────
// Chain configuration
// ─────────────────────────────────────────────────────────────

struct IndexerChain {
    name: String,
    rpc_url: String,
}

// ─────────────────────────────────────────────────────────────
// Main
// ─────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize global config (loads config/default.yml + env overrides)
    liquifier_config::Settings::init().expect("Failed to load config");
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("alloy_transport_http=warn".parse().unwrap())
                .add_directive("alloy_transport_ws=warn".parse().unwrap()),
        )
        // .json()
        .init();

    let cfg = liquifier_config::Settings::global();
    let nats_url = &cfg.nats.url;
    let database_url = &cfg.database.url;
    let indexer_cfg = cfg.indexer.clone();

    // Connect to PostgreSQL (for loading common-user wallet addresses)
    let db = PgPoolOptions::new()
        .max_connections(3)
        .connect(database_url)
        .await
        .context("Failed to connect to PostgreSQL")?;

    // Connect to NATS with JetStream
    let nats_client =
        common::retry::retry("NATS", 10, || async { async_nats::connect(nats_url).await })
            .await
            .context("Failed to connect to NATS")?;
    let jetstream = async_nats::jetstream::new(nats_client);

    // Ensure the DEX_SWAPS stream exists
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

    // Ensure the DEPOSITS stream exists
    jetstream
        .get_or_create_stream(async_nats::jetstream::stream::Config {
            name: "DEPOSITS".to_string(),
            subjects: vec![SUBJECT_DEPOSITS.to_string()],
            retention: async_nats::jetstream::stream::RetentionPolicy::WorkQueue,
            max_age: std::time::Duration::from_secs(86400), // 24 hour retention
            ..Default::default()
        })
        .await
        .context("Failed to create DEPOSITS NATS stream")?;

    // Shared wallet lookup cache, refreshed periodically from DB
    let wallet_lookup: WalletLookup = Arc::new(RwLock::new(HashMap::new()));

    // Initial load of wallet addresses
    if let Err(e) = refresh_wallet_lookup(&db, &wallet_lookup).await {
        warn!(error = %e, "Initial wallet address load failed (will retry)");
    }

    // Spawn periodic wallet-address refresh task (every 60s)
    {
        let db = db.clone();
        let wl = wallet_lookup.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                if let Err(e) = refresh_wallet_lookup(&db, &wl).await {
                    warn!(error = %e, "Failed to refresh wallet addresses");
                }
            }
        });
    }

    // Build chain list from config — only enabled chains with an rpc_url
    let chains: Vec<IndexerChain> = cfg
        .chains
        .iter()
        .filter(|(_, c)| c.enabled && !c.rpc_url.is_empty())
        .map(|(name, c)| IndexerChain {
            name: name.clone(),
            rpc_url: c.rpc_url.clone(),
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
        let token_cache: TokenCache = Arc::new(RwLock::new(HashMap::new()));
        let wl = wallet_lookup.clone();
        let indexer_cfg = indexer_cfg.clone();
        let handle = tokio::spawn(async move {
            loop {
                match index_chain(
                    &chain.name,
                    &chain.rpc_url,
                    &indexer_cfg,
                    &js,
                    &token_cache,
                    &wl,
                )
                .await
                {
                    Ok(()) => {
                        warn!(chain = %chain.name, "Indexer poller ended, reconnecting in 5s...");
                    }
                    Err(e) => {
                        error!(chain = %chain.name, error = %e, "Indexer poller error, reconnecting in 5s...");
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
    rpc_url: &str,
    indexer_cfg: &liquifier_config::IndexerSettings,
    jetstream: &async_nats::jetstream::Context,
    token_cache: &TokenCache,
    wallet_lookup: &WalletLookup,
) -> Result<()> {
    let rpc_url: alloy::transports::http::reqwest::Url = rpc_url
        .parse()
        .with_context(|| format!("Invalid rpc_url for chain {chain_name}"))?;
    let provider = ProviderBuilder::new().connect_http(rpc_url);

    let worker_count = indexer_cfg.worker_count.max(1);
    let block_queue_size = indexer_cfg.block_queue_size.max(1);
    let block_poll_interval_ms = indexer_cfg.block_poll_interval_ms.max(1);
    let max_block_age_secs = indexer_cfg.max_block_age_secs;

    info!(
        chain = %chain_name,
        worker_count,
        block_queue_size,
        block_poll_interval_ms,
        max_block_age_secs,
        "Starting RPC polling indexer"
    );

    let (block_tx, block_rx) = mpsc::channel::<u64>(block_queue_size);
    let shared_rx = Arc::new(Mutex::new(block_rx));

    for worker_id in 0..worker_count {
        let provider = provider.clone();
        let chain_name = chain_name.to_string();
        let jetstream = jetstream.clone();
        let token_cache = token_cache.clone();
        let wallet_lookup = wallet_lookup.clone();
        let block_rx = shared_rx.clone();

        tokio::spawn(async move {
            loop {
                let maybe_block_number = {
                    let mut locked = block_rx.lock().await;
                    locked.recv().await
                };

                let Some(block_number) = maybe_block_number else {
                    warn!(chain = %chain_name, worker_id, "Block queue closed, worker exiting");
                    break;
                };

                if let Err(e) = process_block(
                    &chain_name,
                    &provider,
                    block_number,
                    max_block_age_secs,
                    &jetstream,
                    &token_cache,
                    &wallet_lookup,
                )
                .await
                {
                    error!(
                        chain = %chain_name,
                        worker_id,
                        block_number,
                        error = %e,
                        "Failed to process block"
                    );
                }
            }
        });
    }

    let mut last_enqueued = provider
        .get_block_number()
        .await
        .context("Failed to fetch initial block number")?;

    info!(
        chain = %chain_name,
        starting_block = last_enqueued,
        "Block poller initialized at latest height"
    );

    loop {
        match provider.get_block_number().await {
            Ok(latest_block) => {
                if latest_block > last_enqueued {
                    for block_number in (last_enqueued + 1)..=latest_block {
                        if block_tx.send(block_number).await.is_err() {
                            anyhow::bail!("Block queue closed unexpectedly");
                        }
                    }
                    last_enqueued = latest_block;
                }
            }
            Err(e) => {
                warn!(chain = %chain_name, error = %e, "Failed to fetch latest block number");
            }
        }

        tokio::time::sleep(Duration::from_millis(block_poll_interval_ms)).await;
    }
}

async fn process_block<P: Provider<Ethereum> + Clone>(
    chain_name: &str,
    provider: &P,
    block_number: u64,
    max_block_age_secs: u64,
    jetstream: &async_nats::jetstream::Context,
    token_cache: &TokenCache,
    wallet_lookup: &WalletLookup,
) -> Result<()> {
    let Some(block) = provider
        .get_block_by_number(BlockNumberOrTag::Number(block_number))
        .await
        .with_context(|| format!("Failed to fetch block {block_number}"))?
    else {
        warn!(chain = %chain_name, block_number, "Block not found");
        return Ok(());
    };

    let block_timestamp = block.header().timestamp();
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let block_age_secs = now_secs.saturating_sub(block_timestamp);

    if block_age_secs > max_block_age_secs {
        // info!(
        //     chain = %chain_name,
        //     block_number,
        //     block_age_secs,
        //     max_block_age_secs,
        //     "Dropped block number: too old"
        // );
        return Ok(());
    }

    // info!(
    //     chain = %chain_name,
    //     block_number,
    //     block_age_secs,
    //     "Valid block number: processing"
    // );

    let receipts = provider
        .get_block_receipts(BlockId::Number(BlockNumberOrTag::Number(block_number)))
        .await
        .with_context(|| format!("Failed to fetch receipts for block {block_number}"))?;

    let Some(receipts) = receipts else {
        warn!(chain = %chain_name, block_number, "No receipts returned for block");
        return Ok(());
    };

    for receipt in receipts {
        for log in receipt.logs() {
            process_receipt_log(
                chain_name,
                provider,
                log,
                block_timestamp,
                jetstream,
                token_cache,
                wallet_lookup,
            )
            .await;
        }
    }

    Ok(())
}

async fn process_receipt_log<P: Provider<Ethereum> + Clone>(
    chain_name: &str,
    provider: &P,
    log: &Log,
    block_timestamp: u64,
    jetstream: &async_nats::jetstream::Context,
    token_cache: &TokenCache,
    wallet_lookup: &WalletLookup,
) {
    let topic0 = match log.topic0() {
        Some(t) => *t,
        None => return,
    };

    if topic0 != UNISWAP_V2_SWAP_SIG
        && topic0 != UNISWAP_V3_SWAP_SIG
        && topic0 != ERC20_TRANSFER_SIG
    {
        return;
    }

    // ── ERC-20 Transfer event ──────────────────────────
    if topic0 == ERC20_TRANSFER_SIG {
        if let Some(deposit) = try_parse_deposit(chain_name, log, wallet_lookup).await {
            let payload = match serde_json::to_vec(&deposit) {
                Ok(p) => p,
                Err(e) => {
                    warn!(error = %e, "Failed to serialize deposit event");
                    return;
                }
            };
            if let Err(e) = jetstream.publish(SUBJECT_DEPOSITS, payload.into()).await {
                error!(chain = %chain_name, error = %e, "Failed to publish deposit to NATS");
            }
        }
        return;
    }

    // ── DEX Swap event ─────────────────────────────────
    match parser::parse_swap_log(chain_name, log) {
        Ok(Some(mut event)) => {
            event.timestamp = block_timestamp;

            if let Err(e) = resolve_tokens_for_event(provider, log, &mut event, token_cache).await {
                debug!(
                    chain = %chain_name,
                    pool = %event.pool_address,
                    block_number = event.block_number,
                    error = %e,
                    "Failed to resolve token_in/token_out; skipping event"
                );
                return;
            }

            match serde_json::to_vec(&event) {
                Ok(payload) => {
                    if let Err(e) = jetstream.publish(SUBJECT_DEX_SWAPS, payload.into()).await {
                        error!(chain = %chain_name, block_number = event.block_number, error = %e, "Failed to publish to NATS");
                    }
                }
                Err(e) => {
                    warn!(
                        chain = %chain_name,
                        block_number = event.block_number,
                        error = %e,
                        "Failed to serialize swap event"
                    );
                }
            }
        }
        Ok(None) => {
            // Unrecognized log, skip
        }
        Err(e) => {
            warn!(chain = %chain_name, block_number = log.block_number.unwrap_or_default(), error = %e, "Failed to parse swap log");
        }
    }
}

async fn resolve_tokens_for_event<P: Provider + Clone>(
    provider: &P,
    log: &Log,
    event: &mut DexSwapEvent,
    token_cache: &TokenCache,
) -> Result<()> {
    let pool = log.address();

    // Check cache first
    let cached = {
        let cache = token_cache.read().await;
        cache.get(&pool).copied()
    };

    let (token0, token1) = match cached {
        Some(Some(pair)) => pair,
        Some(None) => {
            // Previously failed — this address is not a valid pool
            anyhow::bail!("address previously identified as non-pool");
        }
        None => {
            // Not in cache — query on-chain
            let pool_contract = IPoolTokens::new(pool, provider.clone());
            match (
                pool_contract.token0().call().await,
                pool_contract.token1().call().await,
            ) {
                (Ok(t0), Ok(t1)) => {
                    let pair = (Address::from(t0.0), Address::from(t1.0));
                    token_cache.write().await.insert(pool, Some(pair));
                    pair
                }
                _ => {
                    // Mark as non-pool so we don't retry
                    token_cache.write().await.insert(pool, None);
                    anyhow::bail!(
                        "contract call to `token0`/`token1` failed; address is likely not a pool"
                    );
                }
            }
        }
    };

    let data = log.data().data.as_ref();
    match event.dex_type.as_str() {
        "uniswap_v2" => {
            if data.len() < 128 {
                anyhow::bail!("V2 swap log data too short");
            }
            let amount0_in = U256::from_be_slice(&data[0..32]);
            if !amount0_in.is_zero() {
                event.token_in = format!("{token0:?}");
                event.token_out = format!("{token1:?}");
            } else {
                event.token_in = format!("{token1:?}");
                event.token_out = format!("{token0:?}");
            }
        }
        "uniswap_v3" => {
            if data.len() < 64 {
                anyhow::bail!("V3 swap log data too short");
            }
            let amount0_raw = U256::from_be_slice(&data[0..32]);
            let amount0_negative = amount0_raw.bit(255);
            if !amount0_negative {
                event.token_in = format!("{token0:?}");
                event.token_out = format!("{token1:?}");
            } else {
                event.token_in = format!("{token1:?}");
                event.token_out = format!("{token0:?}");
            }
        }
        other => {
            anyhow::bail!("Unsupported dex_type for token resolution: {other}");
        }
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────
// Deposit detection helpers
// ─────────────────────────────────────────────────────────────

/// Refresh the in-memory wallet lookup from database.
/// Loads all wallets belonging to common-role users.
async fn refresh_wallet_lookup(db: &sqlx::PgPool, lookup: &WalletLookup) -> Result<()> {
    let rows = sqlx::query_as::<_, (String, String, String)>(
        r#"
        SELECT w.address, w.id::text, w.user_id::text
        FROM wallets w
        JOIN users u ON u.id = w.user_id
        WHERE u.role = 'common'
        "#,
    )
    .fetch_all(db)
    .await?;

    let mut map = HashMap::with_capacity(rows.len());
    for (addr, wallet_id, user_id) in rows {
        if let Ok(parsed) = addr.parse::<Address>() {
            map.insert(parsed, (wallet_id, user_id));
        }
    }

    let count = map.len();
    *lookup.write().await = map;
    debug!(wallet_count = count, "Refreshed common-user wallet lookup");
    Ok(())
}

/// Try to parse an ERC-20 Transfer log as a deposit to a known wallet.
/// Returns `Some(DepositEvent)` if the `to` address matches a common user wallet.
async fn try_parse_deposit(
    chain_name: &str,
    log: &Log,
    wallet_lookup: &WalletLookup,
) -> Option<DepositEvent> {
    // Transfer(address indexed from, address indexed to, uint256 value)
    // topics: [sig, from, to]  data: [value]
    if log.topics().len() < 3 {
        return None;
    }
    let data = log.data().data.as_ref();
    if data.len() < 32 {
        return None;
    }

    let to_addr = Address::from_word(log.topics()[2]);

    // Check if recipient is a known common-user wallet
    let (wallet_id, user_id) = {
        let cache = wallet_lookup.read().await;
        cache.get(&to_addr)?.clone()
    };

    let from_addr = Address::from_word(log.topics()[1]);
    let amount = U256::from_be_slice(&data[0..32]);
    let token_address = format!("{:?}", log.address());
    let tx_hash = log
        .transaction_hash
        .map(|h| format!("{h:?}"))
        .unwrap_or_default();
    let block_number = log.block_number.unwrap_or(0);
    let log_index = log.log_index.unwrap_or(0) as u32;

    info!(
        chain = %chain_name,
        to = %to_addr,
        token = %token_address,
        amount = %amount,
        tx = %tx_hash,
        "Deposit detected for common user wallet"
    );

    Some(DepositEvent {
        chain: chain_name.to_string(),
        block_number,
        tx_hash,
        log_index,
        token_address,
        from: format!("{from_addr:?}"),
        to: format!("{to_addr:?}"),
        amount: amount.to_string(),
        wallet_id,
        user_id,
    })
}
