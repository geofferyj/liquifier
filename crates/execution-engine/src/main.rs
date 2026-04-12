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
use sqlx::{PgPool, Row};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tonic::transport::Channel;
use tracing::{debug, error, info, warn};

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
    buy_trigger_oracle: Address,
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
        function balanceOf(address account) external view returns (uint256);
        function transfer(address to, uint256 amount) external returns (bool);
    }
}

sol! {
    #[sol(rpc)]
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

        function getAmountsOut(
            uint256 amountIn,
            address[] calldata path
        ) external view returns (uint256[] memory amounts);
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

sol! {
    #[sol(rpc)]
    interface IPriceGetterV3 {
        function getUsdPrice(
            address token,
            address pair,
            uint8 version
        ) external view returns (uint256);
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
    let buy_trigger_oracle: Address = cfg
        .execution
        .buy_trigger_oracle_address
        .parse()
        .with_context(|| {
            format!(
                "Invalid execution.buy_trigger_oracle_address: {}",
                cfg.execution.buy_trigger_oracle_address
            )
        })?;

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
        buy_trigger_oracle,
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

        tokio::spawn(async move {
            let process_result = match serde_json::from_slice::<DexSwapEvent>(&payload) {
                Ok(event) => {
                    let block_number = event.block_number;
                    if let Err(e) = process_swap_event(&state, event).await {
                        error!(block_number = block_number, error = %e, "Failed to process swap event");
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

        let Some(pool_cfg) = session
            .pools
            .iter()
            .find(|p| p.pool_address.eq_ignore_ascii_case(&event.pool_address))
        else {
            warn!(
                session_id = %session.session_id,
                block_number = event.block_number,
                pool = %event.pool_address,
                "Skipping trade: pool config missing, cannot determine swap direction"
            );
            continue;
        };

        // ── Direction check: only trigger when someone BOUGHT the sell_token ──
        // The session sells `sell_token` proportionally when others buy it.
        // Resolve which token is amount_out using the pool's token0/token1 config
        // and the event's token0_is_input flag from the indexer.
        // token0_is_input=true  → amount_out is token1
        // token0_is_input=false → amount_out is token0
        let resolved_token_out = if event.token0_is_input {
            pool_cfg.token1.as_str()
        } else {
            pool_cfg.token0.as_str()
        };

        // Only trigger our sell if the on-chain swap's output is the session's sell_token.
        // That means someone BOUGHT sell_token, which is the signal for us to sell too.
        if !resolved_token_out.eq_ignore_ascii_case(&session.sell_token) {
            debug!(
                session_id = %session.session_id,
                block_number = event.block_number,
                resolved_token_out = %resolved_token_out,
                sell_token = %session.sell_token,
                pool = %event.pool_address,
                "Skipping: swap output is not the sell token (not a buy of sell_token)"
            );
            continue;
        }

        let Some(pool_type) = pool_type_from_str(&pool_cfg.pool_type) else {
            warn!(
                session_id = %session.session_id,
                block_number = event.block_number,
                pool = %event.pool_address,
                pool_type = %pool_cfg.pool_type,
                "Skipping trade: unsupported pool type"
            );
            continue;
        };

        // 2. Check minimum buy trigger using oracle-derived USD valuation
        let min_trigger_usd = session.min_buy_trigger_usd;
        if min_trigger_usd > 0.0 {
            let buy_usd = match fetch_buy_amount_usd_from_oracle(
                &session.chain,
                state.buy_trigger_oracle,
                resolved_token_out,
                &event.pool_address,
                pool_type_to_oracle_version(pool_type),
                buy_amount,
                session.sell_token_decimals,
            )
            .await
            {
                Ok(Some(usd)) => usd,
                Ok(None) => {
                    warn!(
                        session_id = %session.session_id,
                        block_number = event.block_number,
                        token = %resolved_token_out,
                        "Skipping trade: unusable oracle response for trigger token"
                    );
                    continue;
                }
                Err(e) => {
                    warn!(
                        session_id = %session.session_id,
                        block_number = event.block_number,
                        token = %resolved_token_out,
                        error = %e,
                        "Skipping trade: failed to fetch oracle USD price for trigger token"
                    );
                    continue;
                }
            };

            if buy_usd < min_trigger_usd {
                info!(
                    session_id = %session.session_id,
                    block_number = event.block_number,
                    buy_usd = buy_usd,
                    min_trigger_usd = min_trigger_usd,
                    "Skipping trade: buy amount below USD trigger threshold"
                );
                continue;
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

        let Some(route) = parse_pool_swap_route(&pool_cfg.swap_path_json, &event.pool_address)
        else {
            warn!(
                session_id = %session.session_id,
                block_number = event.block_number,
                pool = %event.pool_address,
                "Skipping trade: missing or invalid per-pool route"
            );
            continue;
        };

        let Some(sell_token) = route.hop_tokens.first() else {
            warn!(
                session_id = %session.session_id,
                block_number = event.block_number,
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
                    block_number = event.block_number,
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
                block_number = event.block_number,
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
            block_number = event.block_number,
            buy_tx = %event.tx_hash,
            buy_amount = %event.amount_out,
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
                block_number = event.block_number,
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
                block_number = event.block_number,
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
                        block_number = event.block_number,
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
                    block_number = event.block_number,
                    pool = %event.pool_address,
                    error = %e,
                    "Skipping trade: failed to ensure token approval for router"
                );
                continue;
            }
        }

        // 5. Get on-chain quote and build swap calldata with slippage bounds
        let expected_output = match get_on_chain_quote(
            &session.chain,
            router_address,
            sell_amount,
            &route,
            pool_type,
        )
        .await
        {
            Ok(quoted) => {
                info!(
                    session_id = %session.session_id,
                    block_number = event.block_number,
                    sell_amount = %sell_amount,
                    quoted_output = %quoted,
                    pool = %event.pool_address,
                    "On-chain quote received"
                );
                quoted
            }
            Err(e) => {
                warn!(
                    session_id = %session.session_id,
                    block_number = event.block_number,
                    pool = %event.pool_address,
                    error = %e,
                    "On-chain quote failed, skipping trade"
                );
                continue;
            }
        };

        let slippage_bps = session_slippage_bps(session);
        let calldata = match build_swap_calldata(
            router_address,
            sell_amount,
            expected_output,
            &route,
            pool_type,
            pool_cfg.fee_tier,
            recipient,
            &session.chain,
            slippage_bps,
        ) {
            Ok(tx) => tx,
            Err(e) => {
                warn!(
                    session_id = %session.session_id,
                    block_number = event.block_number,
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
                    block_number = event.block_number,
                    pool = %event.pool_address,
                    error = %e,
                    "Failed to build, sign, or submit transaction"
                );
                continue;
            }
        };

        info!(
            session_id = %session.session_id,
            block_number = event.block_number,
            buy_tx = %event.tx_hash,
            sell_tx = ?sell_tx_hash,
            sell_amount = %sell_amount,
            "Trade pair: buy_tx → sell_tx"
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
                block_number = event.block_number,
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
            &session.target_token,
            recipient,
        )
        .await
        {
            Ok(result) => result,
            Err(e) => {
                error!(
                    session_id = %session.session_id,
                    block_number = event.block_number,
                    trade_id = %recorded_trade.trade_id,
                    error = %e,
                    "Failed to finalize trade status from receipt"
                );
                let failure_reason = format!("receipt_finalization_error: {e}");
                mark_trade_failed(&state.db, &recorded_trade.trade_id, &failure_reason).await?;
                TradeFinalization {
                    status: "failed".to_string(),
                    failure_reason: Some(failure_reason),
                    received_amount: None,
                }
            }
        };

        // 8. Publish trade completion event to NATS
        let tx_hash = sell_tx_hash
            .clone()
            .unwrap_or_else(|| event.tx_hash.clone());

        // Use actual received amount from receipt parsing; fall back to "0" if unknown
        let received_amount = finalization
            .received_amount
            .clone()
            .unwrap_or_else(|| "0".to_string());

        let trade_event = serde_json::json!({
            "type": "trade_completed",
            "trade_id": recorded_trade.trade_id,
            "session_id": session.session_id,
            "chain": session.chain,
            "sell_amount": sell_amount.to_string(),
            "received_amount": received_amount,
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

        // 9. Check if session is now fully sold → auto-complete and sweep funds
        if recorded_trade.inserted {
            let new_sold = sold + sell_amount; // sold was read earlier
            if new_sold >= total {
                info!(
                    session_id = %session.session_id,
                    "Session fully sold — marking completed and sweeping funds to investor wallet"
                );

                // Mark session completed
                let session_id_uuid: uuid::Uuid = session.session_id.parse().unwrap_or_default();
                let _ = sqlx::query(
                    "UPDATE sessions SET status = 'completed', updated_at = NOW() WHERE id = $1",
                )
                .bind(session_id_uuid)
                .execute(&state.db)
                .await;

                // Calculate total received USDT for shortfall check
                let total_received_row = sqlx::query_scalar::<_, String>(
                    "SELECT COALESCE(SUM(CAST(final_received AS NUMERIC)), 0)::text FROM trades WHERE session_id = $1 AND status = 'confirmed'",
                )
                .bind(session_id_uuid)
                .fetch_one(&state.db)
                .await;

                // Check for shortfall: compare received USDT to initial token value
                if let Ok(total_received_str) = total_received_row {
                    let cfg = liquifier_config::Settings::global();
                    let min_deposit_usd = cfg.execution.min_deposit_amount_usd;

                    // Convert total received (in wei) to float USD  (target is USDT, 18 decimals)
                    let received_f64 = total_received_str.parse::<f64>().unwrap_or(0.0) / 1e18;

                    if received_f64 < min_deposit_usd && received_f64 > 0.0 {
                        let shortfall = min_deposit_usd - received_f64;
                        warn!(
                            session_id = %session.session_id,
                            received_usd = received_f64,
                            required_usd = min_deposit_usd,
                            shortfall_usd = shortfall,
                            "Session completed with shortfall — alerting user"
                        );

                        // Look up user email for shortfall alert
                        let user_row = sqlx::query(
                            "SELECT u.email, COALESCE(u.username, u.email) AS display_name
                             FROM sessions s JOIN wallets w ON w.id = s.wallet_id
                             JOIN users u ON u.id = w.user_id
                             WHERE s.id = $1 AND u.role = 'common'",
                        )
                        .bind(session_id_uuid)
                        .fetch_optional(&state.db)
                        .await;

                        if let Ok(Some(row)) = user_row {
                            let email: String = row.get("email");
                            let name: String = row.get("display_name");
                            let _ = state
                                .nats_js
                                .publish(
                                    "session.shortfall",
                                    serde_json::to_vec(&serde_json::json!({
                                        "type": "shortfall_alert",
                                        "session_id": session.session_id,
                                        "user_email": email,
                                        "username": name,
                                        "received_usd": received_f64,
                                        "required_usd": min_deposit_usd,
                                        "shortfall_usd": shortfall,
                                    }))
                                    .unwrap_or_default()
                                    .into(),
                                )
                                .await;
                        }
                    }
                }

                // Sweep converted funds to investor wallet
                let investor_addr = {
                    let cfg = liquifier_config::Settings::global();
                    cfg.execution.investor_wallet.clone()
                };

                if investor_addr.is_empty() {
                    warn!(
                        session_id = %session.session_id,
                        "investor_wallet not configured — skipping fund sweep"
                    );
                } else if let Err(e) =
                    sweep_to_investor(&state.kms_client, &state.db, session, &investor_addr).await
                {
                    error!(
                        session_id = %session.session_id,
                        error = %e,
                        "Failed to sweep funds to investor wallet"
                    );
                }

                // Check for excess sell token remaining and auto-create refund request
                check_and_create_excess_refund(&state.kms_client, &state.db, session).await;
            }
        }
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────
// Post-completion fund sweep
// ─────────────────────────────────────────────────────────────

/// Transfer the session wallet's target-token balance to the configured investor wallet.
async fn sweep_to_investor(
    kms_client: &WalletKmsClient<Channel>,
    db: &PgPool,
    session: &SessionInfo,
    investor_addr_str: &str,
) -> Result<()> {
    let investor: Address = investor_addr_str
        .parse()
        .with_context(|| format!("Invalid investor wallet address: {investor_addr_str}"))?;

    let sender = resolve_session_wallet_address(db, &session.wallet_id, &session.chain).await?;

    let settings = liquifier_config::Settings::global();
    let chain_cfg = liquifier_config::get_chain(&settings.chains, &session.chain)
        .with_context(|| format!("Missing chain config for {}", session.chain))?;
    let chain_id = chain_cfg.chain_id;

    let is_native_target = is_native_placeholder_address(&session.target_token);

    if is_native_target {
        // Query native balance via RPC
        let rpc_url = chain_rpc_url(&session.chain)?;
        let rpc_url: alloy::transports::http::reqwest::Url = rpc_url.parse()?;
        let provider = ProviderBuilder::new().connect_http(rpc_url);
        let balance = provider
            .get_balance(sender)
            .await
            .context("Failed to fetch native balance")?;

        if balance.is_zero() {
            info!(session_id = %session.session_id, "No native balance to sweep");
            return Ok(());
        }

        // Reserve gas for the transfer itself (~21000 gas)
        let gas_price: u128 = provider
            .get_gas_price()
            .await
            .context("Failed to fetch gas price for sweep")?;
        let gas_cost = U256::from(21_000u64) * U256::from(gas_price.saturating_mul(2));

        let send_amount = balance.saturating_sub(gas_cost);
        if send_amount.is_zero() {
            warn!(
                session_id = %session.session_id,
                balance = %balance,
                gas_cost = %gas_cost,
                "Native balance too low to cover gas for sweep"
            );
            return Ok(());
        }

        let tx_hash = build_sign_and_submit_tx(
            kms_client,
            &session.chain,
            chain_id,
            &session.wallet_id,
            sender,
            investor,
            Vec::new(), // no calldata for native transfer
            send_amount,
        )
        .await?;

        info!(
            session_id = %session.session_id,
            tx_hash = %tx_hash,
            amount = %send_amount,
            "Native funds swept to investor wallet"
        );
    } else {
        // ERC-20 target token — query balance then transfer
        let rpc_url = chain_rpc_url(&session.chain)?;
        let rpc_url: alloy::transports::http::reqwest::Url = rpc_url.parse()?;
        let provider = ProviderBuilder::new().connect_http(rpc_url);

        let token_address: Address = session
            .target_token
            .parse()
            .with_context(|| format!("Invalid target token address: {}", session.target_token))?;

        let erc20 = IERC20Approve::new(token_address, &provider);
        let balance = erc20
            .balanceOf(sender)
            .call()
            .await
            .context("Failed to fetch target token balance")?;

        if balance.is_zero() {
            info!(session_id = %session.session_id, "No target token balance to sweep");
            return Ok(());
        }

        // Build ERC-20 transfer calldata
        let transfer_call = IERC20Approve::transferCall {
            to: investor,
            amount: balance,
        };
        let calldata = transfer_call.abi_encode();

        let tx_hash = build_sign_and_submit_tx(
            kms_client,
            &session.chain,
            chain_id,
            &session.wallet_id,
            sender,
            token_address,
            calldata,
            U256::ZERO,
        )
        .await?;

        info!(
            session_id = %session.session_id,
            tx_hash = %tx_hash,
            token = %session.target_token_symbol,
            amount = %balance,
            "ERC-20 funds swept to investor wallet"
        );
    }

    Ok(())
}

/// After session completion, check if the sell token still has balance
/// (i.e., the required USDT target was met and excess token remains).
/// If so, auto-create a refund request so the user can reclaim it.
async fn check_and_create_excess_refund(
    _kms_client: &WalletKmsClient<Channel>,
    db: &PgPool,
    session: &SessionInfo,
) {
    let sender = match resolve_session_wallet_address(db, &session.wallet_id, &session.chain).await
    {
        Ok(addr) => addr,
        Err(_) => return,
    };

    let rpc_url = match chain_rpc_url(&session.chain) {
        Ok(url) => url,
        Err(_) => return,
    };

    let parsed_url: alloy::transports::http::reqwest::Url = match rpc_url.parse() {
        Ok(u) => u,
        Err(_) => return,
    };
    let provider = ProviderBuilder::new().connect_http(parsed_url);

    let sell_token_addr: Address = match session.sell_token.parse() {
        Ok(a) => a,
        Err(_) => return,
    };

    let erc20 = IERC20Approve::new(sell_token_addr, &provider);
    let remaining_balance = match erc20.balanceOf(sender).call().await {
        Ok(b) => b,
        Err(_) => return,
    };

    if remaining_balance.is_zero() {
        return;
    }

    // Excess sell token exists — look up wallet + user and create a pending refund request
    let wallet_row = sqlx::query(
        "SELECT w.id, w.user_id FROM wallets w WHERE w.id = (SELECT wallet_id FROM sessions WHERE id = $1::uuid)"
    )
    .bind(&session.session_id)
    .fetch_optional(db)
    .await;

    if let Ok(Some(row)) = wallet_row {
        let wallet_id: uuid::Uuid = row.get("id");
        let user_id: uuid::Uuid = row.get("user_id");
        let refund_id = uuid::Uuid::new_v4();

        let _ = sqlx::query(
            "INSERT INTO refund_requests (id, user_id, wallet_id, amount, token_address, token_symbol, status)
             VALUES ($1, $2, $3, $4, $5, $6, 'pending')
             ON CONFLICT DO NOTHING",
        )
        .bind(refund_id)
        .bind(user_id)
        .bind(wallet_id)
        .bind(remaining_balance.to_string())
        .bind(&session.sell_token)
        .bind(&session.sell_token_symbol)
        .execute(db)
        .await;

        info!(
            session_id = %session.session_id,
            refund_id = %refund_id,
            amount = %remaining_balance,
            token = %session.sell_token_symbol,
            "Auto-created refund request for excess sell token"
        );
    }
}

// ─────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────

/// Get expected output amount from on-chain router quote.
/// For V2, calls `getAmountsOut` on the router.
/// For V3, we fall back to using the sell amount as a rough estimate (TODO: add V3 quoter).
async fn get_on_chain_quote(
    chain: &str,
    router_address: Address,
    amount: U256,
    route: &PoolSwapRoute,
    pool_type: PoolType,
) -> Result<U256> {
    let rpc_url = chain_rpc_url(chain)?;
    let rpc_url: alloy::transports::http::reqwest::Url = rpc_url
        .parse()
        .with_context(|| format!("Invalid RPC URL for chain {chain}"))?;
    let provider = ProviderBuilder::new().connect_http(rpc_url);

    let (path, _, _) = resolve_route_tokens_for_native(route, chain)?;

    match pool_type {
        PoolType::V2 => {
            let router = IUniswapV2Router::new(router_address, provider);
            let amounts = router
                .getAmountsOut(amount, path.clone())
                .call()
                .await
                .context("getAmountsOut call failed")?;
            // Last element is the final output amount
            amounts
                .last()
                .copied()
                .filter(|a| !a.is_zero())
                .context("getAmountsOut returned zero or empty")
        }
        PoolType::V3 => {
            // V3 quoter not implemented yet — return zero to signal caller should
            // fall back to a conservative estimate.
            Ok(U256::ZERO)
        }
    }
}

/// Build swap calldata for a router swap target with slippage protection.
fn build_swap_calldata(
    _router_address: Address,
    amount: U256,
    expected_output: U256,
    route: &PoolSwapRoute,
    pool_type: PoolType,
    default_fee_tier: u32,
    recipient: Address,
    chain: &str,
    slippage_bps: u32,
) -> Result<Vec<u8>> {
    let (path, native_input, native_output) = resolve_route_tokens_for_native(route, chain)?;
    let deadline = U256::from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_add(300),
    );

    // Compute amountOutMin by applying slippage to the on-chain quoted output.
    // If no quote is available (expected_output == 0), fall back to a 1:1
    // approximation using the sell amount (conservative for non-1:1 pools).
    let base_amount = if expected_output.is_zero() {
        amount
    } else {
        expected_output
    };
    let amount_out_min = if slippage_bps >= 10_000 {
        anyhow::bail!("Slippage bps must be less than 10000 (100%)");
    } else {
        // amount_out_min = base_amount * (10000 - slippage_bps) / 10000
        (base_amount * U256::from(10_000u64 - slippage_bps as u64)) / U256::from(10_000u64)
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
    received_amount: Option<String>,
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
    target_token: &str,
    recipient: Address,
) -> Result<TradeFinalization> {
    let Some(tx_hash) = sell_tx_hash.map(str::trim).filter(|h| !h.is_empty()) else {
        return Ok(TradeFinalization {
            status: "submitted".to_string(),
            failure_reason: None,
            received_amount: None,
        });
    };

    match fetch_receipt_with_received(chain, tx_hash, target_token, recipient).await {
        Ok(Some((true, received))) => {
            mark_trade_confirmed(db, trade_id).await?;
            // Store actual received amount in final_received / final_token
            if let Some(ref amount) = received {
                update_trade_received(db, trade_id, amount, target_token).await?;
            }
            Ok(TradeFinalization {
                status: "confirmed".to_string(),
                failure_reason: None,
                received_amount: received,
            })
        }
        Ok(Some((false, _))) => {
            let reason = "transaction_reverted".to_string();
            mark_trade_failed(db, trade_id, &reason).await?;
            Ok(TradeFinalization {
                status: "failed".to_string(),
                failure_reason: Some(reason),
                received_amount: None,
            })
        }
        Ok(None) => Ok(TradeFinalization {
            status: "submitted".to_string(),
            failure_reason: None,
            received_amount: None,
        }),
        Err(e) => {
            let reason = format!("receipt_lookup_error: {e}");
            mark_trade_failed(db, trade_id, &reason).await?;
            Ok(TradeFinalization {
                status: "failed".to_string(),
                failure_reason: Some(reason),
                received_amount: None,
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

/// Fetch receipt, check success, and extract the actual received amount
/// by parsing ERC-20 Transfer event logs where `to` = recipient.
async fn fetch_receipt_with_received(
    chain: &str,
    tx_hash: &str,
    target_token: &str,
    recipient: Address,
) -> Result<Option<(bool, Option<String>)>> {
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

    let receipt_json = serde_json::to_value(&receipt).context("Failed to serialize receipt")?;
    let Some(success) = parse_receipt_success_from_json(&receipt_json) else {
        return Ok(None);
    };

    if !success {
        return Ok(Some((false, None)));
    }

    // Parse ERC-20 Transfer(from, to, amount) events from receipt logs.
    // Transfer topic0 = keccak256("Transfer(address,address,uint256)")
    let transfer_topic: B256 = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
        .parse()
        .unwrap();

    let is_native_target = is_native_placeholder_address(target_token);

    let received = if is_native_target {
        // For native-out swaps, look for WETH/WBNB Withdrawal event to the router,
        // or simply fall back to None (will use trigger_buy_amount fallback in queries).
        // A more robust approach: check the wrapped native token's Withdrawal event.
        let wrapped_native = configured_wrapped_native_token_for_chain(chain)
            .unwrap_or_default()
            .to_ascii_lowercase();
        // Withdrawal(address indexed src, uint256 wad)
        let withdrawal_topic: B256 =
            "0x7fcf532c15f0a6db0bd6d0e038bea71d30d808c7d98cb3bf7268a95bf5081b65"
                .parse()
                .unwrap();

        extract_received_from_logs(
            &receipt_json,
            &wrapped_native,
            &withdrawal_topic,
            None, // no `to` filter for Withdrawal events
            true, // data-only amount (no indexed to)
        )
    } else {
        let target_lower = target_token.to_ascii_lowercase();
        extract_received_from_logs(
            &receipt_json,
            &target_lower,
            &transfer_topic,
            Some(recipient),
            false,
        )
    };

    Ok(Some((true, received.map(|a| a.to_string()))))
}

/// Parse receipt logs to extract received token amount.
fn extract_received_from_logs(
    receipt: &serde_json::Value,
    token_address: &str,
    event_topic: &B256,
    recipient_filter: Option<Address>,
    amount_in_data_only: bool,
) -> Option<U256> {
    let logs = receipt.get("logs")?.as_array()?;
    let mut total = U256::ZERO;

    for log in logs {
        // Check contract address matches token
        let log_address = log
            .get("address")
            .and_then(|a| a.as_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if log_address != token_address {
            continue;
        }

        // Check topic0 matches the event signature
        let topics = log.get("topics").and_then(|t| t.as_array())?;
        let topic0_str = topics.first().and_then(|t| t.as_str()).unwrap_or_default();
        let topic0: B256 = match topic0_str.parse() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if topic0 != *event_topic {
            continue;
        }

        if amount_in_data_only {
            // Withdrawal(address indexed src, uint256 wad) — amount is in data
            let data_hex = log.get("data").and_then(|d| d.as_str()).unwrap_or_default();
            if let Some(amount) = parse_u256_from_hex(data_hex) {
                total += amount;
            }
        } else {
            // Transfer(address indexed from, address indexed to, uint256 amount)
            // topics[1] = from, topics[2] = to, data = amount
            if let Some(filter_addr) = recipient_filter {
                let to_topic = topics.get(2).and_then(|t| t.as_str()).unwrap_or_default();
                let to_addr = parse_address_from_topic(to_topic);
                if to_addr != Some(filter_addr) {
                    continue;
                }
            }

            let data_hex = log.get("data").and_then(|d| d.as_str()).unwrap_or_default();
            if let Some(amount) = parse_u256_from_hex(data_hex) {
                total += amount;
            }
        }
    }

    if total.is_zero() {
        None
    } else {
        Some(total)
    }
}

fn parse_u256_from_hex(hex_str: &str) -> Option<U256> {
    let trimmed = hex_str
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    if trimmed.is_empty() {
        return None;
    }
    U256::from_str_radix(trimmed, 16).ok()
}

fn parse_address_from_topic(topic: &str) -> Option<Address> {
    // Topics are 32-byte (64 hex) zero-padded; address is the last 20 bytes
    let trimmed = topic
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    if trimmed.len() < 40 {
        return None;
    }
    let addr_hex = &trimmed[trimmed.len() - 40..];
    format!("0x{addr_hex}").parse().ok()
}

async fn update_trade_received(
    db: &PgPool,
    trade_id: &str,
    amount: &str,
    token: &str,
) -> Result<()> {
    let trade_id: uuid::Uuid = trade_id
        .parse()
        .context("Invalid trade_id for received update")?;
    let amount_decimal = parse_numeric_decimal(amount, "final_received")?;
    sqlx::query(
        r#"
        UPDATE trades
        SET final_received = $1,
            final_token = $2
        WHERE id = $3
        "#,
    )
    .bind(amount_decimal)
    .bind(token)
    .bind(trade_id)
    .execute(db)
    .await
    .context("Failed to update trade final_received")?;
    Ok(())
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

    // Log a full curl command for debug_traceCall so the tx can be replayed offline
    {
        let data_hex = hex::encode(&calldata);
        let value_hex = format!("{:#x}", value);
        let rpc = chain_rpc_url(chain).unwrap_or_default();
        let curl = format!(
            r#"curl -s -X POST {rpc} -H 'Content-Type: application/json' -d '{{"jsonrpc":"2.0","id":1,"method":"debug_traceCall","params":[{{"from":"{sender:?}","to":"{to:?}","value":"{value_hex}","data":"0x{data_hex}","gas":"0x1312D00"}},"latest",{{"tracer":"callTracer"}}]}}'"#,
        );
        info!(
            %sender,
            %to,
            %value,
            nonce,
            chain,
            data_len = calldata.len(),
            "\n=== DEBUG TRACE CURL ===\n{curl}\n========================"
        );
    }

    let gas_estimate = provider
        .estimate_gas(tx_for_estimate)
        .await
        .context("Gas estimation failed")?;

    // Fetch current gas price (legacy for BSC-like chains, EIP-1559 otherwise)
    let gas_price: u128 = provider
        .get_gas_price()
        .await
        .context("Failed to fetch gas price")?;

    let gas_limit = gas_estimate.saturating_mul(120) / 100; // 20% buffer

    info!(
        %sender,
        %to,
        nonce,
        chain,
        gas_estimate,
        gas_limit,
        gas_price,
        tx_cost_wei = gas_limit as u128 * gas_price,
        "Gas parameters"
    );

    // Build the final unsigned transaction using legacy gas pricing
    // (BSC and many L2s don't support EIP-1559)
    let unsigned_tx = TransactionRequest::default()
        .from(sender)
        .to(to)
        .value(value)
        .input(alloy::rpc::types::TransactionInput::new(Bytes::from(
            calldata,
        )))
        .nonce(nonce)
        .with_chain_id(chain_id)
        .gas_limit(gas_limit)
        .gas_price(gas_price);

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
    let pending = match provider.send_raw_transaction(&signed_tx_bytes).await {
        Ok(p) => p,
        Err(e) => {
            error!(
                %sender,
                %to,
                nonce,
                chain,
                rpc_error = %e,
                "send_raw_transaction failed"
            );
            anyhow::bail!("Failed to submit transaction to RPC: {e}");
        }
    };

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

fn pool_type_to_oracle_version(pool_type: PoolType) -> u8 {
    match pool_type {
        PoolType::V2 => 2,
        PoolType::V3 => 3,
    }
}

/// Convert oracle outputs into a floating-point USD amount.
///
/// The oracle returns a per-token USD price scaled by 1e36.
fn usd_value_from_oracle_price_36(
    amount_raw: U256,
    token_decimals: u32,
    usd_price_36: U256,
) -> Option<f64> {
    if amount_raw.is_zero() || usd_price_36.is_zero() {
        return Some(0.0);
    }

    let amount_raw_f64 = amount_raw.to_string().parse::<f64>().ok()?;
    let usd_price_36_f64 = usd_price_36.to_string().parse::<f64>().ok()?;

    let amount_tokens = amount_raw_f64 / 10_f64.powi(token_decimals as i32);
    let usd_per_token = usd_price_36_f64 / 1e36_f64;
    let usd_value = amount_tokens * usd_per_token;
    if usd_value.is_finite() {
        Some(usd_value)
    } else {
        None
    }
}

async fn fetch_buy_amount_usd_from_oracle(
    chain: &str,
    oracle_address: Address,
    token_address: &str,
    pair_address: &str,
    version: u8,
    amount_raw: U256,
    token_decimals: u32,
) -> Result<Option<f64>> {
    if amount_raw.is_zero() {
        return Ok(Some(0.0));
    }

    let rpc_url = chain_rpc_url(chain)?;
    let rpc_url: alloy::transports::http::reqwest::Url = rpc_url
        .parse()
        .with_context(|| format!("Invalid RPC URL for chain {chain}"))?;
    let provider = ProviderBuilder::new().connect_http(rpc_url);

    let token: Address = token_address
        .parse()
        .with_context(|| format!("Invalid trigger token address: {token_address}"))?;
    let pair: Address = pair_address
        .parse()
        .with_context(|| format!("Invalid pair address: {pair_address}"))?;

    let oracle = IPriceGetterV3::new(oracle_address, &provider);
    let quote = oracle
        .getUsdPrice(token, pair, version)
        .call()
        .await
        .with_context(|| {
            format!(
                "Failed getUsdPrice(token={token_address}, pair={pair_address}, version={version})"
            )
        })?;

    Ok(usd_value_from_oracle_price_36(
        amount_raw,
        token_decimals,
        quote,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::Address;

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

    // ── oracle trigger valuation helpers ────────────────────
    #[test]
    fn test_pool_type_to_oracle_version() {
        assert_eq!(pool_type_to_oracle_version(PoolType::V2), 2);
        assert_eq!(pool_type_to_oracle_version(PoolType::V3), 3);
    }

    #[test]
    fn test_oracle_usd_value_zero_amount() {
        let price_36 = U256::from(10u64).pow(U256::from(36u64));
        let result = usd_value_from_oracle_price_36(U256::ZERO, 18, price_36);
        assert_eq!(result, Some(0.0));
    }

    #[test]
    fn test_oracle_usd_value_18_decimals() {
        // 1 token amount at 18 decimals
        let amount = U256::from(10u64).pow(U256::from(18u64));
        // $2000 scaled by 1e36
        let price_36 = U256::from(2000u64) * U256::from(10u64).pow(U256::from(36u64));

        let result = usd_value_from_oracle_price_36(amount, 18, price_36).unwrap();
        assert!((result - 2000.0).abs() < 0.01);
    }

    #[test]
    fn test_oracle_usd_value_6_decimals() {
        // 1.0 token amount at 6 decimals
        let amount = U256::from(1_000_000u64);
        // $1 scaled by 1e36
        let price_36 = U256::from(10u64).pow(U256::from(36u64));

        let result = usd_value_from_oracle_price_36(amount, 6, price_36).unwrap();
        assert!((result - 1.0).abs() < 0.001);
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
