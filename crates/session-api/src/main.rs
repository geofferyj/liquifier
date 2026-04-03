use alloy::providers::ProviderBuilder;
use anyhow::{Context, Result};
use common::proto::{
    session_service_server::{SessionService, SessionServiceServer},
    ActiveSessionsQuery, ActiveSessionsResponse, ComputePoolPathRequest, ComputePoolPathResponse,
    CreateSessionRequest, DiscoverPoolsRequest, DiscoverPoolsResponse, GetSessionBySlugRequest,
    GetSessionRequest, GetSwapPathsRequest, GetSwapPathsResponse, ListSessionsRequest,
    ListSessionsResponse, PoolInfo, SessionInfo, SwapPath, TogglePublicSharingRequest,
    UpdateSessionConfigRequest, UpdateSessionStatusRequest,
};
use rand::distr::Alphanumeric;
use rand::RngExt;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use std::net::SocketAddr;
use tonic::{transport::Server, Request, Response, Status};
use tracing::info;
use uuid::Uuid;

mod pools;

// ─────────────────────────────────────────────────────────────
// Service State
// ─────────────────────────────────────────────────────────────

pub struct SessionServiceImpl {
    db: PgPool,
    rpc_url: String,
}

impl SessionServiceImpl {
    pub fn new(db: PgPool, rpc_url: String) -> Self {
        Self { db, rpc_url }
    }
}

// ─────────────────────────────────────────────────────────────
// gRPC Implementation
// ─────────────────────────────────────────────────────────────

#[tonic::async_trait]
impl SessionService for SessionServiceImpl {
    async fn create_session(
        &self,
        request: Request<CreateSessionRequest>,
    ) -> Result<Response<SessionInfo>, Status> {
        let req = request.into_inner();
        let user_id: Uuid = req
            .user_id
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid user_id"))?;
        let wallet_id: Uuid = req
            .wallet_id
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid wallet_id"))?;
        let session_id = Uuid::new_v4();

        // Generate a random public slug
        let slug: String = {
            let mut rng = rand::rng();
            (0..12).map(|_| rng.sample(Alphanumeric) as char).collect()
        };

        let row = sqlx::query(
            r#"
            INSERT INTO sessions (
                id, user_id, wallet_id, chain, sell_token, sell_token_symbol,
                sell_token_decimals, target_token, target_token_symbol,
                target_token_decimals, strategy, total_amount, pov_percent,
                max_price_impact, min_buy_trigger_usd, swap_path, public_slug
            )
            VALUES (
                $1, $2, $3, $4::chain_id, $5, $6, $7, $8, $9, $10,
                $11::strategy_type, $12::numeric, $13, $14, $15, $16, $17
            )
            RETURNING id, created_at, updated_at
            "#,
        )
        .bind(session_id)
        .bind(user_id)
        .bind(wallet_id)
        .bind(&req.chain)
        .bind(&req.sell_token)
        .bind(&req.sell_token_symbol)
        .bind(req.sell_token_decimals as i32)
        .bind(&req.target_token)
        .bind(&req.target_token_symbol)
        .bind(req.target_token_decimals as i32)
        .bind(&req.strategy)
        .bind(&req.total_amount)
        .bind(req.pov_percent)
        .bind(req.max_price_impact)
        .bind(req.min_buy_trigger_usd)
        .bind(if req.swap_path_json.is_empty() {
            None
        } else {
            Some(
                serde_json::from_str::<serde_json::Value>(&req.swap_path_json)
                    .map_err(|_| Status::invalid_argument("Invalid swap_path_json"))?,
            )
        })
        .bind(&slug)
        .fetch_one(&self.db)
        .await
        .map_err(|e| Status::internal(format!("DB error: {e}")))?;

        let created_at: chrono::DateTime<chrono::Utc> = row.get("created_at");
        let updated_at: chrono::DateTime<chrono::Utc> = row.get("updated_at");

        // Persist discovered pools
        for pool in &req.pools {
            let swap_path_val: Option<serde_json::Value> = if pool.swap_path_json.is_empty() {
                None
            } else {
                serde_json::from_str(&pool.swap_path_json).ok()
            };
            sqlx::query(
                r#"
                INSERT INTO session_pools (session_id, pool_address, pool_type, dex_name, token0, token1, fee_tier, swap_path)
                VALUES ($1, $2, $3::pool_type, $4, $5, $6, $7, $8)
                ON CONFLICT (session_id, pool_address) DO NOTHING
                "#,
            )
            .bind(session_id)
            .bind(&pool.pool_address)
            .bind(&pool.pool_type)
            .bind(&pool.dex_name)
            .bind(&pool.token0)
            .bind(&pool.token1)
            .bind(pool.fee_tier as i32)
            .bind(swap_path_val)
            .execute(&self.db)
            .await
            .map_err(|e| Status::internal(format!("DB error inserting pool: {e}")))?;
        }

        info!(session_id = %session_id, pools = req.pools.len(), "Session created");

        Ok(Response::new(SessionInfo {
            session_id: session_id.to_string(),
            user_id: user_id.to_string(),
            wallet_id: wallet_id.to_string(),
            chain: req.chain,
            status: "pending".to_string(),
            sell_token: req.sell_token,
            sell_token_symbol: req.sell_token_symbol,
            sell_token_decimals: req.sell_token_decimals,
            target_token: req.target_token,
            target_token_symbol: req.target_token_symbol,
            target_token_decimals: req.target_token_decimals,
            strategy: req.strategy,
            total_amount: req.total_amount,
            amount_sold: "0".to_string(),
            pov_percent: req.pov_percent,
            max_price_impact: req.max_price_impact,
            min_buy_trigger_usd: req.min_buy_trigger_usd,
            swap_path_json: req.swap_path_json,
            public_slug: slug,
            public_sharing_enabled: true,
            created_at: created_at.to_rfc3339(),
            updated_at: updated_at.to_rfc3339(),
            pools: req.pools,
        }))
    }

    async fn get_session(
        &self,
        request: Request<GetSessionRequest>,
    ) -> Result<Response<SessionInfo>, Status> {
        let session_id: Uuid = request
            .into_inner()
            .session_id
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid session_id"))?;

        let mut info = fetch_session(&self.db, session_id)
            .await
            .map_err(|e| Status::internal(format!("DB error: {e}")))?
            .ok_or_else(|| Status::not_found("Session not found"))?;

        info.pools = fetch_session_pools(&self.db, session_id)
            .await
            .map_err(|e| Status::internal(format!("DB error: {e}")))?;

        Ok(Response::new(info))
    }

    async fn list_sessions(
        &self,
        request: Request<ListSessionsRequest>,
    ) -> Result<Response<ListSessionsResponse>, Status> {
        let user_id: Uuid = request
            .into_inner()
            .user_id
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid user_id"))?;

        let rows = sqlx::query(
            r#"
            SELECT id, user_id, wallet_id, chain::text, status::text,
                   sell_token, sell_token_symbol, sell_token_decimals,
                   target_token, target_token_symbol, target_token_decimals,
                   strategy::text, total_amount::text, amount_sold::text,
                   pov_percent, max_price_impact, min_buy_trigger_usd,
                   swap_path, public_slug, public_sharing_enabled, created_at, updated_at
            FROM sessions
            WHERE user_id = $1
            ORDER BY created_at DESC
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.db)
        .await
        .map_err(|e| Status::internal(format!("DB error: {e}")))?;

        let mut sessions: Vec<SessionInfo> = rows.iter().map(row_to_session_info).collect();
        for session in &mut sessions {
            let sid: Uuid = session.session_id.parse().unwrap_or_default();
            session.pools = fetch_session_pools(&self.db, sid).await.unwrap_or_default();
        }

        Ok(Response::new(ListSessionsResponse { sessions }))
    }

    async fn update_session_status(
        &self,
        request: Request<UpdateSessionStatusRequest>,
    ) -> Result<Response<SessionInfo>, Status> {
        let req = request.into_inner();
        let session_id: Uuid = req
            .session_id
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid session_id"))?;

        // Validate status transition
        match req.status.as_str() {
            "active" | "paused" | "cancelled" | "completed" => {}
            _ => return Err(Status::invalid_argument("Invalid status")),
        }

        sqlx::query("UPDATE sessions SET status = $1::session_status WHERE id = $2")
            .bind(&req.status)
            .bind(session_id)
            .execute(&self.db)
            .await
            .map_err(|e| Status::internal(format!("DB error: {e}")))?;

        let mut info = fetch_session(&self.db, session_id)
            .await
            .map_err(|e| Status::internal(format!("DB error: {e}")))?
            .ok_or_else(|| Status::not_found("Session not found"))?;

        info.pools = fetch_session_pools(&self.db, session_id)
            .await
            .map_err(|e| Status::internal(format!("DB error: {e}")))?;

        Ok(Response::new(info))
    }

    async fn get_swap_paths(
        &self,
        request: Request<GetSwapPathsRequest>,
    ) -> Result<Response<GetSwapPathsResponse>, Status> {
        let req = request.into_inner();

        // In production, this queries on-chain DEX routers to find optimal paths.
        // For scaffolding, return mock paths.
        info!(
            chain = %req.chain,
            sell = %req.sell_token,
            target = %req.target_token,
            "Computing swap paths"
        );

        let paths = vec![
            SwapPath {
                rank: 1,
                hops: vec!["0xPoolDirectA".into()],
                hop_tokens: vec![req.sell_token.clone(), req.target_token.clone()],
                estimated_output: req.amount.clone(),
                estimated_price_impact: 0.15,
                fee_percent: 0.30,
            },
            SwapPath {
                rank: 2,
                hops: vec!["0xPoolHop1".into(), "0xPoolHop2".into()],
                hop_tokens: vec![
                    req.sell_token.clone(),
                    "0xWETH_PLACEHOLDER".into(),
                    req.target_token.clone(),
                ],
                estimated_output: req.amount.clone(),
                estimated_price_impact: 0.25,
                fee_percent: 0.60,
            },
            SwapPath {
                rank: 3,
                hops: vec!["0xPoolAltA".into(), "0xPoolAltB".into()],
                hop_tokens: vec![
                    req.sell_token.clone(),
                    "0xUSDC_PLACEHOLDER".into(),
                    req.target_token.clone(),
                ],
                estimated_output: req.amount.clone(),
                estimated_price_impact: 0.35,
                fee_percent: 0.55,
            },
        ];

        Ok(Response::new(GetSwapPathsResponse { paths }))
    }

    async fn compute_pool_path(
        &self,
        request: Request<ComputePoolPathRequest>,
    ) -> Result<Response<ComputePoolPathResponse>, Status> {
        let req = request.into_inner();

        let sell = req.sell_token.to_lowercase();
        let target = req.target_token.to_lowercase();
        let t0 = req.token0.to_lowercase();
        let t1 = req.token1.to_lowercase();

        // Determine the "other" token in the pool (the one that isn't the sell token)
        let other_token = if t0 == sell {
            &req.token1
        } else if t1 == sell {
            &req.token0
        } else {
            return Err(Status::invalid_argument(
                "Sell token not found in pool's token pair",
            ));
        };

        // Case 1: Direct pair — pool contains both sell and target tokens
        if t0 == target || t1 == target {
            let path = SwapPath {
                rank: 1,
                hops: vec![req.pool_address.clone()],
                hop_tokens: vec![req.sell_token.clone(), req.target_token.clone()],
                estimated_output: String::new(),
                estimated_price_impact: 0.0,
                fee_percent: if req.fee_tier > 0 {
                    req.fee_tier as f64 / 10000.0
                } else {
                    0.3
                },
            };
            return Ok(Response::new(ComputePoolPathResponse { path: Some(path) }));
        }

        // Case 2: The pool's other token is a base token — try to find a second pool
        // from other_token → target_token
        let rpc_url: alloy::transports::http::reqwest::Url = self
            .rpc_url
            .parse()
            .map_err(|_| Status::internal("Invalid RPC URL"))?;
        let provider = ProviderBuilder::new().connect_http(rpc_url);

        let second_pools = pools::discover_pools(provider, &req.chain, &req.target_token)
            .await
            .map_err(|e| Status::internal(format!("Pool discovery failed: {e}")))?;

        // Find a pool that pairs other_token with target_token
        let other_lower = other_token.to_lowercase();
        if let Some(bridge) = second_pools.iter().find(|p| {
            let pt0 = p.token0.to_lowercase();
            let pt1 = p.token1.to_lowercase();
            (pt0 == other_lower && pt1 == target) || (pt1 == other_lower && pt0 == target)
        }) {
            let path = SwapPath {
                rank: 1,
                hops: vec![req.pool_address.clone(), bridge.pool_address.clone()],
                hop_tokens: vec![
                    req.sell_token.clone(),
                    other_token.to_string(),
                    req.target_token.clone(),
                ],
                estimated_output: String::new(),
                estimated_price_impact: 0.0,
                fee_percent: if req.fee_tier > 0 {
                    req.fee_tier as f64 / 10000.0
                } else {
                    0.3
                } + if bridge.fee_tier > 0 {
                    bridge.fee_tier as f64 / 10000.0
                } else {
                    0.3
                },
            };
            return Ok(Response::new(ComputePoolPathResponse { path: Some(path) }));
        }

        // No route found
        Ok(Response::new(ComputePoolPathResponse { path: None }))
    }

    async fn get_active_sessions_for_token(
        &self,
        request: Request<ActiveSessionsQuery>,
    ) -> Result<Response<ActiveSessionsResponse>, Status> {
        let req = request.into_inner();

        let rows = if !req.pool_address.is_empty() {
            // Query by pool address via session_pools join
            sqlx::query(
                r#"
                SELECT DISTINCT s.id, s.user_id, s.wallet_id, s.chain::text, s.status::text,
                       s.sell_token, s.sell_token_symbol, s.sell_token_decimals,
                       s.target_token, s.target_token_symbol, s.target_token_decimals,
                       s.strategy::text, s.total_amount::text, s.amount_sold::text,
                       s.pov_percent, s.max_price_impact, s.min_buy_trigger_usd,
                       s.swap_path, s.public_slug, s.public_sharing_enabled, s.created_at, s.updated_at
                FROM sessions s
                INNER JOIN session_pools sp ON sp.session_id = s.id
                WHERE s.chain = $1::chain_id
                  AND s.status = 'active'
                  AND LOWER(sp.pool_address) = LOWER($2)
                "#,
            )
            .bind(&req.chain)
            .bind(&req.pool_address)
            .fetch_all(&self.db)
            .await
            .map_err(|e| Status::internal(format!("DB error: {e}")))?
        } else {
            // Fallback: query by sell_token address
            sqlx::query(
                r#"
                SELECT id, user_id, wallet_id, chain::text, status::text,
                       sell_token, sell_token_symbol, sell_token_decimals,
                       target_token, target_token_symbol, target_token_decimals,
                       strategy::text, total_amount::text, amount_sold::text,
                       pov_percent, max_price_impact, min_buy_trigger_usd,
                       swap_path, public_slug, public_sharing_enabled, created_at, updated_at
                FROM sessions
                WHERE chain = $1::chain_id
                  AND sell_token = $2
                  AND status = 'active'
                "#,
            )
            .bind(&req.chain)
            .bind(&req.token_address)
            .fetch_all(&self.db)
            .await
            .map_err(|e| Status::internal(format!("DB error: {e}")))?
        };

        let mut sessions: Vec<SessionInfo> = rows.iter().map(row_to_session_info).collect();
        for session in &mut sessions {
            let sid: Uuid = session.session_id.parse().unwrap_or_default();
            session.pools = fetch_session_pools(&self.db, sid).await.unwrap_or_default();
        }
        Ok(Response::new(ActiveSessionsResponse { sessions }))
    }

    async fn discover_pools(
        &self,
        request: Request<DiscoverPoolsRequest>,
    ) -> Result<Response<DiscoverPoolsResponse>, Status> {
        let req = request.into_inner();

        let rpc_url: alloy::transports::http::reqwest::Url = self
            .rpc_url
            .parse()
            .map_err(|_| Status::internal("Invalid RPC URL configured"))?;
        let provider = ProviderBuilder::new().connect_http(rpc_url);

        let discovered = pools::discover_pools(provider, &req.chain, &req.token_address)
            .await
            .map_err(|e| Status::internal(format!("Pool discovery failed: {e}")))?;

        let pools = discovered
            .into_iter()
            .map(|p| PoolInfo {
                pool_address: p.pool_address,
                pool_type: p.pool_type.to_string(),
                dex_name: p.dex_name,
                token0: p.token0,
                token1: p.token1,
                fee_tier: p.fee_tier,
                reserve0: p.reserve0,
                reserve1: p.reserve1,
                liquidity: p.liquidity,
                balance0: p.balance0,
                balance1: p.balance1,
                token0_price_usd: p.token0_price_usd,
                token1_price_usd: p.token1_price_usd,
                swap_path_json: String::new(),
            })
            .collect();

        Ok(Response::new(DiscoverPoolsResponse { pools }))
    }

    async fn update_session_config(
        &self,
        request: Request<UpdateSessionConfigRequest>,
    ) -> Result<Response<SessionInfo>, Status> {
        let req = request.into_inner();
        let session_id: Uuid = req
            .session_id
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid session_id"))?;

        sqlx::query(
            r#"
            UPDATE sessions
            SET pov_percent = $1, max_price_impact = $2, min_buy_trigger_usd = $3, updated_at = NOW()
            WHERE id = $4
            "#,
        )
        .bind(req.pov_percent)
        .bind(req.max_price_impact)
        .bind(req.min_buy_trigger_usd)
        .bind(session_id)
        .execute(&self.db)
        .await
        .map_err(|e| Status::internal(format!("DB error: {e}")))?;

        let mut info = fetch_session(&self.db, session_id)
            .await
            .map_err(|e| Status::internal(format!("DB error: {e}")))?
            .ok_or_else(|| Status::not_found("Session not found"))?;
        info.pools = fetch_session_pools(&self.db, session_id)
            .await
            .map_err(|e| Status::internal(format!("DB error: {e}")))?;

        Ok(Response::new(info))
    }

    async fn toggle_public_sharing(
        &self,
        request: Request<TogglePublicSharingRequest>,
    ) -> Result<Response<SessionInfo>, Status> {
        let req = request.into_inner();
        let session_id: Uuid = req
            .session_id
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid session_id"))?;

        sqlx::query(
            "UPDATE sessions SET public_sharing_enabled = $1, updated_at = NOW() WHERE id = $2",
        )
        .bind(req.enabled)
        .bind(session_id)
        .execute(&self.db)
        .await
        .map_err(|e| Status::internal(format!("DB error: {e}")))?;

        let mut info = fetch_session(&self.db, session_id)
            .await
            .map_err(|e| Status::internal(format!("DB error: {e}")))?
            .ok_or_else(|| Status::not_found("Session not found"))?;
        info.pools = fetch_session_pools(&self.db, session_id)
            .await
            .map_err(|e| Status::internal(format!("DB error: {e}")))?;

        Ok(Response::new(info))
    }

    async fn get_session_by_slug(
        &self,
        request: Request<GetSessionBySlugRequest>,
    ) -> Result<Response<SessionInfo>, Status> {
        let slug = request.into_inner().slug;

        let row = sqlx::query(
            r#"
            SELECT id, user_id, wallet_id, chain::text, status::text,
                   sell_token, sell_token_symbol, sell_token_decimals,
                   target_token, target_token_symbol, target_token_decimals,
                   strategy::text, total_amount::text, amount_sold::text,
                   pov_percent, max_price_impact, min_buy_trigger_usd,
                   swap_path, public_slug, public_sharing_enabled, created_at, updated_at
            FROM sessions
            WHERE public_slug = $1 AND public_sharing_enabled = TRUE
            "#,
        )
        .bind(&slug)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| Status::internal(format!("DB error: {e}")))?
        .ok_or_else(|| Status::not_found("Session not found or sharing disabled"))?;

        let mut info = row_to_session_info(&row);
        let sid: Uuid = info.session_id.parse().unwrap_or_default();
        info.pools = fetch_session_pools(&self.db, sid)
            .await
            .map_err(|e| Status::internal(format!("DB error: {e}")))?;

        Ok(Response::new(info))
    }
}

// ─────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────

async fn fetch_session(db: &PgPool, session_id: Uuid) -> Result<Option<SessionInfo>, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT id, user_id, wallet_id, chain::text, status::text,
               sell_token, sell_token_symbol, sell_token_decimals,
               target_token, target_token_symbol, target_token_decimals,
               strategy::text, total_amount::text, amount_sold::text,
               pov_percent, max_price_impact, min_buy_trigger_usd,
               swap_path, public_slug, public_sharing_enabled, created_at, updated_at
        FROM sessions
        WHERE id = $1
        "#,
    )
    .bind(session_id)
    .fetch_optional(db)
    .await?;

    Ok(row.as_ref().map(row_to_session_info))
}

fn row_to_session_info(row: &sqlx::postgres::PgRow) -> SessionInfo {
    let id: Uuid = row.get("id");
    let user_id: Uuid = row.get("user_id");
    let wallet_id: Uuid = row.get("wallet_id");
    let created_at: chrono::DateTime<chrono::Utc> = row.get("created_at");
    let updated_at: chrono::DateTime<chrono::Utc> = row.get("updated_at");
    let swap_path: Option<serde_json::Value> = row.get("swap_path");
    let public_slug: Option<String> = row.get("public_slug");
    let public_sharing_enabled: bool = row.try_get("public_sharing_enabled").unwrap_or(true);
    let pov_percent: sqlx::types::BigDecimal = row.get("pov_percent");
    let max_price_impact: sqlx::types::BigDecimal = row.get("max_price_impact");
    let min_buy_trigger_usd: sqlx::types::BigDecimal = row.get("min_buy_trigger_usd");

    SessionInfo {
        session_id: id.to_string(),
        user_id: user_id.to_string(),
        wallet_id: wallet_id.to_string(),
        chain: row.get("chain"),
        status: row.get("status"),
        sell_token: row.get("sell_token"),
        sell_token_symbol: row.get("sell_token_symbol"),
        sell_token_decimals: row.get::<i32, _>("sell_token_decimals") as u32,
        target_token: row.get("target_token"),
        target_token_symbol: row.get("target_token_symbol"),
        target_token_decimals: row.get::<i32, _>("target_token_decimals") as u32,
        strategy: row.get("strategy"),
        total_amount: row.get("total_amount"),
        amount_sold: row.get("amount_sold"),
        pov_percent: pov_percent.to_string().parse().unwrap_or(10.0),
        max_price_impact: max_price_impact.to_string().parse().unwrap_or(1.0),
        min_buy_trigger_usd: min_buy_trigger_usd.to_string().parse().unwrap_or(100.0),
        swap_path_json: swap_path.map(|v| v.to_string()).unwrap_or_default(),
        public_slug: public_slug.unwrap_or_default(),
        public_sharing_enabled,
        created_at: created_at.to_rfc3339(),
        updated_at: updated_at.to_rfc3339(),
        pools: vec![],
    }
}

async fn fetch_session_pools(db: &PgPool, session_id: Uuid) -> Result<Vec<PoolInfo>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT pool_address, pool_type::text, dex_name, token0, token1, fee_tier, swap_path
        FROM session_pools
        WHERE session_id = $1
        ORDER BY dex_name, pool_type::text, fee_tier
        "#,
    )
    .bind(session_id)
    .fetch_all(db)
    .await?;

    Ok(rows
        .iter()
        .map(|r| {
            let sp: Option<serde_json::Value> = r.get("swap_path");
            PoolInfo {
                pool_address: r.get("pool_address"),
                pool_type: r.get("pool_type"),
                dex_name: r.get("dex_name"),
                token0: r.get("token0"),
                token1: r.get("token1"),
                fee_tier: r.get::<i32, _>("fee_tier") as u32,
                reserve0: String::new(),
                reserve1: String::new(),
                liquidity: String::new(),
                balance0: String::new(),
                balance1: String::new(),
                token0_price_usd: 0.0,
                token1_price_usd: 0.0,
                swap_path_json: sp.map(|v| v.to_string()).unwrap_or_default(),
            }
        })
        .collect())
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

    let database_url = &cfg.database.url;
    let rpc_url = cfg.session_api.evm_rpc_url.clone();
    let listen_addr: SocketAddr = SocketAddr::from(([0, 0, 0, 0], cfg.session_api.grpc_port));

    let db = PgPoolOptions::new()
        .max_connections(15)
        .connect(database_url)
        .await
        .context("Failed to connect to PostgreSQL")?;

    info!(%listen_addr, "Session API gRPC server starting");

    let service = SessionServiceImpl::new(db, rpc_url);

    Server::builder()
        .add_service(SessionServiceServer::new(service))
        .serve(listen_addr)
        .await
        .context("gRPC server failed")?;

    Ok(())
}
