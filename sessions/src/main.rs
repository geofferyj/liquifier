//! Session API & Routing Service
//!
//! Exposes a gRPC interface for reading/updating session configurations.
//! Also queries DEX routers to compute the top-5 liquidity paths for a
//! given token pair on the requested chain.

use alloy::{
    primitives::{Address, Bytes, U256},
    providers::{Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
};
use anyhow::Context;
use sqlx::PgPool;
use std::net::SocketAddr;
use tonic::{transport::Server, Request, Response, Status};
use tracing::info;
use uuid::Uuid;
use hex;

pub mod proto {
    tonic::include_proto!("liquifier");
}

use proto::{
    session_service_server::{SessionService, SessionServiceServer},
    *,
};

// ─── Service ───────────────────────────────────────────────────────────────

struct SessionsService {
    db:      PgPool,
    rpc_url: String,
}

#[tonic::async_trait]
impl SessionService for SessionsService {
    async fn get_session_config(
        &self,
        req: Request<GetSessionConfigRequest>,
    ) -> Result<Response<GetSessionConfigResponse>, Status> {
        let session_id: Uuid = req
            .into_inner()
            .session_id
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid session_id"))?;

        let row = sqlx::query!(
            r#"
            SELECT s.id, s.wallet_id, w.address, s.chain_id,
                   s.token_address, s.target_token_address,
                   s.total_amount, s.amount_sold,
                   s.strategy as "strategy: String",
                   s.pov_percentage, s.max_price_impact_bps,
                   s.min_buy_trigger_usd, s.swap_paths,
                   s.status as "status: String"
            FROM sessions s
            JOIN wallets w ON w.id = s.wallet_id
            WHERE s.id = $1
            "#,
            session_id
        )
        .fetch_optional(&self.db)
        .await
        .map_err(|e| Status::internal(e.to_string()))?
        .ok_or_else(|| Status::not_found("Session not found"))?;

        let paths: Vec<SwapPath> = serde_json::from_value(row.swap_paths)
            .unwrap_or_default();

        let config = SessionConfig {
            session_id:           row.id.to_string(),
            wallet_id:            row.wallet_id.to_string(),
            wallet_address:       row.address,
            chain_id:             row.chain_id as u64,
            token_address:        row.token_address,
            target_token_address: row.target_token_address,
            total_amount:         row.total_amount.to_string(),
            amount_sold:          row.amount_sold.to_string(),
            strategy:             row.strategy,
            pov_percentage:       row
                .pov_percentage
                .and_then(|v| v.to_string().parse().ok())
                .unwrap_or(0.0),
            max_price_impact_bps: row.max_price_impact_bps.unwrap_or(50) as u32,
            min_buy_trigger_usd:  row.min_buy_trigger_usd.to_string(),
            swap_paths:           paths,
            status:               row.status,
        };

        Ok(Response::new(GetSessionConfigResponse { config: Some(config) }))
    }

    async fn list_active_sessions(
        &self,
        req: Request<ListActiveSessionsRequest>,
    ) -> Result<Response<ListActiveSessionsResponse>, Status> {
        let chain_id = req.into_inner().chain_id;

        let rows = sqlx::query!(
            r#"
            SELECT s.id, s.wallet_id, w.address, s.chain_id,
                   s.token_address, s.target_token_address,
                   s.total_amount, s.amount_sold,
                   s.strategy as "strategy: String",
                   s.pov_percentage, s.max_price_impact_bps,
                   s.min_buy_trigger_usd, s.swap_paths,
                   s.status as "status: String"
            FROM sessions s
            JOIN wallets w ON w.id = s.wallet_id
            WHERE s.status = 'active' AND s.chain_id = $1
            "#,
            chain_id as i64
        )
        .fetch_all(&self.db)
        .await
        .map_err(|e| Status::internal(e.to_string()))?;

        let sessions = rows
            .into_iter()
            .map(|row| {
                let paths: Vec<SwapPath> =
                    serde_json::from_value(row.swap_paths).unwrap_or_default();

                SessionConfig {
                    session_id:           row.id.to_string(),
                    wallet_id:            row.wallet_id.to_string(),
                    wallet_address:       row.address,
                    chain_id:             row.chain_id as u64,
                    token_address:        row.token_address,
                    target_token_address: row.target_token_address,
                    total_amount:         row.total_amount.to_string(),
                    amount_sold:          row.amount_sold.to_string(),
                    strategy:             row.strategy,
                    pov_percentage:       row
                        .pov_percentage
                        .and_then(|v| v.to_string().parse().ok())
                        .unwrap_or(0.0),
                    max_price_impact_bps: row.max_price_impact_bps.unwrap_or(50) as u32,
                    min_buy_trigger_usd:  row.min_buy_trigger_usd.to_string(),
                    swap_paths:           paths,
                    status:               row.status,
                }
            })
            .collect();

        Ok(Response::new(ListActiveSessionsResponse { sessions }))
    }

    async fn update_session_state(
        &self,
        req: Request<UpdateSessionStateRequest>,
    ) -> Result<Response<UpdateSessionStateResponse>, Status> {
        let body = req.into_inner();
        let session_id: Uuid = body
            .session_id
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid session_id"))?;

        let amount_delta: bigdecimal::BigDecimal = body.amount_delta
            .parse()
            .unwrap_or_else(|_| bigdecimal::BigDecimal::from(0));

        sqlx::query!(
            r#"
            UPDATE sessions
            SET status       = $1::session_status,
                amount_sold  = amount_sold + $2
            WHERE id = $3
            "#,
            body.new_status as _,
            amount_delta,
            session_id,
        )
        .execute(&self.db)
        .await
        .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(UpdateSessionStateResponse {
            success: true,
            message: "Session updated".into(),
        }))
    }
}

// ─── Top-5 path discovery ──────────────────────────────────────────────────
//
// Queries the Uniswap V2 Router02 getAmountsOut for multiple paths and
// ranks them by output amount.  This is called from the gateway's
// session creation handler (via gRPC), and the result is stored in
// sessions.swap_paths as JSONB.
//
// In a real deployment you'd also query V3 pools and aggregate liquidity
// across chains; this function illustrates the concept.

pub async fn get_top_paths(
    rpc_url: &str,
    token_in: Address,
    token_out: Address,
    amount_in: U256,
) -> anyhow::Result<Vec<SwapPath>> {
    let provider = ProviderBuilder::new()
        .on_http(rpc_url.parse()?)
        .root()
        .clone();

    // Common intermediate tokens on mainnet
    let intermediates: Vec<Address> = vec![
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".parse()?, // WETH
        "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".parse()?, // USDC
        "0xdAC17F958D2ee523a2206206994597C13D831ec7".parse()?, // USDT
        "0x6B175474E89094C44Da98b954EedeAC495271d0F".parse()?, // DAI
    ];

    // Uniswap V2 Router02 on mainnet
    let router: Address = "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D".parse()?;

    let mut paths: Vec<SwapPath> = Vec::new();

    // Direct path
    let direct_out = query_amounts_out(&provider, router, amount_in, &[token_in, token_out]).await;

    if let Ok(out) = direct_out {
        paths.push(SwapPath {
            tokens:    vec![format!("{token_in:#x}"), format!("{token_out:#x}")],
            pools:     vec![],   // populated in production via getPair
            fee_bps:   "30".into(),
            liquidity: out.to_string(),
        });
    }

    // Single-hop paths via each intermediate
    for mid in &intermediates {
        if *mid == token_in || *mid == token_out {
            continue;
        }
        let result = query_amounts_out(
            &provider,
            router,
            amount_in,
            &[token_in, *mid, token_out],
        ).await;

        if let Ok(out) = result {
            paths.push(SwapPath {
                tokens: vec![
                    format!("{token_in:#x}"),
                    format!("{mid:#x}"),
                    format!("{token_out:#x}"),
                ],
                pools:    vec![],
                fee_bps:  "60".into(),
                liquidity: out.to_string(),
            });
        }
    }

    // Sort descending by liquidity (approx. output amount)
    paths.sort_by(|a, b| {
        let la: u128 = a.liquidity.parse().unwrap_or(0);
        let lb: u128 = b.liquidity.parse().unwrap_or(0);
        lb.cmp(&la)
    });

    Ok(paths.into_iter().take(5).collect())
}

/// Call getAmountsOut(amountIn, path[]) on the V2 router.
async fn query_amounts_out(
    provider: &impl Provider,
    router: Address,
    amount_in: U256,
    path: &[Address],
) -> anyhow::Result<U256> {
    // selector: getAmountsOut(uint256,address[]) = 0xd06ca61f
    let mut calldata = hex::decode("d06ca61f").expect("valid getAmountsOut selector");

    // amountIn (slot 0)
    calldata.extend_from_slice(&amount_in.to_be_bytes::<32>());
    // offset for path array (slot 1 = 0x40)
    let mut offset = [0u8; 32];
    offset[31] = 0x40;
    calldata.extend_from_slice(&offset);
    // path length
    let mut len_word = [0u8; 32];
    len_word[31] = path.len() as u8;
    calldata.extend_from_slice(&len_word);
    // path elements
    for addr in path {
        let mut word = [0u8; 32];
        word[12..].copy_from_slice(addr.as_slice());
        calldata.extend_from_slice(&word);
    }

    let result = provider
        .call(
            &TransactionRequest::default()
                .to(router)
                .input(Bytes::from(calldata).into()),
        )
        .await
        .context("getAmountsOut call")?;

    // Result is a dynamic array; last element is amounts[amounts.length-1]
    let n_elements = result.len() / 32;
    if n_elements < 2 {
        return Err(anyhow::anyhow!("Unexpected getAmountsOut result length"));
    }

    let last = &result[(n_elements - 1) * 32..n_elements * 32];
    Ok(U256::from_be_slice(last))
}

// ─── Main ──────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL")?;
    let rpc_url      = std::env::var("EVM_RPC_URL").context("EVM_RPC_URL")?;
    let bind_addr: SocketAddr = std::env::var("BIND_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:50052".into())
        .parse()
        .context("Invalid BIND_ADDR")?;

    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await
        .context("DB connect")?;

    let service = SessionsService { db, rpc_url };

    info!("Sessions gRPC server listening on {bind_addr}");
    Server::builder()
        .add_service(SessionServiceServer::new(service))
        .serve(bind_addr)
        .await
        .context("gRPC server error")?;

    Ok(())
}


