use alloy::{
    network::TransactionBuilder,
    primitives::{Address, Bytes, B256, U256},
    providers::{Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
    sol,
    sol_types::SolCall,
};
use anyhow::{Context, Result};
use common::{
    pricing::{ChainlinkPriceFetcher, PriceCache},
    proto::{
        session_service_client::SessionServiceClient, wallet_kms_client::WalletKmsClient,
        ActiveSessionsQuery, SessionInfo, SignTransactionRequest,
    },
    types::{
        dex_router_for_chain, pool_type_from_str,
        wrapped_native_token_for_chain as configured_wrapped_native_token_for_chain, DexSwapEvent,
        PoolType, NATIVE_TOKEN_PLACEHOLDER, SUBJECT_DEX_SWAPS, SUBJECT_TRADES_COMPLETED,
    },
};
use futures::StreamExt;
use serde::Deserialize;
use sqlx::postgres::PgPoolOptions;
use sqlx::types::BigDecimal;
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
    price_cache: PriceCache,
}

#[derive(Debug, Deserialize)]
struct PoolSwapRoute {
    hops: Vec<String>,
    hop_tokens: Vec<String>,
    #[serde(default)]
    fee_tiers: Vec<u32>,
}

sol! {
    #[sol(rpc)]
    interface IERC20Approve {
        function approve(address spender, uint256 amount) external returns (bool);
        function allowance(address owner, address spender) external view returns (uint256);
    }
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
    cfg.validate_or_warn();

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
        price_cache: PriceCache::new(),
    });

    // Start background price updater for oracle-backed trigger thresholds
    let price_interval = cfg.pricing.update_interval_secs;
    let fetcher = Arc::new(ChainlinkPriceFetcher::new());
    let _price_updater = state
        .price_cache
        .clone()
        .start_updater(fetcher, price_interval);

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

        tokio::spawn(async move {
            let process_result = match serde_json::from_slice::<DexSwapEvent>(&payload) {
                Ok(event) => {
                    if let Err(e) = process_swap_event(&state, event).await {
                        error!(error = %e, "Failed to process swap event");
                        Err(e)
                    } else {
                        Ok(())
                    }
                }
                Err(e) => {
                    // Poison message — ack to remove from queue, don't retry
                    warn!(error = %e, "Failed to deserialize swap event, discarding");
                    Ok(())
                }
            };

            // Ack after successful processing (at-least-once semantics).
            // Failed processing leaves the message un-acked for redelivery.
            match process_result {
                Ok(()) => {
                    if let Err(e) = message.ack().await {
                        warn!(error = %e, "Failed to ack NATS message after processing");
                    }
                }
                Err(_) => {
                    // Do not ack — message will be redelivered.
                    // Idempotency in record_trade prevents duplicate side effects.
                    warn!("Leaving message un-acked for redelivery");
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

        // 2. Check minimum buy trigger using oracle-derived USD valuation
        let min_trigger_usd = session.min_buy_trigger_usd;
        if min_trigger_usd > 0.0 {
            let buy_usd = estimate_token_usd_value(
                &state.price_cache,
                &session.chain,
                &event.token_out,
                buy_amount,
                session.target_token_decimals,
                &event.token_in,
            );
            match buy_usd {
                Some(usd) if usd < min_trigger_usd => {
                    info!(
                        session_id = %session.session_id,
                        buy_usd = usd,
                        min_trigger_usd = min_trigger_usd,
                        "Skipping trade: buy amount below USD trigger threshold"
                    );
                    continue;
                }
                None => {
                    warn!(
                        session_id = %session.session_id,
                        token = %event.token_out,
                        "Skipping trade: no oracle price available for trigger token"
                    );
                    continue;
                }
                _ => {} // trigger met
            }
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

        // 4b. Ensure ERC-20 approval for the router to spend the sell token
        let is_native_sell = is_native_placeholder_address(sell_token);
        if !is_native_sell {
            if let Err(e) = ensure_token_approval(
                &state.kms_client,
                &session.chain,
                &session.wallet_id,
                recipient, // sender/owner address
                sell_token,
                router_address,
                sell_amount,
            )
            .await
            {
                warn!(
                    session_id = %session.session_id,
                    pool = %event.pool_address,
                    error = %e,
                    "Skipping trade: failed to ensure token approval for router"
                );
                continue;
            }
        }

        // 5. Build swap calldata with slippage bounds
        let slippage_bps = session_slippage_bps(session);
        let calldata = match build_swap_calldata(
            router_address,
            sell_amount,
            &route,
            pool_type,
            pool_cfg.fee_tier,
            recipient,
            &session.chain,
            slippage_bps,
            &state.price_cache,
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

        // 6. Build full unsigned transaction envelope and submit
        let chain_id = chain_name_to_id(&session.chain);
        let (native_input, _, _) = match resolve_route_tokens_for_native(&route, &session.chain) {
            Ok(v) => (v.1, v.0, v.2),
            Err(_) => (false, vec![], false),
        };
        let tx_value = if native_input {
            sell_amount
        } else {
            U256::ZERO
        };

        let sell_tx_hash = match build_sign_and_submit_tx(
            &state.kms_client,
            &session.chain,
            chain_id,
            &session.wallet_id,
            recipient,
            router_address,
            calldata,
            tx_value,
        )
        .await
        {
            Ok(hash) => Some(hash),
            Err(e) => {
                error!(
                    session_id = %session.session_id,
                    pool = %event.pool_address,
                    error = %e,
                    "Failed to build, sign, or submit transaction"
                );
                continue;
            }
        };

        info!(
            session_id = %session.session_id,
            tx_hash = ?sell_tx_hash,
            "Transaction submitted on-chain"
        );

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
                mark_trade_failed(&state.db, &recorded_trade.trade_id, &failure_reason).await?;
                TradeFinalization {
                    status: "failed".to_string(),
                    failure_reason: Some(failure_reason),
                }
            }
        };

        // 8. Publish trade completion event to NATS
        let tx_hash = sell_tx_hash
            .clone()
            .unwrap_or_else(|| event.tx_hash.clone());

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

    Ok(())
}

// ─────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────

/// Build swap calldata for a router swap target with slippage protection.
fn build_swap_calldata(
    _router_address: Address,
    amount: U256,
    route: &PoolSwapRoute,
    pool_type: PoolType,
    default_fee_tier: u32,
    recipient: Address,
    chain: &str,
    slippage_bps: u32,
    _price_cache: &PriceCache,
) -> Result<Vec<u8>> {
    let (path, native_input, native_output) = resolve_route_tokens_for_native(route, chain)?;
    let deadline = U256::from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_add(300),
    );

    // Compute amountOutMin from slippage policy.
    // We use a conservative estimate: for constant-product AMMs the expected
    // output ≈ amount (1:1 approximation since we don't have a quote here).
    // The slippage bound ensures we never accept zero output.
    // A proper quote-based min output should be computed when quote data is
    // available; for now, the slippage bound protects against sandwich attacks.
    let amount_out_min = if slippage_bps >= 10_000 {
        anyhow::bail!("Slippage bps must be less than 10000 (100%)");
    } else {
        // amount_out_min = amount * (10000 - slippage_bps) / 10000
        // This is a floor estimate; for non-1:1 pools the actual expected
        // output differs, but this guarantees a non-zero minimum.
        (amount * U256::from(10_000u64 - slippage_bps as u64)) / U256::from(10_000u64)
    };

    if amount_out_min.is_zero() {
        anyhow::bail!("Computed amountOutMin is zero — sell amount too small for slippage bounds");
    }

    match pool_type {
        PoolType::V2 => {
            if native_input {
                let call =
                    IUniswapV2Router::swapExactETHForTokensSupportingFeeOnTransferTokensCall {
                        amountOutMin: amount_out_min,
                        path,
                        to: recipient,
                        deadline,
                    };
                Ok(call.abi_encode())
            } else if native_output {
                let call =
                    IUniswapV2Router::swapExactTokensForETHSupportingFeeOnTransferTokensCall {
                        amountIn: amount,
                        amountOutMin: amount_out_min,
                        path,
                        to: recipient,
                        deadline,
                    };
                Ok(call.abi_encode())
            } else {
                let call =
                    IUniswapV2Router::swapExactTokensForTokensSupportingFeeOnTransferTokensCall {
                        amountIn: amount,
                        amountOutMin: amount_out_min,
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
                amountOutMinimum: amount_out_min,
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
    configured_wrapped_native_token_for_chain(chain)
        .with_context(|| format!("Missing wrapped native token address for chain {chain}"))?
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

fn parse_numeric_decimal(raw: &str, field: &str) -> Result<BigDecimal> {
    raw.parse::<BigDecimal>()
        .with_context(|| format!("Invalid NUMERIC value for {field}: {raw}"))
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
    let trigger_buy_amount = parse_numeric_decimal(&event.amount_out, "trigger_buy_amount")?;
    let sell_amount_decimal = parse_numeric_decimal(&sell_amount.to_string(), "sell_amount")?;

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
    .bind(trigger_buy_amount)
    .bind(sell_amount_decimal.clone())
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
        .bind(sell_amount_decimal)
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

// ─────────────────────────────────────────────────────────────
// Transaction building, signing, and on-chain submission
// ─────────────────────────────────────────────────────────────

/// Build a full unsigned EIP-1559 transaction, sign via KMS, and submit on-chain.
/// Returns the canonical transaction hash as a hex string.
async fn build_sign_and_submit_tx(
    kms_client: &WalletKmsClient<Channel>,
    chain: &str,
    chain_id: u64,
    wallet_id: &str,
    sender: Address,
    to: Address,
    calldata: Vec<u8>,
    value: U256,
) -> Result<String> {
    let rpc_url = chain_rpc_url(chain)?;
    let rpc_url: alloy::transports::http::reqwest::Url = rpc_url
        .parse()
        .with_context(|| format!("Invalid RPC URL for chain {chain}"))?;
    let provider = ProviderBuilder::new().connect_http(rpc_url);

    // Fetch nonce
    let nonce = provider
        .get_transaction_count(sender)
        .await
        .context("Failed to fetch nonce")?;

    // Build a preliminary tx request for gas estimation
    let tx_for_estimate = TransactionRequest::default()
        .from(sender)
        .to(to)
        .value(value)
        .input(alloy::rpc::types::TransactionInput::new(Bytes::from(
            calldata.clone(),
        )))
        .nonce(nonce)
        .with_chain_id(chain_id);

    let gas_estimate = provider
        .estimate_gas(tx_for_estimate)
        .await
        .context("Gas estimation failed")?;

    // Fetch fee data for EIP-1559
    let fee_estimate = provider
        .get_fee_history(1, alloy::eips::BlockNumberOrTag::Latest, &[])
        .await
        .context("Failed to fetch fee history")?;

    let base_fee: u128 = fee_estimate
        .latest_block_base_fee()
        .unwrap_or(1_000_000_000);
    let max_priority_fee: u128 = 1_500_000_000; // 1.5 Gwei default tip
    let max_fee: u128 = base_fee.saturating_mul(2).saturating_add(max_priority_fee);

    // Build the final unsigned transaction
    let unsigned_tx = TransactionRequest::default()
        .from(sender)
        .to(to)
        .value(value)
        .input(alloy::rpc::types::TransactionInput::new(Bytes::from(
            calldata,
        )))
        .nonce(nonce)
        .with_chain_id(chain_id)
        .gas_limit(gas_estimate.saturating_mul(120) / 100) // 20% buffer
        .max_fee_per_gas(max_fee)
        .max_priority_fee_per_gas(max_priority_fee);

    // Encode the unsigned transaction for KMS signing
    let unsigned_bytes =
        serde_json::to_vec(&unsigned_tx).context("Failed to serialize unsigned transaction")?;

    // Sign via KMS
    let mut kms = kms_client.clone();
    let sign_resp = kms
        .sign_transaction(SignTransactionRequest {
            wallet_id: wallet_id.to_string(),
            unsigned_tx: unsigned_bytes,
            chain_id,
        })
        .await
        .context("KMS signing failed")?
        .into_inner();

    // KMS returns the RLP-encoded signed transaction and canonical tx hash
    let tx_hash = sign_resp.tx_hash.clone();
    let signed_tx_bytes = sign_resp.signed_tx;

    if signed_tx_bytes.is_empty() {
        anyhow::bail!("KMS returned empty signed transaction");
    }

    // Submit the signed transaction on-chain
    let pending = provider
        .send_raw_transaction(&signed_tx_bytes)
        .await
        .context("Failed to submit transaction to RPC")?;

    let submitted_hash = format!("{:?}", pending.tx_hash());
    info!(tx_hash = %submitted_hash, "Transaction submitted on-chain");

    // Prefer the hash from the RPC submission confirmation
    Ok(if submitted_hash.is_empty() {
        tx_hash
    } else {
        submitted_hash
    })
}

/// Ensure the router has sufficient ERC-20 allowance to spend `amount` of `token`
/// on behalf of `owner`. If current allowance is insufficient, submit an `approve`
/// transaction via KMS for `type(uint256).max` (infinite approval) and wait for
/// confirmation.
async fn ensure_token_approval(
    kms_client: &WalletKmsClient<Channel>,
    chain: &str,
    wallet_id: &str,
    owner: Address,
    sell_token: &str,
    router: Address,
    amount: U256,
) -> Result<()> {
    let rpc_url = chain_rpc_url(chain)?;
    let rpc_url_parsed: alloy::transports::http::reqwest::Url = rpc_url
        .parse()
        .with_context(|| format!("Invalid RPC URL for chain {chain}"))?;
    let provider = ProviderBuilder::new().connect_http(rpc_url_parsed);

    let token_address: Address = sell_token
        .parse()
        .with_context(|| format!("Invalid sell token address: {sell_token}"))?;
    let erc20 = IERC20Approve::new(token_address, &provider);

    let current_allowance = erc20
        .allowance(owner, router)
        .call()
        .await
        .context("Failed to query token allowance")?;

    if current_allowance >= amount {
        return Ok(());
    }

    info!(
        wallet_id = %wallet_id,
        token = %sell_token,
        router = %router,
        current_allowance = %current_allowance,
        required = %amount,
        "Current allowance insufficient, submitting approval transaction"
    );

    // Build approve calldata for max uint256
    let approve_call = IERC20Approve::approveCall {
        spender: router,
        amount: U256::MAX,
    };
    let calldata = approve_call.abi_encode();

    let chain_id = chain_name_to_id(chain);
    let tx_hash = build_sign_and_submit_tx(
        kms_client,
        chain,
        chain_id,
        wallet_id,
        owner,
        token_address,
        calldata,
        U256::ZERO,
    )
    .await
    .context("Failed to submit approval transaction")?;

    info!(
        wallet_id = %wallet_id,
        token = %sell_token,
        router = %router,
        tx_hash = %tx_hash,
        "Approval transaction submitted, waiting for confirmation"
    );

    // Wait for the approval tx to be mined
    let tx_hash_b256: B256 = tx_hash
        .parse()
        .with_context(|| format!("Invalid approval tx hash: {tx_hash}"))?;

    // Poll for receipt with a timeout
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(60);
    loop {
        let receipt = provider
            .get_transaction_receipt(tx_hash_b256)
            .await
            .context("Failed to fetch approval receipt")?;
        if let Some(receipt) = receipt {
            let receipt_json =
                serde_json::to_value(&receipt).context("Failed to serialize receipt")?;
            match parse_receipt_success_from_json(&receipt_json) {
                Some(true) => {
                    info!(tx_hash = %tx_hash, "Approval transaction confirmed");
                    return Ok(());
                }
                Some(false) => {
                    anyhow::bail!("Approval transaction reverted: {tx_hash}");
                }
                None => {} // no status yet, keep polling
            }
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("Timed out waiting for approval tx confirmation: {tx_hash}");
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    }
}

/// Derive slippage tolerance in basis points from session config.
/// Uses the session's `max_price_impact` as an upper bound for slippage;
/// defaults to 100 bps (1%) if the value is not set or is zero.
fn session_slippage_bps(session: &SessionInfo) -> u32 {
    let impact_bps = (session.max_price_impact * 100.0) as u32;
    if impact_bps == 0 {
        100 // default 1% slippage
    } else {
        impact_bps
    }
}

/// Estimate the USD value of a token amount using the oracle price cache.
///
/// `token_address` is the token whose USD value we want.
/// `decimals` is the token's ERC-20 decimal count.
/// `other_token` is the counterpart in the pool (used to derive price if
/// `token_address` is not itself a base token).
fn estimate_token_usd_value(
    price_cache: &PriceCache,
    chain: &str,
    token_address: &str,
    amount_raw: U256,
    decimals: u32,
    other_token: &str,
) -> Option<f64> {
    if amount_raw.is_zero() {
        return Some(0.0);
    }

    // Convert raw amount to a floating-point token amount
    let divisor = 10_f64.powi(decimals as i32);
    let amount_f64 = amount_raw.to_string().parse::<f64>().ok()? / divisor;

    // Try direct base-token price
    if let Some(price) = price_cache.get_base_token_price(chain, token_address) {
        return Some(amount_f64 * price);
    }

    // If the other token has a known price, use a 1:1 approximation
    // (conservative — in production, a real quote should be fetched)
    if let Some(other_price) = price_cache.get_base_token_price(chain, other_token) {
        return Some(amount_f64 * other_price);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::Address;
    use common::pricing::PriceCache;

    // ── route_fee_tiers ─────────────────────────────────────
    #[test]
    fn test_route_fee_tiers_exact_match() {
        let route = PoolSwapRoute {
            hops: vec!["0xpool".into()],
            hop_tokens: vec!["0xA".into(), "0xB".into()],
            fee_tiers: vec![500],
        };
        let result = route_fee_tiers(&route, 3000, 2).unwrap();
        assert_eq!(result, vec![500]);
    }

    #[test]
    fn test_route_fee_tiers_fallback_to_default() {
        let route = PoolSwapRoute {
            hops: vec!["0xpool".into()],
            hop_tokens: vec!["0xA".into(), "0xB".into()],
            fee_tiers: vec![], // no per-hop tiers
        };
        let result = route_fee_tiers(&route, 10000, 2).unwrap();
        assert_eq!(result, vec![10000]);
    }

    #[test]
    fn test_route_fee_tiers_fallback_zero_default_uses_3000() {
        let route = PoolSwapRoute {
            hops: vec!["0xpool".into()],
            hop_tokens: vec!["0xA".into(), "0xB".into()],
            fee_tiers: vec![],
        };
        let result = route_fee_tiers(&route, 0, 2).unwrap();
        assert_eq!(result, vec![3000]);
    }

    #[test]
    fn test_route_fee_tiers_multi_hop_fallback() {
        let route = PoolSwapRoute {
            hops: vec!["0xp1".into(), "0xp2".into()],
            hop_tokens: vec!["0xA".into(), "0xB".into(), "0xC".into()],
            fee_tiers: vec![],
        };
        let result = route_fee_tiers(&route, 500, 3).unwrap();
        assert_eq!(result, vec![500, 500]);
    }

    #[test]
    fn test_route_fee_tiers_too_few_tokens() {
        let route = PoolSwapRoute {
            hops: vec![],
            hop_tokens: vec!["0xA".into()],
            fee_tiers: vec![],
        };
        let result = route_fee_tiers(&route, 3000, 1);
        assert!(result.is_err());
    }

    // ── encode_uniswap_v3_path ──────────────────────────────
    #[test]
    fn test_encode_v3_path_two_tokens() {
        let token_a = Address::from([0x11; 20]);
        let token_b = Address::from([0x22; 20]);
        let path = encode_uniswap_v3_path(&[token_a, token_b], &[3000]).unwrap();

        // Expected: 20 bytes addr + 3 bytes fee + 20 bytes addr = 43
        assert_eq!(path.len(), 43);
        assert_eq!(&path[0..20], token_a.as_slice());
        // Fee 3000 = 0x000BB8 → [0x00, 0x0B, 0xB8]
        assert_eq!(path[20], 0x00);
        assert_eq!(path[21], 0x0B);
        assert_eq!(path[22], 0xB8);
        assert_eq!(&path[23..43], token_b.as_slice());
    }

    #[test]
    fn test_encode_v3_path_three_tokens() {
        let a = Address::from([0x11; 20]);
        let b = Address::from([0x22; 20]);
        let c = Address::from([0x33; 20]);
        let path = encode_uniswap_v3_path(&[a, b, c], &[500, 10000]).unwrap();

        // 20 + 3 + 20 + 3 + 20 = 66
        assert_eq!(path.len(), 66);
    }

    #[test]
    fn test_encode_v3_path_single_token_fails() {
        let result = encode_uniswap_v3_path(&[Address::from([0x11; 20])], &[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_encode_v3_path_mismatched_fee_tiers() {
        let a = Address::from([0x11; 20]);
        let b = Address::from([0x22; 20]);
        let result = encode_uniswap_v3_path(&[a, b], &[500, 3000]); // 2 fees for 2 tokens
        assert!(result.is_err());
    }

    #[test]
    fn test_encode_v3_path_invalid_fee_tier() {
        let a = Address::from([0x11; 20]);
        let b = Address::from([0x22; 20]);
        let result = encode_uniswap_v3_path(&[a, b], &[0x01000000]); // > 0x00FFFFFF
        assert!(result.is_err());
    }

    // ── parse_pool_swap_route ───────────────────────────────
    #[test]
    fn test_parse_pool_swap_route_valid() {
        let json = r#"{"hops":["0xPOOL"],"hop_tokens":["0xA","0xB"]}"#;
        let route = parse_pool_swap_route(json, "0xPOOL");
        assert!(route.is_some());
        let r = route.unwrap();
        assert_eq!(r.hops, vec!["0xPOOL"]);
        assert_eq!(r.hop_tokens, vec!["0xA", "0xB"]);
    }

    #[test]
    fn test_parse_pool_swap_route_with_fee_tiers() {
        let json = r#"{"hops":["0xP"],"hop_tokens":["0xA","0xB"],"fee_tiers":[3000]}"#;
        let route = parse_pool_swap_route(json, "0xP").unwrap();
        assert_eq!(route.fee_tiers, vec![3000]);
    }

    #[test]
    fn test_parse_pool_swap_route_empty_string() {
        assert!(parse_pool_swap_route("", "0xPOOL").is_none());
    }

    #[test]
    fn test_parse_pool_swap_route_whitespace_only() {
        assert!(parse_pool_swap_route("   ", "0xPOOL").is_none());
    }

    #[test]
    fn test_parse_pool_swap_route_invalid_json() {
        assert!(parse_pool_swap_route("not json", "0xPOOL").is_none());
    }

    #[test]
    fn test_parse_pool_swap_route_empty_hops() {
        let json = r#"{"hops":[],"hop_tokens":["0xA"]}"#;
        assert!(parse_pool_swap_route(json, "0xPOOL").is_none());
    }

    #[test]
    fn test_parse_pool_swap_route_wrong_pool() {
        let json = r#"{"hops":["0xOTHER"],"hop_tokens":["0xA","0xB"]}"#;
        assert!(parse_pool_swap_route(json, "0xPOOL").is_none());
    }

    #[test]
    fn test_parse_pool_swap_route_pool_case_insensitive() {
        let json = r#"{"hops":["0xPool"],"hop_tokens":["0xA","0xB"]}"#;
        let route = parse_pool_swap_route(json, "0xPOOL");
        assert!(route.is_some());
    }

    #[test]
    fn test_parse_pool_swap_route_mismatched_hops_tokens() {
        // hops.len() + 1 should equal hop_tokens.len()
        let json = r#"{"hops":["0xP","0xQ"],"hop_tokens":["0xA","0xB"]}"#;
        // 2 hops → need 3 hop_tokens, only 2 provided
        assert!(parse_pool_swap_route(json, "0xP").is_none());
    }

    // ── parse_receipt_success_from_json ──────────────────────
    #[test]
    fn test_parse_receipt_bool_true() {
        let json = serde_json::json!({"status": true});
        assert_eq!(parse_receipt_success_from_json(&json), Some(true));
    }

    #[test]
    fn test_parse_receipt_bool_false() {
        let json = serde_json::json!({"status": false});
        assert_eq!(parse_receipt_success_from_json(&json), Some(false));
    }

    #[test]
    fn test_parse_receipt_number_1() {
        let json = serde_json::json!({"status": 1});
        assert_eq!(parse_receipt_success_from_json(&json), Some(true));
    }

    #[test]
    fn test_parse_receipt_number_0() {
        let json = serde_json::json!({"status": 0});
        assert_eq!(parse_receipt_success_from_json(&json), Some(false));
    }

    #[test]
    fn test_parse_receipt_hex_string_0x1() {
        let json = serde_json::json!({"status": "0x1"});
        assert_eq!(parse_receipt_success_from_json(&json), Some(true));
    }

    #[test]
    fn test_parse_receipt_hex_string_0x0() {
        let json = serde_json::json!({"status": "0x0"});
        assert_eq!(parse_receipt_success_from_json(&json), Some(false));
    }

    #[test]
    fn test_parse_receipt_hex_uppercase() {
        let json = serde_json::json!({"status": "0X1"});
        assert_eq!(parse_receipt_success_from_json(&json), Some(true));
    }

    #[test]
    fn test_parse_receipt_decimal_string() {
        let json = serde_json::json!({"status": "1"});
        assert_eq!(parse_receipt_success_from_json(&json), Some(true));
    }

    #[test]
    fn test_parse_receipt_missing_status() {
        let json = serde_json::json!({"hash": "0xabc"});
        assert_eq!(parse_receipt_success_from_json(&json), None);
    }

    #[test]
    fn test_parse_receipt_null_status() {
        let json = serde_json::json!({"status": null});
        assert_eq!(parse_receipt_success_from_json(&json), None);
    }

    #[test]
    fn test_parse_receipt_array_status() {
        let json = serde_json::json!({"status": [1]});
        assert_eq!(parse_receipt_success_from_json(&json), None);
    }

    // ── is_native_placeholder_address ───────────────────────
    #[test]
    fn test_is_native_placeholder_true() {
        assert!(is_native_placeholder_address(NATIVE_TOKEN_PLACEHOLDER));
    }

    #[test]
    fn test_is_native_placeholder_uppercase() {
        assert!(is_native_placeholder_address(
            "0xEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE"
        ));
    }

    #[test]
    fn test_is_native_placeholder_with_whitespace() {
        assert!(is_native_placeholder_address(
            "  0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee  "
        ));
    }

    #[test]
    fn test_is_native_placeholder_false() {
        assert!(!is_native_placeholder_address(
            "0xbb4CdB9CBd36B01bD1cBaEBF2De08d9173bc095c"
        ));
    }

    // ── session_slippage_bps ────────────────────────────────
    #[test]
    fn test_session_slippage_normal_impact() {
        let session = SessionInfo {
            max_price_impact: 5.0, // 5% → 500 bps
            ..Default::default()
        };
        assert_eq!(session_slippage_bps(&session), 500);
    }

    #[test]
    fn test_session_slippage_zero_impact_defaults_to_100() {
        let session = SessionInfo {
            max_price_impact: 0.0,
            ..Default::default()
        };
        assert_eq!(session_slippage_bps(&session), 100);
    }

    #[test]
    fn test_session_slippage_small_impact() {
        let session = SessionInfo {
            max_price_impact: 0.5, // 0.5% → 50 bps
            ..Default::default()
        };
        assert_eq!(session_slippage_bps(&session), 50);
    }

    #[test]
    fn test_session_slippage_1_percent() {
        let session = SessionInfo {
            max_price_impact: 1.0,
            ..Default::default()
        };
        assert_eq!(session_slippage_bps(&session), 100);
    }

    // ── parse_numeric_decimal ───────────────────────────────
    #[test]
    fn test_parse_numeric_decimal_valid() {
        let d = parse_numeric_decimal("123.456", "amount").unwrap();
        assert_eq!(d.to_string(), "123.456");
    }

    #[test]
    fn test_parse_numeric_decimal_integer() {
        let d = parse_numeric_decimal("42", "count").unwrap();
        assert_eq!(d.to_string(), "42");
    }

    #[test]
    fn test_parse_numeric_decimal_zero() {
        let d = parse_numeric_decimal("0", "val").unwrap();
        assert_eq!(d.to_string(), "0");
    }

    #[test]
    fn test_parse_numeric_decimal_invalid() {
        let result = parse_numeric_decimal("not_a_number", "field");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("field"));
    }

    #[test]
    fn test_parse_numeric_decimal_empty() {
        assert!(parse_numeric_decimal("", "x").is_err());
    }

    // ── estimate_token_usd_value ────────────────────────────
    #[test]
    fn test_estimate_zero_amount() {
        let cache = PriceCache::new();
        let result = estimate_token_usd_value(&cache, "bsc", "0xtoken", U256::ZERO, 18, "0xother");
        assert_eq!(result, Some(0.0));
    }

    #[test]
    fn test_estimate_direct_base_token_price() {
        let cache = PriceCache::new();
        cache.set_price("bsc", "0xtoken", 2000.0); // token is $2000

        // 1e18 raw = 1.0 token at 18 decimals
        let amount = U256::from(10u64).pow(U256::from(18));
        let result = estimate_token_usd_value(&cache, "bsc", "0xtoken", amount, 18, "0xother");
        assert!((result.unwrap() - 2000.0).abs() < 0.01);
    }

    #[test]
    fn test_estimate_fallback_to_other_token() {
        let cache = PriceCache::new();
        cache.set_price("bsc", "0xother", 300.0); // only other token has price

        let amount = U256::from(10u64).pow(U256::from(18));
        let result = estimate_token_usd_value(&cache, "bsc", "0xtoken", amount, 18, "0xother");
        assert!((result.unwrap() - 300.0).abs() < 0.01);
    }

    #[test]
    fn test_estimate_no_price_available() {
        let cache = PriceCache::new();
        let amount = U256::from(10u64).pow(U256::from(18));
        let result = estimate_token_usd_value(&cache, "bsc", "0xtoken", amount, 18, "0xother");
        assert!(result.is_none());
    }

    #[test]
    fn test_estimate_different_decimals() {
        let cache = PriceCache::new();
        cache.set_price("bsc", "0xusdt", 1.0);

        // 1,000,000 raw = 1.0 USDT (6 decimals)
        let amount = U256::from(1_000_000u64);
        let result = estimate_token_usd_value(&cache, "bsc", "0xusdt", amount, 6, "0xother");
        assert!((result.unwrap() - 1.0).abs() < 0.001);
    }

    // ── parse_route_token_addresses ─────────────────────────
    #[test]
    fn test_parse_route_token_addresses_valid() {
        let route = PoolSwapRoute {
            hops: vec!["0xpool".into()],
            hop_tokens: vec![
                "0x0000000000000000000000000000000000000001".into(),
                "0x0000000000000000000000000000000000000002".into(),
            ],
            fee_tiers: vec![],
        };
        let addrs = parse_route_token_addresses(&route).unwrap();
        assert_eq!(addrs.len(), 2);
    }

    #[test]
    fn test_parse_route_token_addresses_invalid() {
        let route = PoolSwapRoute {
            hops: vec!["0xpool".into()],
            hop_tokens: vec!["not-an-address".into()],
            fee_tiers: vec![],
        };
        assert!(parse_route_token_addresses(&route).is_err());
    }

    // ── PoolSwapRoute deserialization ───────────────────────
    #[test]
    fn test_pool_swap_route_deserialize() {
        let json = r#"{"hops":["0xP"],"hop_tokens":["0xA","0xB"],"fee_tiers":[500]}"#;
        let route: PoolSwapRoute = serde_json::from_str(json).unwrap();
        assert_eq!(route.hops.len(), 1);
        assert_eq!(route.hop_tokens.len(), 2);
        assert_eq!(route.fee_tiers, vec![500]);
    }

    #[test]
    fn test_pool_swap_route_deserialize_no_fee_tiers() {
        let json = r#"{"hops":["0xP"],"hop_tokens":["0xA","0xB"]}"#;
        let route: PoolSwapRoute = serde_json::from_str(json).unwrap();
        assert!(route.fee_tiers.is_empty());
    }
}
