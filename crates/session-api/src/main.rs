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
use common::types::{normalize_token_for_chain, PoolType};
use rand::distr::Alphanumeric;
use rand::RngExt;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::time::Instant;
use tokio::sync::RwLock;
use tonic::{transport::Server, Request, Response, Status};
use tracing::info;
use uuid::Uuid;

mod pools;

// ─────────────────────────────────────────────────────────────
// Service State
// ─────────────────────────────────────────────────────────────

/// Cache key: (chain, pool_address, sell_token, target_token) — all lowercased.
type PoolPathCacheKey = (String, String, String, String);

struct CachedPath {
    path: Option<SwapPath>,
    inserted_at: Instant,
}

/// How long a cached pool-path entry stays valid.
const POOL_PATH_CACHE_TTL_SECS: u64 = 300; // 5 minutes

pub struct SessionServiceImpl {
    db: PgPool,
    rpc_url: String,
    pool_path_cache: RwLock<HashMap<PoolPathCacheKey, CachedPath>>,
}

impl SessionServiceImpl {
    pub fn new(db: PgPool, rpc_url: String) -> Self {
        Self {
            db,
            rpc_url,
            pool_path_cache: RwLock::new(HashMap::new()),
        }
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

        if req.pools.is_empty() {
            return Err(Status::invalid_argument(
                "At least one pool with a valid swap_path_json is required",
            ));
        }

        let mut parsed_pool_paths = Vec::with_capacity(req.pools.len());
        for pool in &req.pools {
            parsed_pool_paths.push(parse_pool_swap_path_json(
                &pool.swap_path_json,
                &pool.pool_address,
            )?);
        }

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
        for (pool, swap_path_val) in req.pools.iter().zip(parsed_pool_paths.iter()) {
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
        let user_id_str = request.into_inner().user_id;

        let rows = if user_id_str.is_empty() {
            // Empty user_id = list all sessions (admin use-case)
            sqlx::query(
                r#"
                SELECT id, user_id, wallet_id, chain::text, status::text,
                       sell_token, sell_token_symbol, sell_token_decimals,
                       target_token, target_token_symbol, target_token_decimals,
                       strategy::text, total_amount::text, amount_sold::text,
                       pov_percent, max_price_impact, min_buy_trigger_usd,
                       swap_path, public_slug, public_sharing_enabled, created_at, updated_at
                FROM sessions
                ORDER BY created_at DESC
                "#,
            )
            .fetch_all(&self.db)
            .await
            .map_err(|e| Status::internal(format!("DB error: {e}")))?
        } else {
            let user_id: Uuid = user_id_str
                .parse()
                .map_err(|_| Status::invalid_argument("Invalid user_id"))?;

            sqlx::query(
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
            .map_err(|e| Status::internal(format!("DB error: {e}")))?
        };

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
        let sell_token = normalize_token_for_chain(&req.chain, &req.sell_token);
        let target_token = normalize_token_for_chain(&req.chain, &req.target_token);

        info!(
            chain = %req.chain,
            sell = %sell_token,
            target = %target_token,
            "Computing swap paths"
        );

        let rpc_url: alloy::transports::http::reqwest::Url = self
            .rpc_url
            .parse()
            .map_err(|_| Status::internal("Invalid RPC URL"))?;
        let provider = ProviderBuilder::new().connect_http(rpc_url);

        let sell_pools = pools::discover_pools(provider.clone(), &req.chain, &sell_token)
            .await
            .map_err(|e| Status::internal(format!("Pool discovery failed: {e}")))?;

        let target_pools = if sell_token.eq_ignore_ascii_case(&target_token) {
            Vec::new()
        } else {
            pools::discover_pools(provider, &req.chain, &target_token)
                .await
                .map_err(|e| Status::internal(format!("Pool discovery failed: {e}")))?
        };

        let mut paths: Vec<SwapPath> = sell_pools
            .iter()
            .filter_map(|pool| {
                build_swap_path(
                    &pool.pool_address,
                    &pool.token0,
                    &pool.token1,
                    pool.fee_tier,
                    &sell_token,
                    &target_token,
                    &target_pools,
                    Some(req.amount.as_str()),
                )
            })
            .collect();

        paths = dedupe_paths(paths);
        paths.sort_by(compare_path_score);
        paths.truncate(5);

        for (idx, path) in paths.iter_mut().enumerate() {
            path.rank = (idx + 1) as u32;
        }

        Ok(Response::new(GetSwapPathsResponse { paths }))
    }

    async fn compute_pool_path(
        &self,
        request: Request<ComputePoolPathRequest>,
    ) -> Result<Response<ComputePoolPathResponse>, Status> {
        let req = request.into_inner();
        let sell_token = normalize_token_for_chain(&req.chain, &req.sell_token);
        let target_token = normalize_token_for_chain(&req.chain, &req.target_token);
        let token0 = normalize_token_for_chain(&req.chain, &req.token0);
        let token1 = normalize_token_for_chain(&req.chain, &req.token1);

        if other_token_in_pool(&sell_token, &token0, &token1).is_none() {
            return Err(Status::invalid_argument(
                "Sell token not found in pool's token pair",
            ));
        }

        // ── Check cache ──
        let cache_key: PoolPathCacheKey = (
            req.chain.to_lowercase(),
            req.pool_address.to_lowercase(),
            sell_token.to_lowercase(),
            target_token.to_lowercase(),
        );

        {
            let cache = self.pool_path_cache.read().await;
            if let Some(entry) = cache.get(&cache_key) {
                if entry.inserted_at.elapsed().as_secs() < POOL_PATH_CACHE_TTL_SECS {
                    return Ok(Response::new(ComputePoolPathResponse {
                        path: entry.path.clone(),
                    }));
                }
            }
        }

        let rpc_url: alloy::transports::http::reqwest::Url = self
            .rpc_url
            .parse()
            .map_err(|_| Status::internal("Invalid RPC URL"))?;
        let provider = ProviderBuilder::new().connect_http(rpc_url);

        let second_pools = pools::discover_pools(provider, &req.chain, &target_token)
            .await
            .map_err(|e| Status::internal(format!("Pool discovery failed: {e}")))?;

        let path = build_swap_path(
            &req.pool_address,
            &token0,
            &token1,
            req.fee_tier,
            &sell_token,
            &target_token,
            &second_pools,
            None,
        )
        .map(|mut path| {
            path.rank = 1;
            path
        });

        // ── Store in cache ──
        {
            let mut cache = self.pool_path_cache.write().await;
            cache.insert(
                cache_key,
                CachedPath {
                    path: path.clone(),
                    inserted_at: Instant::now(),
                },
            );
        }

        Ok(Response::new(ComputePoolPathResponse { path }))
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
        let token_address = normalize_token_for_chain(&req.chain, &req.token_address);

        let rpc_url: alloy::transports::http::reqwest::Url = self
            .rpc_url
            .parse()
            .map_err(|_| Status::internal("Invalid RPC URL configured"))?;
        let provider = ProviderBuilder::new().connect_http(rpc_url);

        let discovered = pools::discover_pools(provider, &req.chain, &token_address)
            .await
            .map_err(|e| Status::internal(format!("Pool discovery failed: {e}")))?;

        let pools = discovered
            .into_iter()
            .map(|pool| {
                let swap_path_json = build_default_pool_swap_path_json(&pool, &token_address);
                PoolInfo {
                    pool_address: pool.pool_address,
                    pool_type: pool.pool_type.to_string(),
                    dex_name: pool.dex_name,
                    token0: pool.token0,
                    token1: pool.token1,
                    fee_tier: pool.fee_tier,
                    reserve0: pool.reserve0.unwrap_or_default(),
                    reserve1: pool.reserve1.unwrap_or_default(),
                    liquidity: pool.liquidity.unwrap_or_default(),
                    balance0: pool.balance0.unwrap_or_default(),
                    balance1: pool.balance1.unwrap_or_default(),
                    token0_price_usd: pool.token0_price_usd,
                    token1_price_usd: pool.token1_price_usd,
                    swap_path_json,
                }
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

fn parse_pool_swap_path_json(path: &str, pool_address: &str) -> Result<serde_json::Value, Status> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(Status::invalid_argument(
            "Pool swap_path_json is required for every selected pool",
        ));
    }

    let value = serde_json::from_str::<serde_json::Value>(trimmed)
        .map_err(|_| Status::invalid_argument("Invalid pool swap_path_json"))?;

    let hops = value
        .get("hops")
        .and_then(|v| v.as_array())
        .ok_or_else(|| Status::invalid_argument("Pool route must include hops"))?;
    if hops.is_empty() {
        return Err(Status::invalid_argument(
            "Pool route must include at least one hop",
        ));
    }

    let first_hop = hops
        .first()
        .and_then(|v| v.as_str())
        .ok_or_else(|| Status::invalid_argument("Pool route first hop must be a string"))?;
    if !first_hop.eq_ignore_ascii_case(pool_address) {
        return Err(Status::invalid_argument(
            "Pool route first hop must match pool_address",
        ));
    }

    let hop_tokens = value
        .get("hop_tokens")
        .and_then(|v| v.as_array())
        .ok_or_else(|| Status::invalid_argument("Pool route must include hop_tokens"))?;
    if hop_tokens.len() != hops.len().saturating_add(1) {
        return Err(Status::invalid_argument(
            "Pool route hop_tokens length must equal hops length + 1",
        ));
    }

    Ok(value)
}

fn fee_percent_from_tier(fee_tier: u32) -> f64 {
    if fee_tier > 0 {
        fee_tier as f64 / 10_000.0
    } else {
        0.3
    }
}

fn estimate_output(amount_hint: Option<&str>, fee_percent: f64) -> String {
    let Some(raw_amount) = amount_hint.map(str::trim).filter(|value| !value.is_empty()) else {
        return "0".to_string();
    };

    match raw_amount.parse::<f64>() {
        Ok(amount) if amount.is_finite() && amount > 0.0 => {
            let adjusted = (amount * (1.0 - (fee_percent / 100.0))).max(0.0);
            format!("{adjusted:.0}")
        }
        _ => raw_amount.to_string(),
    }
}

fn estimate_price_impact(hop_count: usize, fee_percent: f64) -> f64 {
    let hop_penalty = (hop_count.max(1) as f64) * 0.1;
    let fee_penalty = fee_percent * 0.5;
    hop_penalty + fee_penalty
}

fn other_token_in_pool<'a>(sell_token: &str, token0: &'a str, token1: &'a str) -> Option<&'a str> {
    if token0.eq_ignore_ascii_case(sell_token) {
        Some(token1)
    } else if token1.eq_ignore_ascii_case(sell_token) {
        Some(token0)
    } else {
        None
    }
}

fn build_swap_path(
    pool_address: &str,
    token0: &str,
    token1: &str,
    fee_tier: u32,
    sell_token: &str,
    target_token: &str,
    target_pools: &[pools::DiscoveredPool],
    amount_hint: Option<&str>,
) -> Option<SwapPath> {
    let other_token = other_token_in_pool(sell_token, token0, token1)?;
    let direct_fee_percent = fee_percent_from_tier(fee_tier);

    if token0.eq_ignore_ascii_case(target_token) || token1.eq_ignore_ascii_case(target_token) {
        return Some(SwapPath {
            rank: 0,
            hops: vec![pool_address.to_string()],
            hop_tokens: vec![sell_token.to_string(), target_token.to_string()],
            estimated_output: estimate_output(amount_hint, direct_fee_percent),
            estimated_price_impact: estimate_price_impact(1, direct_fee_percent),
            fee_percent: direct_fee_percent,
        });
    }

    let other_lower = other_token.to_lowercase();
    let target_lower = target_token.to_lowercase();

    let bridge_pool = target_pools
        .iter()
        .filter(|pool| {
            let token0 = pool.token0.to_lowercase();
            let token1 = pool.token1.to_lowercase();
            (token0 == other_lower && token1 == target_lower)
                || (token1 == other_lower && token0 == target_lower)
        })
        .min_by(|a, b| {
            fee_percent_from_tier(a.fee_tier)
                .partial_cmp(&fee_percent_from_tier(b.fee_tier))
                .unwrap_or(Ordering::Equal)
        })?;

    let total_fee_percent = direct_fee_percent + fee_percent_from_tier(bridge_pool.fee_tier);

    Some(SwapPath {
        rank: 0,
        hops: vec![pool_address.to_string(), bridge_pool.pool_address.clone()],
        hop_tokens: vec![
            sell_token.to_string(),
            other_token.to_string(),
            target_token.to_string(),
        ],
        estimated_output: estimate_output(amount_hint, total_fee_percent),
        estimated_price_impact: estimate_price_impact(2, total_fee_percent),
        fee_percent: total_fee_percent,
    })
}

fn compare_path_score(a: &SwapPath, b: &SwapPath) -> Ordering {
    (a.estimated_price_impact + a.fee_percent)
        .partial_cmp(&(b.estimated_price_impact + b.fee_percent))
        .unwrap_or(Ordering::Equal)
}

fn dedupe_paths(paths: Vec<SwapPath>) -> Vec<SwapPath> {
    let mut seen = HashSet::new();
    let mut unique = Vec::new();

    for path in paths {
        let key = format!("{}|{}", path.hops.join(","), path.hop_tokens.join(","));
        if seen.insert(key) {
            unique.push(path);
        }
    }

    unique
}

fn build_default_pool_swap_path_json(pool: &pools::DiscoveredPool, token_address: &str) -> String {
    let token0_matches = pool.token0.eq_ignore_ascii_case(token_address);
    let token1_matches = pool.token1.eq_ignore_ascii_case(token_address);

    let (from_token, to_token) = if token0_matches {
        (pool.token0.clone(), pool.token1.clone())
    } else if token1_matches {
        (pool.token1.clone(), pool.token0.clone())
    } else {
        (pool.token0.clone(), pool.token1.clone())
    };

    let fee_tiers = if matches!(pool.pool_type, PoolType::V3) {
        vec![pool.fee_tier]
    } else {
        Vec::new()
    };

    serde_json::json!({
        "hops": [pool.pool_address.clone()],
        "hop_tokens": [from_token, to_token],
        "fee_tiers": fee_tiers,
    })
    .to_string()
}

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
        SELECT sp.pool_address, sp.pool_type::text, sp.dex_name, sp.token0, sp.token1, sp.fee_tier, sp.swap_path
        FROM session_pools sp
        WHERE sp.session_id = $1
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
        // .json()
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
