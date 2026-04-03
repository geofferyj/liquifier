use alloy::{
    primitives::{Address, B256, U256},
    providers::{Provider, ProviderBuilder},
    sol,
    sol_types::SolCall,
};
use anyhow::{Context, Result};
use common::{
    proto::{
        session_service_client::SessionServiceClient, wallet_kms_client::WalletKmsClient,
        ActiveSessionsQuery, SessionInfo, SignTransactionRequest,
    },
    types::{
        dex_router_for_chain, pool_type_from_str, DexSwapEvent, PoolType, NATIVE_TOKEN_PLACEHOLDER,
        SUBJECT_DEX_SWAPS, SUBJECT_TRADES_COMPLETED,
    },
};
use futures::StreamExt;
use serde::Deserialize;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tonic::transport::Channel;
use tracing::{error, info, warn};

mod impact;

// ─────────────────────────────────────────────────────────────
// Execution Engine Worker
// ─────────────────────────────────────────────────────────────

struct EngineState {
    db: PgPool,
    nats_js: async_nats::jetstream::Context,
    kms_client: WalletKmsClient<Channel>,
    session_client: SessionServiceClient<Channel>,
    redis: redis::aio::ConnectionManager,
}

#[derive(Debug, Deserialize)]
struct PoolSwapRoute {
    hops: Vec<String>,
    hop_tokens: Vec<String>,
    #[serde(default)]
    fee_tiers: Vec<u32>,
}

sol! {
    interface IUniswapV2Router {
        function swapExactTokensForTokens(
            uint256 amountIn,
            uint256 amountOutMin,
            address[] calldata path,
            address to,
            uint256 deadline
        ) external returns (uint256[] memory amounts);

        function swapExactTokensForTokensSupportingFeeOnTransferTokens(
            uint256 amountIn,
            uint256 amountOutMin,
            address[] calldata path,
            address to,
            uint256 deadline
        ) external;

        function swapExactETHForTokensSupportingFeeOnTransferTokens(
            uint256 amountOutMin,
            address[] calldata path,
            address to,
            uint256 deadline
        ) external payable;

        function swapExactTokensForETHSupportingFeeOnTransferTokens(
            uint256 amountIn,
            uint256 amountOutMin,
            address[] calldata path,
            address to,
            uint256 deadline
        ) external;
    }

    interface IUniswapV3Router {
        struct ExactInputParams {
            bytes path;
            address recipient;
            uint256 deadline;
            uint256 amountIn;
            uint256 amountOutMinimum;
        }

        function exactInput(ExactInputParams calldata params)
            external
            payable
            returns (uint256 amountOut);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize global config (loads config/default.yml + env overrides)
    liquifier_config::Settings::init().expect("Failed to load config");
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        // .json()
        .init();

    let cfg = liquifier_config::Settings::global();

    let database_url = &cfg.database.url;
    let nats_url = &cfg.nats.url;
    let kms_addr = cfg.kms.grpc_addr.clone();
    let session_addr = cfg.session_api.grpc_addr.clone();
    let redis_url = &cfg.redis.url;

    // Database
    let db = PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await
        .context("Failed to connect to PostgreSQL")?;

    // Redis
    let redis_url_owned = redis_url.to_string();
    let redis_conn = common::retry::retry("Redis", 10, || {
        let url = redis_url_owned.clone();
        async move {
            let client = redis::Client::open(url.as_str())?;
            redis::aio::ConnectionManager::new(client).await
        }
    })
    .await
    .context("Failed to connect to Redis")?;

    // NATS JetStream
    let nats_client =
        common::retry::retry("NATS", 10, || async { async_nats::connect(nats_url).await })
            .await
            .context("Failed to connect to NATS")?;
    let nats_js = async_nats::jetstream::new(nats_client);

    // gRPC clients
    let kms_addr_clone = kms_addr.clone();
    let kms_client = common::retry::retry("KMS gRPC", 10, || {
        let addr = kms_addr_clone.clone();
        async move { WalletKmsClient::connect(addr).await }
    })
    .await
    .context("Failed to connect to KMS gRPC")?;
    let session_addr_clone = session_addr.clone();
    let session_client = common::retry::retry("Session gRPC", 10, || {
        let addr = session_addr_clone.clone();
        async move { SessionServiceClient::connect(addr).await }
    })
    .await
    .context("Failed to connect to Session gRPC")?;

    let state = Arc::new(EngineState {
        db,
        nats_js: nats_js.clone(),
        kms_client,
        session_client,
        redis: redis_conn,
    });

    info!("Execution Engine worker starting — subscribing to DEX swap events");

    // Subscribe to the DEX_SWAPS stream as a durable consumer
    let consumer = nats_js
        .get_or_create_stream(async_nats::jetstream::stream::Config {
            name: "DEX_SWAPS".to_string(),
            subjects: vec![SUBJECT_DEX_SWAPS.to_string()],
            retention: async_nats::jetstream::stream::RetentionPolicy::WorkQueue,
            max_age: std::time::Duration::from_secs(3600),
            ..Default::default()
        })
        .await?;

    let consumer = consumer
        .get_or_create_consumer(
            "execution-engine",
            async_nats::jetstream::consumer::pull::Config {
                durable_name: Some("execution-engine".to_string()),
                ..Default::default()
            },
        )
        .await
        .context("Failed to create NATS consumer")?;

    // Process messages
    let mut messages = consumer
        .messages()
        .await
        .context("Failed to start consuming")?;

    while let Some(Ok(message)) = messages.next().await {
        let state = Arc::clone(&state);
        let payload = message.payload.to_vec();

        // Acknowledge immediately for at-most-once (fast path).
        // For at-least-once, move ack after processing.
        if let Err(e) = message.ack().await {
            warn!(error = %e, "Failed to ack NATS message");
        }

        tokio::spawn(async move {
            match serde_json::from_slice::<DexSwapEvent>(&payload) {
                Ok(event) => {
                    if let Err(e) = process_swap_event(&state, event).await {
                        error!(error = %e, "Failed to process swap event");
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Failed to deserialize swap event");
                }
            }
        });
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────
// Core event processing logic
// ─────────────────────────────────────────────────────────────

async fn process_swap_event(state: &EngineState, event: DexSwapEvent) -> Result<()> {
    // 1. Query active sessions that match this token on this chain
    let mut session_client = state.session_client.clone();
    let response = session_client
        .get_active_sessions_for_token(ActiveSessionsQuery {
            chain: event.chain.clone(),
            token_address: String::new(), // Not used when pool_address is set
            pool_address: event.pool_address.clone(), // Match sessions watching this pool
        })
        .await?
        .into_inner();

    if response.sessions.is_empty() {
        return Ok(());
    }

    let buy_amount = U256::from_str_radix(&event.amount_out, 10).unwrap_or(U256::ZERO);

    for session in &response.sessions {
        // Pool match is guaranteed by the session-api query (JOIN on session_pools)

        // 2. Check minimum buy trigger (simplified — production would use a price oracle)
        let min_trigger =
            U256::from(session.min_buy_trigger_usd as u64) * U256::from(10).pow(U256::from(18));
        if buy_amount < min_trigger {
            continue;
        }

        // 3. Calculate our sell amount = buy_amount * (pov_percent / 100)
        let pov_bps = (session.pov_percent * 100.0) as u64; // convert % to basis points
        let sell_amount = (buy_amount * U256::from(pov_bps)) / U256::from(10_000);

        // Check remaining session balance
        let total = U256::from_str_radix(&session.total_amount, 10).unwrap_or(U256::ZERO);
        let sold = U256::from_str_radix(&session.amount_sold, 10).unwrap_or(U256::ZERO);
        let remaining = total.saturating_sub(sold);

        if remaining.is_zero() {
            continue;
        }

        let sell_amount = sell_amount.min(remaining);

        let Some(pool_cfg) = session
            .pools
            .iter()
            .find(|p| p.pool_address.eq_ignore_ascii_case(&event.pool_address))
        else {
            warn!(
                session_id = %session.session_id,
                pool = %event.pool_address,
                "Skipping trade: pool metadata missing from session"
            );
            continue;
        };

        let Some(pool_type) = pool_type_from_str(&pool_cfg.pool_type) else {
            warn!(
                session_id = %session.session_id,
                pool = %event.pool_address,
                pool_type = %pool_cfg.pool_type,
                "Skipping trade: unsupported pool type"
            );
            continue;
        };

        let Some(route) = parse_pool_swap_route(&pool_cfg.swap_path_json, &event.pool_address)
        else {
            warn!(
                session_id = %session.session_id,
                pool = %event.pool_address,
                "Skipping trade: missing or invalid per-pool route"
            );
            continue;
        };

        let Some(sell_token) = route.hop_tokens.first() else {
            warn!(
                session_id = %session.session_id,
                pool = %event.pool_address,
                "Skipping trade: route missing sell token"
            );
            continue;
        };

        // 4. Calculate price impact using on-chain pool data.
        let price_impact_bps = match impact::calculate_price_impact(
            &session.chain,
            &event.pool_address,
            pool_type,
            sell_token,
            sell_amount,
        )
        .await
        {
            Ok(value) => value,
            Err(e) => {
                warn!(
                    session_id = %session.session_id,
                    pool = %event.pool_address,
                    error = %e,
                    "Skipping trade: failed to compute price impact"
                );
                continue;
            }
        };

        let session_max_impact_bps = (session.max_price_impact * 100.0) as u32;
        let global_max_impact_bps = liquifier_config::Settings::global()
            .execution
            .max_price_impact_bps;
        let max_impact_bps = session_max_impact_bps.min(global_max_impact_bps);
        if price_impact_bps > max_impact_bps {
            warn!(
                session_id = %session.session_id,
                impact_bps = price_impact_bps,
                session_max_bps = session_max_impact_bps,
                global_max_bps = global_max_impact_bps,
                effective_max_bps = max_impact_bps,
                "Skipping trade: price impact exceeds threshold"
            );
            continue;
        }

        info!(
            session_id = %session.session_id,
            sell_amount = %sell_amount,
            impact_bps = price_impact_bps,
            max_bps = max_impact_bps,
            pool = %event.pool_address,
            "Executing POV trade"
        );

        let Some(router_address) =
            dex_router_for_chain(&session.chain, &pool_cfg.dex_name, pool_type)
        else {
            warn!(
                session_id = %session.session_id,
                chain = %session.chain,
                dex = %pool_cfg.dex_name,
                pool_type = %pool_cfg.pool_type,
                "Skipping trade: no configured router for chain/dex/pool_type"
            );
            continue;
        };

        let Ok(router_address) = router_address.parse::<Address>() else {
            warn!(
                session_id = %session.session_id,
                chain = %session.chain,
                dex = %pool_cfg.dex_name,
                "Skipping trade: configured router address is invalid"
            );
            continue;
        };

        let recipient =
            match resolve_session_wallet_address(&state.db, &session.wallet_id, &session.chain)
                .await
            {
                Ok(address) => address,
                Err(e) => {
                    warn!(
                        session_id = %session.session_id,
                        wallet_id = %session.wallet_id,
                        error = %e,
                        "Skipping trade: failed to resolve session wallet recipient"
                    );
                    continue;
                }
            };

        // 5. Build and sign the transaction via KMS
        let tx_bytes = match build_swap_calldata(
            router_address,
            sell_amount,
            &route,
            pool_type,
            pool_cfg.fee_tier,
            recipient,
            &session.chain,
        ) {
            Ok(tx) => tx,
            Err(e) => {
                warn!(
                    session_id = %session.session_id,
                    pool = %event.pool_address,
                    error = %e,
                    "Skipping trade: failed to build router calldata"
                );
                continue;
            }
        };

        let mut kms_client = state.kms_client.clone();
        let sign_response = kms_client
            .sign_transaction(SignTransactionRequest {
                wallet_id: session.wallet_id.clone(),
                unsigned_tx: tx_bytes,
                chain_id: chain_name_to_id(&session.chain),
            })
            .await;

        match sign_response {
            Ok(resp) => {
                let signed = resp.into_inner();
                info!(
                    session_id = %session.session_id,
                    "Transaction signed, submitting..."
                );

                // 6. Submit transaction (placeholder — production uses Flashbots/MEV-share)
                // submit_transaction(&signed.signed_tx, &session.chain).await?;

                let sell_tx_hash = if signed.tx_hash.trim().is_empty() {
                    None
                } else {
                    Some(signed.tx_hash.clone())
                };

                // 7. Record trade in DB
                let recorded_trade = record_trade(
                    &state.db,
                    session,
                    &event,
                    sell_amount,
                    price_impact_bps,
                    sell_tx_hash.as_deref(),
                )
                .await?;

                if !recorded_trade.inserted {
                    info!(
                        session_id = %session.session_id,
                        trade_id = %recorded_trade.trade_id,
                        trigger_tx = %event.tx_hash,
                        trigger_log_index = event.log_index,
                        "Skipping duplicate trigger event"
                    );
                    continue;
                }

                let finalization = match finalize_trade_status_from_receipt(
                    &state.db,
                    &session.chain,
                    &recorded_trade.trade_id,
                    sell_tx_hash.as_deref(),
                )
                .await
                {
                    Ok(result) => result,
                    Err(e) => {
                        error!(
                            session_id = %session.session_id,
                            trade_id = %recorded_trade.trade_id,
                            error = %e,
                            "Failed to finalize trade status from receipt"
                        );
                        let failure_reason = format!("receipt_finalization_error: {e}");
                        mark_trade_failed(&state.db, &recorded_trade.trade_id, &failure_reason)
                            .await?;
                        TradeFinalization {
                            status: "failed".to_string(),
                            failure_reason: Some(failure_reason),
                        }
                    }
                };

                // 8. Publish trade completion event to NATS
                let tx_hash = if signed.tx_hash.trim().is_empty() {
                    event.tx_hash.clone()
                } else {
                    signed.tx_hash.clone()
                };

                let trade_event = serde_json::json!({
                    "type": "trade_completed",
                    "trade_id": recorded_trade.trade_id,
                    "session_id": session.session_id,
                    "chain": session.chain,
                    "sell_amount": sell_amount.to_string(),
                    "received_amount": event.amount_out,
                    "tx_hash": tx_hash,
                    "price_impact_bps": price_impact_bps,
                    "executed_at": recorded_trade.executed_at,
                    "status": finalization.status,
                    "failure_reason": finalization.failure_reason,
                });
                let _ = state
                    .nats_js
                    .publish(
                        SUBJECT_TRADES_COMPLETED,
                        serde_json::to_vec(&trade_event)?.into(),
                    )
                    .await;
            }
            Err(e) => {
                error!(
                    session_id = %session.session_id,
                    error = %e,
                    "Failed to sign transaction"
                );
            }
        }
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────

/// Build swap calldata for a router swap target.
fn build_swap_calldata(
    _router_address: Address,
    amount: U256,
    route: &PoolSwapRoute,
    pool_type: PoolType,
    default_fee_tier: u32,
    recipient: Address,
    chain: &str,
) -> Result<Vec<u8>> {
    let (path, native_input, native_output) = resolve_route_tokens_for_native(route, chain)?;
    let deadline = U256::from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_add(300),
    );

    match pool_type {
        PoolType::V2 => {
            if native_input {
                let call =
                    IUniswapV2Router::swapExactETHForTokensSupportingFeeOnTransferTokensCall {
                        amountOutMin: U256::ZERO,
                        path,
                        to: recipient,
                        deadline,
                    };
                Ok(call.abi_encode())
            } else if native_output {
                let call =
                    IUniswapV2Router::swapExactTokensForETHSupportingFeeOnTransferTokensCall {
                        amountIn: amount,
                        amountOutMin: U256::ZERO,
                        path,
                        to: recipient,
                        deadline,
                    };
                Ok(call.abi_encode())
            } else {
                let call =
                    IUniswapV2Router::swapExactTokensForTokensSupportingFeeOnTransferTokensCall {
                        amountIn: amount,
                        amountOutMin: U256::ZERO,
                        path,
                        to: recipient,
                        deadline,
                    };
                Ok(call.abi_encode())
            }
        }
        PoolType::V3 => {
            let fee_tiers = route_fee_tiers(route, default_fee_tier, path.len())?;
            let encoded_path = encode_uniswap_v3_path(&path, &fee_tiers)?;

            let params = IUniswapV3Router::ExactInputParams {
                path: encoded_path.into(),
                recipient,
                deadline,
                amountIn: amount,
                amountOutMinimum: U256::ZERO,
            };
            let call = IUniswapV3Router::exactInputCall { params };
            Ok(call.abi_encode())
        }
    }
}

fn parse_route_token_addresses(route: &PoolSwapRoute) -> Result<Vec<Address>> {
    route
        .hop_tokens
        .iter()
        .map(|token| {
            token
                .parse::<Address>()
                .with_context(|| format!("Invalid token address in route: {token}"))
        })
        .collect()
}

fn resolve_route_tokens_for_native(
    route: &PoolSwapRoute,
    chain: &str,
) -> Result<(Vec<Address>, bool, bool)> {
    let mut path = parse_route_token_addresses(route)?;

    let native_input = route
        .hop_tokens
        .first()
        .map(|token| is_native_placeholder_address(token))
        .unwrap_or(false);
    let native_output = route
        .hop_tokens
        .last()
        .map(|token| is_native_placeholder_address(token))
        .unwrap_or(false);

    if route
        .hop_tokens
        .iter()
        .any(|token| is_native_placeholder_address(token))
    {
        let wrapped_native = wrapped_native_token_for_chain(chain)?;
        for (idx, token) in route.hop_tokens.iter().enumerate() {
            if is_native_placeholder_address(token) {
                if let Some(slot) = path.get_mut(idx) {
                    *slot = wrapped_native;
                }
            }
        }
    }

    Ok((path, native_input, native_output))
}

fn is_native_placeholder_address(token: &str) -> bool {
    token.trim().eq_ignore_ascii_case(NATIVE_TOKEN_PLACEHOLDER)
}

fn wrapped_native_token_for_chain(chain: &str) -> Result<Address> {
    let settings = liquifier_config::Settings::global();
    let chain_cfg = settings
        .chains
        .get(chain)
        .with_context(|| format!("Missing chain config for {chain}"))?;

    const WRAPPED_NATIVE_SYMBOLS: [&str; 11] = [
        "WETH", "WBNB", "WMATIC", "WAVAX", "WFTM", "WONE", "WCELO", "WGLMR", "WKLAY", "WOKT",
        "WSEI",
    ];

    let wrapped = chain_cfg
        .base_tokens
        .iter()
        .find(|token| {
            let symbol = token.symbol.trim().to_ascii_uppercase();
            WRAPPED_NATIVE_SYMBOLS.contains(&symbol.as_str())
        })
        .with_context(|| {
            format!("Missing wrapped native token symbol in base_tokens for {chain}")
        })?;

    wrapped
        .address
        .parse::<Address>()
        .with_context(|| format!("Invalid wrapped native token address for {chain}"))
}

fn route_fee_tiers(
    route: &PoolSwapRoute,
    default_fee_tier: u32,
    token_count: usize,
) -> Result<Vec<u32>> {
    if token_count < 2 {
        anyhow::bail!("Route must include at least two hop tokens");
    }

    let expected_fees = token_count.saturating_sub(1);
    if route.fee_tiers.len() == expected_fees {
        return Ok(route.fee_tiers.clone());
    }

    // Fallback: when per-hop fee tiers are missing from the route payload,
    // apply the selected pool fee tier for all hops (or 3000 if unspecified).
    let fee = if default_fee_tier == 0 {
        3000
    } else {
        default_fee_tier
    };
    Ok(vec![fee; expected_fees])
}

fn encode_uniswap_v3_path(tokens: &[Address], fee_tiers: &[u32]) -> Result<Vec<u8>> {
    if tokens.len() < 2 {
        anyhow::bail!("V3 path requires at least two tokens");
    }
    if fee_tiers.len() != tokens.len().saturating_sub(1) {
        anyhow::bail!("V3 path fee_tiers length must equal tokens length - 1");
    }

    let mut encoded = Vec::with_capacity(tokens.len() * 20 + fee_tiers.len() * 3);

    for (idx, fee) in fee_tiers.iter().enumerate() {
        if *fee > 0x00FF_FFFF {
            anyhow::bail!("Invalid V3 fee tier: {fee}");
        }

        encoded.extend_from_slice(tokens[idx].as_slice());
        encoded.push((fee >> 16) as u8);
        encoded.push((fee >> 8) as u8);
        encoded.push(*fee as u8);
    }

    encoded.extend_from_slice(tokens[tokens.len() - 1].as_slice());
    Ok(encoded)
}

fn parse_pool_swap_route(raw: &str, pool_address: &str) -> Option<PoolSwapRoute> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let route = serde_json::from_str::<PoolSwapRoute>(trimmed).ok()?;
    if route.hops.is_empty() {
        return None;
    }

    if !route.hops[0].eq_ignore_ascii_case(pool_address) {
        return None;
    }

    if route.hop_tokens.len() != route.hops.len().saturating_add(1) {
        return None;
    }

    Some(route)
}

fn chain_name_to_id(name: &str) -> u64 {
    liquifier_config::chain_name_to_id(&liquifier_config::Settings::global().chains, name)
}

async fn resolve_session_wallet_address(
    db: &PgPool,
    wallet_id: &str,
    chain: &str,
) -> Result<Address> {
    let wallet_id: uuid::Uuid = wallet_id
        .parse()
        .with_context(|| format!("Invalid wallet_id: {wallet_id}"))?;

    let row = sqlx::query_as::<_, (String, String)>(
        r#"
        SELECT address, chain::text
        FROM wallets
        WHERE id = $1
        "#,
    )
    .bind(wallet_id)
    .fetch_optional(db)
    .await
    .context("Failed to query wallet recipient")?
    .with_context(|| format!("Wallet not found for wallet_id: {wallet_id}"))?;

    if !row.1.eq_ignore_ascii_case(chain) {
        anyhow::bail!(
            "Wallet chain {} does not match session chain {}",
            row.1,
            chain
        );
    }

    row.0
        .parse::<Address>()
        .with_context(|| format!("Invalid wallet address in DB: {}", row.0))
}

struct RecordedTrade {
    trade_id: String,
    executed_at: String,
    inserted: bool,
}

struct TradeFinalization {
    status: String,
    failure_reason: Option<String>,
}

async fn record_trade(
    db: &PgPool,
    session: &SessionInfo,
    event: &DexSwapEvent,
    sell_amount: U256,
    price_impact_bps: u32,
    sell_tx_hash: Option<&str>,
) -> Result<RecordedTrade> {
    let session_id: uuid::Uuid = session.session_id.parse()?;
    let inserted = sqlx::query_as::<_, (String, chrono::DateTime<chrono::Utc>)>(
        r#"
        INSERT INTO trades (
            session_id, chain, status, trigger_tx_hash, trigger_pool,
            trigger_log_index, trigger_buy_amount, sell_amount, sell_tx_hash,
            sell_pool, price_impact_bps, executed_at
        )
        VALUES ($1, $2::chain_id, 'submitted', $3, $4, $5, $6, $7, $8, $9, $10, NOW())
        ON CONFLICT (session_id, trigger_tx_hash, trigger_pool, trigger_log_index) DO NOTHING
        RETURNING id::text, executed_at
        "#,
    )
    .bind(session_id)
    .bind(&session.chain)
    .bind(&event.tx_hash)
    .bind(&event.pool_address)
    .bind(event.log_index as i32)
    .bind(event.amount_out.parse::<i64>().unwrap_or(0)) // Simplified; production uses NUMERIC
    .bind(sell_amount.to_string().parse::<i64>().unwrap_or(0))
    .bind(sell_tx_hash)
    .bind(&event.pool_address)
    .bind(price_impact_bps as i32)
    .fetch_optional(db)
    .await
    .context("Failed to record trade")?;

    if let Some((trade_id, executed_at)) = inserted {
        // Update session amount_sold only when the trade row is newly inserted.
        sqlx::query(
            r#"
            UPDATE sessions SET amount_sold = amount_sold + $1
            WHERE id = $2
            "#,
        )
        .bind(sell_amount.to_string().parse::<i64>().unwrap_or(0))
        .bind(session_id)
        .execute(db)
        .await
        .context("Failed to update session")?;

        return Ok(RecordedTrade {
            trade_id,
            executed_at: executed_at.to_rfc3339(),
            inserted: true,
        });
    }

    // Trade already exists for this trigger (idempotent retry path).
    let (trade_id, executed_at): (String, chrono::DateTime<chrono::Utc>) = sqlx::query_as(
        r#"
        SELECT id::text, COALESCE(executed_at, created_at)
        FROM trades
        WHERE session_id = $1
          AND trigger_tx_hash = $2
          AND trigger_pool = $3
          AND trigger_log_index = $4
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(session_id)
    .bind(&event.tx_hash)
    .bind(&event.pool_address)
    .bind(event.log_index as i32)
    .fetch_one(db)
    .await
    .context("Failed to fetch existing trade for duplicate trigger")?;

    Ok(RecordedTrade {
        trade_id,
        executed_at: executed_at.to_rfc3339(),
        inserted: false,
    })
}

async fn finalize_trade_status_from_receipt(
    db: &PgPool,
    chain: &str,
    trade_id: &str,
    sell_tx_hash: Option<&str>,
) -> Result<TradeFinalization> {
    let Some(tx_hash) = sell_tx_hash.map(str::trim).filter(|h| !h.is_empty()) else {
        return Ok(TradeFinalization {
            status: "submitted".to_string(),
            failure_reason: None,
        });
    };

    match fetch_receipt_success(chain, tx_hash).await {
        Ok(Some(true)) => {
            mark_trade_confirmed(db, trade_id).await?;
            Ok(TradeFinalization {
                status: "confirmed".to_string(),
                failure_reason: None,
            })
        }
        Ok(Some(false)) => {
            let reason = "transaction_reverted".to_string();
            mark_trade_failed(db, trade_id, &reason).await?;
            Ok(TradeFinalization {
                status: "failed".to_string(),
                failure_reason: Some(reason),
            })
        }
        Ok(None) => Ok(TradeFinalization {
            status: "submitted".to_string(),
            failure_reason: None,
        }),
        Err(e) => {
            let reason = format!("receipt_lookup_error: {e}");
            mark_trade_failed(db, trade_id, &reason).await?;
            Ok(TradeFinalization {
                status: "failed".to_string(),
                failure_reason: Some(reason),
            })
        }
    }
}

async fn mark_trade_confirmed(db: &PgPool, trade_id: &str) -> Result<()> {
    let trade_id: uuid::Uuid = trade_id.parse()?;
    sqlx::query(
        r#"
        UPDATE trades
        SET status = 'confirmed',
            failure_reason = NULL
        WHERE id = $1
        "#,
    )
    .bind(trade_id)
    .execute(db)
    .await
    .context("Failed to mark trade confirmed")?;
    Ok(())
}

async fn mark_trade_failed(db: &PgPool, trade_id: &str, reason: &str) -> Result<()> {
    let trade_id: uuid::Uuid = trade_id.parse()?;
    sqlx::query(
        r#"
        UPDATE trades
        SET status = 'failed',
            failure_reason = $1
        WHERE id = $2
          AND status <> 'confirmed'
        "#,
    )
    .bind(reason)
    .bind(trade_id)
    .execute(db)
    .await
    .context("Failed to mark trade failed")?;
    Ok(())
}

async fn fetch_receipt_success(chain: &str, tx_hash: &str) -> Result<Option<bool>> {
    let rpc_url = chain_rpc_url(chain)?;
    let rpc_url: alloy::transports::http::reqwest::Url = rpc_url
        .parse()
        .with_context(|| format!("Invalid RPC URL for chain {chain}"))?;

    let provider = ProviderBuilder::new().connect_http(rpc_url);
    let tx_hash: B256 = tx_hash
        .parse()
        .with_context(|| format!("Invalid tx hash: {tx_hash}"))?;

    let receipt = provider
        .get_transaction_receipt(tx_hash)
        .await
        .context("Failed to query transaction receipt")?;
    let Some(receipt) = receipt else {
        return Ok(None);
    };

    let receipt_json = serde_json::to_value(receipt).context("Failed to serialize receipt")?;
    Ok(parse_receipt_success_from_json(&receipt_json))
}

fn parse_receipt_success_from_json(receipt: &serde_json::Value) -> Option<bool> {
    let status = receipt.get("status")?;
    match status {
        serde_json::Value::Bool(v) => Some(*v),
        serde_json::Value::Number(v) => v.as_u64().map(|n| n != 0),
        serde_json::Value::String(v) => {
            let raw = v.trim();
            if raw.starts_with("0x") || raw.starts_with("0X") {
                u64::from_str_radix(&raw[2..], 16).ok().map(|n| n != 0)
            } else {
                raw.parse::<u64>().ok().map(|n| n != 0)
            }
        }
        _ => None,
    }
}

fn chain_rpc_url(chain: &str) -> Result<String> {
    let settings = liquifier_config::Settings::global();
    let chain_cfg = settings
        .chains
        .get(chain)
        .with_context(|| format!("Missing chain config for {chain}"))?;
    let rpc = chain_cfg.rpc_url.trim();
    if rpc.is_empty() {
        anyhow::bail!("RPC URL is not configured for chain {chain}");
    }
    Ok(rpc.to_string())
}
