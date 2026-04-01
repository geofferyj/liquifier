//! Execution Engine — horizontally-scalable worker
//!
//! Subscribes to `evm.dex.swaps` Kafka topic, filters events against
//! active Liquifier sessions, calculates price impact (x·y=k), and
//! submits sell transactions via Flashbots relay.

use alloy::{
    primitives::{Address, Bytes, U256},
    providers::{Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
};
use anyhow::Context;
use rdkafka::{
    config::ClientConfig,
    consumer::{Consumer, StreamConsumer},
    message::Message,
    producer::{FutureProducer, FutureRecord},
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::RwLock;
use tonic::transport::Channel;
use tracing::{error, info, warn};
use uuid::Uuid;
use chrono;

pub mod proto {
    tonic::include_proto!("liquifier");
}

use proto::{
    wallet_kms_client::WalletKmsClient, SignTransactionRequest,
    session_service_client::SessionServiceClient, ListActiveSessionsRequest,
};

// ─── Types from indexer ────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Clone)]
pub struct DexSwapEvent {
    pub block_number:  u64,
    pub tx_hash:       String,
    pub log_index:     u64,
    pub pool_address:  String,
    pub chain_id:      u64,
    pub version:       u8,
    pub token_in:      String,
    pub token_out:     String,
    pub amount_in:     String,
    pub amount_out:    String,
    pub sender:        String,
    pub recipient:     String,
}

#[derive(Debug, Serialize)]
pub struct TradeCompleted {
    pub session_id:      String,
    pub tx_hash:         String,
    pub pool_address:    String,
    pub amount_in:       String,
    pub amount_out:      String,
    pub price_impact_bps: u32,
    pub status:          String,
}

// ─── Session cache ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct CachedSession {
    session_id:            String,
    wallet_id:             String,
    wallet_address:        String,
    chain_id:              u64,
    token_address:         String,
    target_token_address:  String,
    remaining:             U256,
    strategy:              String,
    pov_percentage:        f64,
    max_price_impact_bps:  u32,
    min_buy_trigger_usd:   f64,
}

// ─── x·y = k price impact calculation ─────────────────────────────────────
//
// For a constant-product AMM pool with reserves (r_in, r_out) and a trade
// of amount `delta_in`, the price impact in basis points is:
//
//   new_r_in  = r_in  + delta_in
//   new_r_out = k / new_r_in
//   amount_out_ideal = r_out - new_r_out   (no fees, no slippage)
//   impact_bps = (1 - (amount_out_ideal / r_out)) * 10_000
//
// This is equivalent to:  impact_bps = delta_in * 10_000 / (r_in + delta_in)
//
// We use U256 integer arithmetic to avoid floating-point issues.
fn price_impact_bps(reserve_in: U256, delta_in: U256) -> u32 {
    if reserve_in.is_zero() || delta_in.is_zero() {
        return 0;
    }
    let denominator = reserve_in + delta_in;
    // impact = delta_in * 10_000 / denominator
    let impact = delta_in
        .saturating_mul(U256::from(10_000u64))
        .wrapping_div(denominator);
    impact.saturating_to::<u32>()
}

// ─── Engine state ──────────────────────────────────────────────────────────

struct Engine {
    db:            PgPool,
    kms:           WalletKmsClient<Channel>,
    sessions_svc:  SessionServiceClient<Channel>,
    producer:      FutureProducer,
    topic_out:     String,
    rpc_url:       String,
    flashbots_url: String,
    chain_id:      u64,
    session_cache: Arc<RwLock<HashMap<String, CachedSession>>>,
}

impl Engine {
    /// Refresh local session cache from the Sessions service.
    async fn refresh_cache(&self) -> anyhow::Result<()> {
        let mut svc = self.sessions_svc.clone();
        let resp = svc
            .list_active_sessions(ListActiveSessionsRequest { chain_id: self.chain_id })
            .await?
            .into_inner();

        let mut cache = self.session_cache.write().await;
        cache.clear();

        for cfg in resp.sessions {
            let remaining = cfg.total_amount.parse::<U256>().unwrap_or(U256::ZERO)
                .saturating_sub(cfg.amount_sold.parse::<U256>().unwrap_or(U256::ZERO));

            cache.insert(
                cfg.token_address.clone(),
                CachedSession {
                    session_id:           cfg.session_id,
                    wallet_id:            cfg.wallet_id,
                    wallet_address:       cfg.wallet_address,
                    chain_id:             cfg.chain_id,
                    token_address:        cfg.token_address,
                    target_token_address: cfg.target_token_address,
                    remaining,
                    strategy:             cfg.strategy,
                    pov_percentage:       cfg.pov_percentage,
                    max_price_impact_bps: cfg.max_price_impact_bps,
                    min_buy_trigger_usd:  cfg.min_buy_trigger_usd.parse().unwrap_or(100.0),
                },
            );
        }

        info!("Session cache refreshed: {} active sessions", cache.len());
        Ok(())
    }

    /// Process one DEX swap event from Kafka.
    async fn handle_event(&self, event: DexSwapEvent) -> anyhow::Result<()> {
        let cache = self.session_cache.read().await;

        // Find sessions interested in this pool's token_out (the "buy" token)
        let session = match cache.get(&event.token_out) {
            Some(s) if s.chain_id == event.chain_id => s.clone(),
            _ => return Ok(()),
        };
        drop(cache);

        // Parse buy amount
        let buy_amount = event.amount_out.parse::<U256>().unwrap_or(U256::ZERO);
        if buy_amount.is_zero() {
            return Ok(());
        }

        // TODO: convert buy_amount to USD using a price oracle; for now we
        // use a placeholder check against a scaled integer.
        // In production this would query a Chainlink feed or Uniswap TWAP.
        let buy_usd_approx = 0f64; // placeholder
        if buy_usd_approx < session.min_buy_trigger_usd {
            return Ok(());
        }

        // Determine sell amount based on strategy
        let sell_amount = match session.strategy.as_str() {
            "pov" => {
                // Sell `pov_percentage`% of the incoming buy volume
                let pov_factor = (session.pov_percentage / 100.0 * 1e18) as u128;
                let amount = buy_amount
                    .saturating_mul(U256::from(pov_factor))
                    .wrapping_div(U256::from(1_000_000_000_000_000_000u128));
                amount.min(session.remaining)
            }
            _ => return Ok(()),
        };

        if sell_amount.is_zero() {
            return Ok(());
        }

        // ── Price impact check ─────────────────────────────────────────────
        // Fetch pool reserves via getReserves() (V2) or slot0 (V3).
        // Simplified: we derive reserve_in from pool address call.
        let provider = ProviderBuilder::new()
            .on_http(self.rpc_url.parse()?)
            .root()
            .clone();

        // getReserves() selector: 0x0902f1ac
        let reserves_data = provider
            .call(
                &TransactionRequest::default()
                    .to(event.pool_address.parse::<Address>()?)
                    .input(hex::decode("0902f1ac").expect("valid getReserves selector").into()),
            )
            .await
            .context("getReserves call")?;

        if reserves_data.len() < 64 {
            return Err(anyhow::anyhow!("Unexpected getReserves response"));
        }

        let reserve0 = U256::from_be_slice(&reserves_data[..32]);
        let reserve1 = U256::from_be_slice(&reserves_data[32..64]);

        // We assume token_in corresponds to reserve0; a production
        // implementation would verify by calling token0() on the pool.
        let impact = price_impact_bps(reserve0, sell_amount);

        if impact > session.max_price_impact_bps {
            warn!(
                session_id = %session.session_id,
                impact_bps = impact,
                max_bps = session.max_price_impact_bps,
                "Price impact too high — skipping trade"
            );
            return Ok(());
        }

        // ── Build calldata for swapExactTokensForTokens (V2) ─────────────
        // selector: 0x38ed1739
        // Arguments: amountIn, amountOutMin (1 = accept any), path[], to, deadline
        let deadline = (chrono::Utc::now().timestamp() as u64) + 300;
        let token_in_addr: Address  = session.token_address.parse()?;
        let token_out_addr: Address = session.target_token_address.parse()?;
        let wallet_addr: Address    = session.wallet_address.parse()?;

        // Manual ABI encoding (production should use alloy's sol! macro with the
        // router ABI, but we inline it here to keep the service self-contained)
        let calldata = build_swap_calldata(
            sell_amount,
            U256::from(1u64), // amountOutMin = 1 (accept anything)
            &[token_in_addr, token_out_addr],
            wallet_addr,
            U256::from(deadline),
        );

        // ── Get nonce, gas prices ──────────────────────────────────────────
        let nonce = provider
            .get_transaction_count(wallet_addr)
            .await
            .context("get_transaction_count")?;

        let gas_price = provider
            .get_gas_price()
            .await
            .context("get_gas_price")?;

        // ── Sign via KMS gRPC ──────────────────────────────────────────────
        let mut kms = self.kms.clone();
        // Uniswap V2 router addresses vary by chain; placeholder here.
        let router = "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D";

        let sign_resp = kms
            .sign_transaction(SignTransactionRequest {
                wallet_id:               session.wallet_id.clone(),
                chain_id:                session.chain_id,
                to:                      router.into(),
                value:                   "0".into(),
                data:                    calldata.to_vec(),
                gas_limit:               "300000".into(),
                max_fee_per_gas:         gas_price.to_string(),
                max_priority_fee_per_gas: "1000000000".into(), // 1 gwei tip
                nonce:                   nonce.to_string(),
            })
            .await
            .context("KMS sign_transaction")?
            .into_inner();

        let tx_hash = sign_resp.tx_hash.clone();
        info!(
            session_id = %session.session_id,
            %tx_hash,
            sell_amount = %sell_amount,
            impact_bps = impact,
            "Submitting trade via Flashbots"
        );

        // ── Submit via Flashbots ───────────────────────────────────────────
        self.submit_via_flashbots(&sign_resp.raw_tx).await?;

        // ── Record trade in DB ─────────────────────────────────────────────
        let session_id_uuid: Uuid = session.session_id.parse()?;
        sqlx::query!(
            r#"
            INSERT INTO trades (
                id, session_id, trigger_tx_hash, trigger_pool_address, trigger_buy_amount,
                tx_hash, pool_address, amount_in, final_token, price_impact_bps, status
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'submitted'
            )
            "#,
            Uuid::new_v4(),
            session_id_uuid,
            event.tx_hash,
            event.pool_address,
            sell_amount.to_string().parse::<bigdecimal::BigDecimal>().ok(),
            tx_hash,
            event.pool_address,
            sell_amount.to_string().parse::<bigdecimal::BigDecimal>().ok(),
            session.target_token_address,
            impact as i32,
        )
        .execute(&self.db)
        .await?;

        // ── Publish completion event ───────────────────────────────────────
        let completion = TradeCompleted {
            session_id:       session.session_id.clone(),
            tx_hash:          sign_resp.tx_hash,
            pool_address:     event.pool_address,
            amount_in:        sell_amount.to_string(),
            amount_out:       "0".into(), // updated when confirmed
            price_impact_bps: impact,
            status:           "submitted".into(),
        };
        let payload = serde_json::to_string(&completion)?;

        self.producer
            .send(
                FutureRecord::to(&self.topic_out)
                    .key(&session.session_id)
                    .payload(&payload),
                Duration::from_secs(5),
            )
            .await
            .map_err(|(e, _)| anyhow::anyhow!("Kafka send: {e}"))?;

        // ── Async routing: if target != pool's paired token, spawn a task ──
        if session.token_address != session.target_token_address {
            let session_clone = session.clone();
            tokio::spawn(async move {
                // In production: poll for tx confirmation then execute the
                // second leg of the swap (intermediate → target token).
                // For now we log the intent.
                info!(
                    session_id = %session_clone.session_id,
                    "Async routing task spawned for multi-hop swap"
                );
            });
        }

        Ok(())
    }

    /// Broadcast a raw signed transaction to Flashbots relay via eth_sendRawTransaction.
    async fn submit_via_flashbots(&self, raw_tx: &[u8]) -> anyhow::Result<()> {
        let hex_tx = format!("0x{}", hex::encode(raw_tx));
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id":      1,
            "method":  "eth_sendRawTransaction",
            "params":  [hex_tx]
        });

        // In production use reqwest; here we use alloy's HTTP transport.
        // For brevity we just log and return Ok — wire up reqwest in production.
        info!(
            endpoint = %self.flashbots_url,
            "Would submit raw tx ({} bytes) to Flashbots",
            raw_tx.len()
        );
        let _ = body; // suppress unused warning
        Ok(())
    }
}

// ─── ABI encoding helpers ──────────────────────────────────────────────────

/// Encode swapExactTokensForTokens(amountIn, amountOutMin, path[], to, deadline)
fn build_swap_calldata(
    amount_in: U256,
    amount_out_min: U256,
    path: &[Address],
    to: Address,
    deadline: U256,
) -> Bytes {
    // selector: swapExactTokensForTokens(uint256,uint256,address[],address,uint256)
    // keccak256("swapExactTokensForTokens(uint256,uint256,address[],address,uint256)")[..4]
    let selector = hex::decode("38ed1739").expect("valid swapExactTokensForTokens selector");

    let mut buf = selector;

    // Encode fixed-size params (5 × 32 bytes head):
    // slot 0: amountIn
    buf.extend_from_slice(&u256_to_be32(amount_in));
    // slot 1: amountOutMin
    buf.extend_from_slice(&u256_to_be32(amount_out_min));
    // slot 2: offset for path array (= 5 * 32 = 0xa0)
    buf.extend_from_slice(&u256_to_be32(U256::from(0xa0u64)));
    // slot 3: to
    let mut to_word = [0u8; 32];
    to_word[12..].copy_from_slice(to.as_slice());
    buf.extend_from_slice(&to_word);
    // slot 4: deadline
    buf.extend_from_slice(&u256_to_be32(deadline));

    // path array: length + elements
    buf.extend_from_slice(&u256_to_be32(U256::from(path.len() as u64)));
    for addr in path {
        let mut word = [0u8; 32];
        word[12..].copy_from_slice(addr.as_slice());
        buf.extend_from_slice(&word);
    }

    Bytes::from(buf)
}

fn u256_to_be32(v: U256) -> [u8; 32] {
    let mut buf = [0u8; 32];
    let bytes = v.to_be_bytes::<32>();
    buf.copy_from_slice(&bytes);
    buf
}

// ─── Main ──────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let database_url   = std::env::var("DATABASE_URL").context("DATABASE_URL")?;
    let brokers        = std::env::var("KAFKA_BROKERS").context("KAFKA_BROKERS")?;
    let topic_in       = std::env::var("KAFKA_TOPIC_DEX_SWAPS").unwrap_or_else(|_| "evm.dex.swaps".into());
    let topic_out      = std::env::var("KAFKA_TOPIC_TRADES_COMPLETED").unwrap_or_else(|_| "trades.completed".into());
    let kms_addr       = std::env::var("KMS_GRPC_ADDR").context("KMS_GRPC_ADDR")?;
    let sessions_addr  = std::env::var("SESSIONS_GRPC_ADDR").unwrap_or_else(|_| "http://sessions:50052".into());
    let rpc_url        = std::env::var("EVM_RPC_URL").context("EVM_RPC_URL")?;
    let flashbots_url  = std::env::var("FLASHBOTS_RELAY_URL")
                            .unwrap_or_else(|_| "https://relay.flashbots.net".into());
    let chain_id: u64  = std::env::var("CHAIN_ID").unwrap_or_else(|_| "1".into())
                            .parse().context("CHAIN_ID")?;

    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await
        .context("DB connect")?;

    let kms = WalletKmsClient::connect(kms_addr)
        .await
        .context("KMS gRPC connect")?;

    let sessions_svc = SessionServiceClient::connect(sessions_addr)
        .await
        .context("Sessions gRPC connect")?;

    let producer: FutureProducer = ClientConfig::new()
        .set("bootstrap.servers", &brokers)
        .set("message.timeout.ms", "5000")
        .create()
        .context("Kafka producer")?;

    let consumer: StreamConsumer = ClientConfig::new()
        .set("bootstrap.servers", &brokers)
        .set("group.id",           "engine-workers")
        .set("auto.offset.reset",  "latest")
        .set("enable.auto.commit", "true")
        .create()
        .context("Kafka consumer")?;

    consumer.subscribe(&[&topic_in])?;

    let engine = Arc::new(Engine {
        db,
        kms,
        sessions_svc,
        producer,
        topic_out,
        rpc_url,
        flashbots_url,
        chain_id,
        session_cache: Arc::new(RwLock::new(HashMap::new())),
    });

    // Refresh session cache on startup and periodically
    {
        let eng = engine.clone();
        tokio::spawn(async move {
            loop {
                if let Err(e) = eng.refresh_cache().await {
                    error!("Cache refresh error: {e:#}");
                }
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        });
    }

    info!("Engine worker started — consuming topic: {topic_in}");

    // Process messages
    use rdkafka::consumer::Consumer;
    loop {
        match consumer.recv().await {
            Err(e) => error!("Kafka recv error: {e}"),
            Ok(msg) => {
                if let Some(payload) = msg.payload() {
                    match serde_json::from_slice::<DexSwapEvent>(payload) {
                        Ok(event) => {
                            let eng = engine.clone();
                            tokio::spawn(async move {
                                if let Err(e) = eng.handle_event(event).await {
                                    warn!("handle_event error: {e:#}");
                                }
                            });
                        }
                        Err(e) => warn!("Deserialise event: {e}"),
                    }
                }
            }
        }
    }
}


