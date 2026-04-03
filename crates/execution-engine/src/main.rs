use alloy::primitives::U256;
use anyhow::{Context, Result};
use common::{
    proto::{
        session_service_client::SessionServiceClient, wallet_kms_client::WalletKmsClient,
        ActiveSessionsQuery, SessionInfo, SignTransactionRequest,
    },
    types::{DexSwapEvent, SUBJECT_DEX_SWAPS, SUBJECT_TRADES_COMPLETED},
};
use futures::StreamExt;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::sync::Arc;
use tonic::transport::Channel;
use tracing::{error, info, warn};

mod impact;
mod router;

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

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize global config (loads config/default.yml + env overrides)
    liquifier_config::Settings::init().expect("Failed to load config");
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .json()
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

        // 4. Calculate price impact using x * y = k
        let price_impact_bps =
            impact::calculate_price_impact_v2(&event.pool_address, sell_amount).await;

        let max_impact_bps = (session.max_price_impact * 100.0) as u32;
        if price_impact_bps > max_impact_bps {
            warn!(
                session_id = %session.session_id,
                impact_bps = price_impact_bps,
                max_bps = max_impact_bps,
                "Skipping trade: price impact exceeds threshold"
            );
            continue;
        }

        info!(
            session_id = %session.session_id,
            sell_amount = %sell_amount,
            impact_bps = price_impact_bps,
            pool = %event.pool_address,
            "Executing POV trade"
        );

        // 5. Build and sign the transaction via KMS
        // In production, this builds a proper DEX router call or direct pool swap
        let tx_bytes = build_swap_calldata(&event.pool_address, sell_amount);

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

                // 7. Record trade in DB
                record_trade(&state.db, session, &event, sell_amount, price_impact_bps).await?;

                // 8. If target token differs from pool's paired token,
                // spawn async routing task
                if needs_routing(session, &event) {
                    let state_clone = state.db.clone();
                    let session_id = session.session_id.clone();
                    tokio::spawn(async move {
                        if let Err(e) =
                            router::route_to_target_token(&state_clone, &session_id).await
                        {
                            error!(
                                session_id = %session_id,
                                error = %e,
                                "Async routing failed"
                            );
                        }
                    });
                }

                // 9. Publish trade completion event to NATS
                let trade_event = serde_json::json!({
                    "session_id": session.session_id,
                    "chain": session.chain,
                    "sell_amount": sell_amount.to_string(),
                    "price_impact_bps": price_impact_bps,
                    "pool": event.pool_address,
                    "trigger_tx": event.tx_hash,
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

/// Build swap calldata for a direct pool swap.
/// In production, this would encode the proper function selector + args.
fn build_swap_calldata(pool_address: &str, amount: U256) -> Vec<u8> {
    // Placeholder: production builds proper ABI-encoded calldata
    let mut data = Vec::with_capacity(68);
    // swap(uint256) selector placeholder
    data.extend_from_slice(&[0x01, 0x23, 0x45, 0x67]);
    data.extend_from_slice(&amount.to_be_bytes::<32>());
    data
}

/// Check if the pool's paired token differs from the session's target token
fn needs_routing(session: &SessionInfo, event: &DexSwapEvent) -> bool {
    // If the token we receive from the pool swap is not the final target token,
    // we need to route it asynchronously
    !event.token_in.eq_ignore_ascii_case(&session.target_token)
}

fn chain_name_to_id(name: &str) -> u64 {
    liquifier_config::chain_name_to_id(&liquifier_config::Settings::global().chains, name)
}

async fn record_trade(
    db: &PgPool,
    session: &SessionInfo,
    event: &DexSwapEvent,
    sell_amount: U256,
    price_impact_bps: u32,
) -> Result<()> {
    let session_id: uuid::Uuid = session.session_id.parse()?;
    sqlx::query(
        r#"
        INSERT INTO trades (
            session_id, chain, status, trigger_tx_hash, trigger_pool,
            trigger_buy_amount, sell_amount, sell_pool, price_impact_bps, executed_at
        )
        VALUES ($1, $2::chain_id, 'confirmed', $3, $4, $5, $6, $7, $8, NOW())
        "#,
    )
    .bind(session_id)
    .bind(&session.chain)
    .bind(&event.tx_hash)
    .bind(&event.pool_address)
    .bind(event.amount_out.parse::<i64>().unwrap_or(0)) // Simplified; production uses NUMERIC
    .bind(sell_amount.to_string().parse::<i64>().unwrap_or(0))
    .bind(&event.pool_address)
    .bind(price_impact_bps as i32)
    .execute(db)
    .await
    .context("Failed to record trade")?;

    // Update session amount_sold
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

    Ok(())
}
