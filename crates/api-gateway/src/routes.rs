use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    Json,
};
use common::proto::{
    session_service_client::SessionServiceClient, wallet_kms_client::WalletKmsClient,
    ComputePoolPathRequest, CreateSessionRequest, CreateWalletRequest, DiscoverPoolsRequest,
    ExportPrivateKeyRequest, GetBalanceRequest, GetSessionBySlugRequest, GetSessionRequest,
    GetSwapPathsRequest, ListSessionsRequest, ListWalletsRequest, PoolInfo,
    TogglePublicSharingRequest, UpdateSessionConfigRequest, UpdateSessionStatusRequest,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use totp_rs::{Algorithm, Secret, TOTP};
use tracing::info;
use uuid::Uuid;

use crate::{auth, jwt_middleware::AuthUser, SharedState};

// ─────────────────────────────────────────────────────────────
// Request/Response types
// ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SignupRequest {
    pub email: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
    pub totp_code: Option<String>,
}

#[derive(Serialize)]
pub struct AuthResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub user_id: String,
}

#[derive(Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

#[derive(Serialize)]
pub struct TotpSetupResponse {
    pub secret: String,
    pub otpauth_url: String,
    pub qr_code_base64: String,
}

#[derive(Deserialize)]
pub struct TotpVerifyRequest {
    pub code: String,
}

#[derive(Deserialize)]
pub struct CreateWalletBody {
    pub chain: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateSessionBody {
    pub wallet_id: String,
    pub chain: String,
    pub sell_token: String,
    pub sell_token_symbol: String,
    pub sell_token_decimals: u32,
    pub target_token: String,
    pub target_token_symbol: String,
    pub target_token_decimals: u32,
    pub total_amount: String,
    pub pov_percent: f64,
    pub max_price_impact: f64,
    pub min_buy_trigger_usd: f64,
    pub swap_path_json: Option<String>,
    pub pools: Option<Vec<PoolInfoBody>>,
}

#[derive(Deserialize)]
pub struct PoolInfoBody {
    pub pool_address: String,
    pub pool_type: String,
    pub dex_name: String,
    pub token0: String,
    pub token1: String,
    pub fee_tier: u32,
    #[serde(default)]
    pub reserve0: String,
    #[serde(default)]
    pub reserve1: String,
    #[serde(default)]
    pub liquidity: String,
    #[serde(default)]
    pub balance0: String,
    #[serde(default)]
    pub balance1: String,
    #[serde(default)]
    pub token0_price_usd: f64,
    #[serde(default)]
    pub token1_price_usd: f64,
    #[serde(default)]
    pub swap_path_json: String,
}

#[derive(Deserialize)]
pub struct DiscoverPoolsBody {
    pub chain: String,
    pub token_address: String,
}

#[derive(Deserialize)]
pub struct ComputePoolPathBody {
    pub chain: String,
    pub sell_token: String,
    pub target_token: String,
    pub pool_address: String,
    pub pool_type: String,
    pub token0: String,
    pub token1: String,
    pub fee_tier: u32,
}

#[derive(Deserialize)]
pub struct UpdateStatusBody {
    pub status: String,
}

#[derive(Deserialize)]
pub struct UpdateSessionConfigBody {
    pub pov_percent: f64,
    pub max_price_impact: f64,
    pub min_buy_trigger_usd: f64,
}

#[derive(Deserialize)]
pub struct ToggleSharingBody {
    pub enabled: bool,
}

#[derive(Deserialize)]
pub struct SwapPathsBody {
    pub chain: String,
    pub sell_token: String,
    pub target_token: String,
    pub amount: String,
}

// ─────────────────────────────────────────────────────────────
// Health
// ─────────────────────────────────────────────────────────────

pub async fn health() -> &'static str {
    "ok"
}

pub async fn list_chains() -> Json<serde_json::Value> {
    let cfg = liquifier_config::Settings::global();
    let chains: Vec<serde_json::Value> = cfg
        .chains
        .iter()
        .filter(|(_, c)| c.enabled)
        .map(|(name, c)| {
            serde_json::json!({
                "name": name,
                "chain_id": c.chain_id,
            })
        })
        .collect();
    Json(serde_json::json!({ "chains": chains }))
}

// ─────────────────────────────────────────────────────────────
// Auth Routes
// ─────────────────────────────────────────────────────────────

pub async fn signup(
    State(state): State<SharedState>,
    Json(body): Json<SignupRequest>,
) -> Result<Json<AuthResponse>, StatusCode> {
    // Validate email format (basic)
    if !body.email.contains('@') || body.email.len() > 256 {
        return Err(StatusCode::BAD_REQUEST);
    }
    if body.password.len() < 8 || body.password.len() > 128 {
        return Err(StatusCode::BAD_REQUEST);
    }

    let password_hash =
        auth::hash_password(&body.password).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let user_id = Uuid::new_v4();
    sqlx::query("INSERT INTO users (id, email, password_hash) VALUES ($1, $2, $3)")
        .bind(user_id)
        .bind(&body.email)
        .bind(&password_hash)
        .execute(&state.db)
        .await
        .map_err(|e| {
            if e.to_string().contains("duplicate key") {
                StatusCode::CONFLICT
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        })?;

    let access_token = auth::create_access_token(&user_id, &state.jwt_secret)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let refresh_token = auth::create_refresh_token(&user_id, &state.jwt_secret)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    info!(user_id = %user_id, email = %body.email, "User signed up");

    Ok(Json(AuthResponse {
        access_token,
        refresh_token,
        user_id: user_id.to_string(),
    }))
}

pub async fn login(
    State(state): State<SharedState>,
    Json(body): Json<LoginRequest>,
) -> Result<Json<AuthResponse>, StatusCode> {
    let row = sqlx::query(
        "SELECT id, password_hash, totp_enabled, totp_secret FROM users WHERE email = $1",
    )
    .bind(&body.email)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::UNAUTHORIZED)?;

    let user_id: Uuid = row.get("id");
    let password_hash: String = row.get("password_hash");
    let totp_enabled: bool = row.get("totp_enabled");

    let valid = auth::verify_password(&body.password, &password_hash)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !valid {
        return Err(StatusCode::UNAUTHORIZED);
    }

    // 2FA check
    if totp_enabled {
        let totp_code = body.totp_code.as_deref().ok_or(StatusCode::FORBIDDEN)?;
        let totp_secret: Vec<u8> = row.get("totp_secret");

        let totp = TOTP::new(Algorithm::SHA1, 6, 1, 30, totp_secret, None, String::new())
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        if !totp
            .check_current(totp_code)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }

    let access_token = auth::create_access_token(&user_id, &state.jwt_secret)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let refresh_token = auth::create_refresh_token(&user_id, &state.jwt_secret)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(AuthResponse {
        access_token,
        refresh_token,
        user_id: user_id.to_string(),
    }))
}

pub async fn refresh_token(
    State(state): State<SharedState>,
    Json(body): Json<RefreshRequest>,
) -> Result<Json<AuthResponse>, StatusCode> {
    let token_data = auth::validate_token(&body.refresh_token, &state.jwt_secret)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    if token_data.claims.token_type != "refresh" {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let user_id: Uuid = token_data
        .claims
        .sub
        .parse()
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    // Verify user still exists
    let exists = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)")
        .bind(user_id)
        .fetch_one(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !exists {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let access_token = auth::create_access_token(&user_id, &state.jwt_secret)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let refresh_token = auth::create_refresh_token(&user_id, &state.jwt_secret)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(AuthResponse {
        access_token,
        refresh_token,
        user_id: user_id.to_string(),
    }))
}

// ─────────────────────────────────────────────────────────────
// 2FA (TOTP)
// ─────────────────────────────────────────────────────────────

pub async fn setup_2fa(
    State(state): State<SharedState>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<TotpSetupResponse>, StatusCode> {
    let secret = Secret::generate_secret();
    let secret_bytes = secret
        .to_bytes()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Fetch email for TOTP label
    let email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
        .bind(user.user_id)
        .fetch_one(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let totp = TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        secret_bytes.clone(),
        Some("Liquifier".to_string()),
        email.clone(),
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let otpauth_url = totp.get_url();
    let qr_code = totp
        .get_qr_base64()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Store secret temporarily (user must verify before enabling)
    sqlx::query("UPDATE users SET totp_secret = $1 WHERE id = $2")
        .bind(&secret_bytes)
        .bind(user.user_id)
        .execute(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(TotpSetupResponse {
        secret: secret.to_encoded().to_string(),
        otpauth_url,
        qr_code_base64: qr_code,
    }))
}

pub async fn verify_2fa(
    State(state): State<SharedState>,
    Extension(user): Extension<AuthUser>,
    Json(body): Json<TotpVerifyRequest>,
) -> Result<StatusCode, StatusCode> {
    let totp_secret: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT totp_secret FROM users WHERE id = $1")
            .bind(user.user_id)
            .fetch_one(&state.db)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let secret = totp_secret.ok_or(StatusCode::BAD_REQUEST)?;

    let totp = TOTP::new(Algorithm::SHA1, 6, 1, 30, secret, None, String::new())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !totp
        .check_current(&body.code)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        return Err(StatusCode::UNAUTHORIZED);
    }

    // Enable 2FA
    sqlx::query("UPDATE users SET totp_enabled = TRUE WHERE id = $1")
        .bind(user.user_id)
        .execute(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(StatusCode::OK)
}

// ─────────────────────────────────────────────────────────────
// Wallet Routes (proxy to KMS via gRPC)
// ─────────────────────────────────────────────────────────────

pub async fn create_wallet(
    State(state): State<SharedState>,
    Extension(user): Extension<AuthUser>,
    Json(body): Json<CreateWalletBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut client = WalletKmsClient::connect(state.kms_addr.clone())
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    let resp = client
        .create_wallet(CreateWalletRequest {
            user_id: user.user_id.to_string(),
            chain: body.chain.unwrap_or_else(|| "ethereum".to_string()),
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_inner();

    Ok(Json(serde_json::json!({
        "wallet_id": resp.wallet_id,
        "address": resp.address,
    })))
}

pub async fn export_wallet(
    State(state): State<SharedState>,
    Extension(user): Extension<AuthUser>,
    Path(wallet_id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut client = WalletKmsClient::connect(state.kms_addr.clone())
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    let resp = client
        .export_private_key(ExportPrivateKeyRequest {
            wallet_id,
            user_id: user.user_id.to_string(),
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_inner();

    Ok(Json(serde_json::json!({
        "private_key": resp.private_key,
        "address": resp.address,
    })))
}

pub async fn list_wallets(
    State(state): State<SharedState>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut client = WalletKmsClient::connect(state.kms_addr.clone())
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    let resp = client
        .list_wallets(ListWalletsRequest {
            user_id: user.user_id.to_string(),
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_inner();

    Ok(Json(
        serde_json::json!({ "wallets": resp.wallets.iter().map(|w| {
        serde_json::json!({
            "wallet_id": w.wallet_id,
            "address": w.address,
            "chain": w.chain,
            "created_at": w.created_at,
        })
    }).collect::<Vec<_>>() }),
    ))
}

pub async fn get_balance(
    State(state): State<SharedState>,
    Extension(_user): Extension<AuthUser>,
    Path(wallet_id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut client = WalletKmsClient::connect(state.kms_addr.clone())
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    let resp = client
        .get_balance(GetBalanceRequest {
            wallet_id,
            token_address: String::new(),
            rpc_url: String::new(),
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_inner();

    Ok(Json(serde_json::json!({
        "balance": resp.balance,
        "decimals": resp.decimals,
    })))
}

// ─────────────────────────────────────────────────────────────
// Session Routes (proxy to Session API via gRPC)
// ─────────────────────────────────────────────────────────────

pub async fn create_session(
    State(state): State<SharedState>,
    Extension(user): Extension<AuthUser>,
    Json(body): Json<CreateSessionBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut client = SessionServiceClient::connect(state.session_addr.clone())
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    let resp = client
        .create_session(CreateSessionRequest {
            user_id: user.user_id.to_string(),
            wallet_id: body.wallet_id,
            chain: body.chain,
            sell_token: body.sell_token,
            sell_token_symbol: body.sell_token_symbol,
            sell_token_decimals: body.sell_token_decimals,
            target_token: body.target_token,
            target_token_symbol: body.target_token_symbol,
            target_token_decimals: body.target_token_decimals,
            strategy: "pov".to_string(),
            total_amount: body.total_amount,
            pov_percent: body.pov_percent,
            max_price_impact: body.max_price_impact,
            min_buy_trigger_usd: body.min_buy_trigger_usd,
            swap_path_json: body.swap_path_json.unwrap_or_default(),
            pools: body
                .pools
                .unwrap_or_default()
                .into_iter()
                .map(|p| PoolInfo {
                    pool_address: p.pool_address,
                    pool_type: p.pool_type,
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
                    swap_path_json: p.swap_path_json,
                })
                .collect(),
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_inner();

    Ok(Json(session_info_to_json(&resp)))
}

pub async fn list_sessions(
    State(state): State<SharedState>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut client = SessionServiceClient::connect(state.session_addr.clone())
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    let resp = client
        .list_sessions(ListSessionsRequest {
            user_id: user.user_id.to_string(),
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_inner();

    let sessions: Vec<_> = resp.sessions.iter().map(session_info_to_json).collect();
    Ok(Json(serde_json::json!({ "sessions": sessions })))
}

pub async fn get_session(
    State(state): State<SharedState>,
    Extension(_user): Extension<AuthUser>,
    Path(session_id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut client = SessionServiceClient::connect(state.session_addr.clone())
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    let resp = client
        .get_session(GetSessionRequest { session_id })
        .await
        .map_err(|e| {
            if e.code() == tonic::Code::NotFound {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        })?
        .into_inner();

    Ok(Json(session_info_to_json(&resp)))
}

pub async fn update_session_status(
    State(state): State<SharedState>,
    Extension(_user): Extension<AuthUser>,
    Path(session_id): Path<String>,
    Json(body): Json<UpdateStatusBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // Validate allowed statuses
    match body.status.as_str() {
        "active" | "paused" | "cancelled" => {}
        _ => return Err(StatusCode::BAD_REQUEST),
    }

    let mut client = SessionServiceClient::connect(state.session_addr.clone())
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    let resp = client
        .update_session_status(UpdateSessionStatusRequest {
            session_id,
            status: body.status,
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_inner();

    Ok(Json(session_info_to_json(&resp)))
}

pub async fn get_swap_paths(
    State(state): State<SharedState>,
    Extension(_user): Extension<AuthUser>,
    Json(body): Json<SwapPathsBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut client = SessionServiceClient::connect(state.session_addr.clone())
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    let resp = client
        .get_swap_paths(GetSwapPathsRequest {
            chain: body.chain,
            sell_token: body.sell_token,
            target_token: body.target_token,
            amount: body.amount,
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_inner();

    let paths: Vec<_> = resp
        .paths
        .iter()
        .map(|p| {
            serde_json::json!({
                "rank": p.rank,
                "hops": p.hops,
                "hop_tokens": p.hop_tokens,
                "estimated_output": p.estimated_output,
                "estimated_price_impact": p.estimated_price_impact,
                "fee_percent": p.fee_percent,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({ "paths": paths })))
}

// ─────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────

pub async fn discover_pools(
    State(state): State<SharedState>,
    Extension(_user): Extension<AuthUser>,
    Json(body): Json<DiscoverPoolsBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut client = SessionServiceClient::connect(state.session_addr.clone())
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    let resp = client
        .discover_pools(DiscoverPoolsRequest {
            chain: body.chain,
            token_address: body.token_address,
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_inner();

    let pools: Vec<serde_json::Value> = resp
        .pools
        .iter()
        .map(|p| {
            serde_json::json!({
                "pool_address": p.pool_address,
                "pool_type": p.pool_type,
                "dex_name": p.dex_name,
                "token0": p.token0,
                "token1": p.token1,
                "fee_tier": p.fee_tier,
                "reserve0": p.reserve0,
                "reserve1": p.reserve1,
                "liquidity": p.liquidity,
                "balance0": p.balance0,
                "balance1": p.balance1,
                "token0_price_usd": p.token0_price_usd,
                "token1_price_usd": p.token1_price_usd,
                "swap_path_json": p.swap_path_json,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({ "pools": pools })))
}

// ─────────────────────────────────────────────────────────────
// Compute Pool Path
// ─────────────────────────────────────────────────────────────

pub async fn compute_pool_path(
    State(state): State<SharedState>,
    Extension(_user): Extension<AuthUser>,
    Json(body): Json<ComputePoolPathBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut client = SessionServiceClient::connect(state.session_api_addr.clone())
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    let resp = client
        .compute_pool_path(ComputePoolPathRequest {
            chain: body.chain,
            sell_token: body.sell_token,
            target_token: body.target_token,
            pool_address: body.pool_address,
            pool_type: body.pool_type,
            token0: body.token0,
            token1: body.token1,
            fee_tier: body.fee_tier,
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_inner();

    let path = resp.path.map(|p| {
        serde_json::json!({
            "rank": p.rank,
            "hops": p.hops,
            "hop_tokens": p.hop_tokens,
            "estimated_output": p.estimated_output,
            "estimated_price_impact": p.estimated_price_impact,
            "fee_percent": p.fee_percent,
        })
    });

    Ok(Json(serde_json::json!({ "path": path })))
}

// ─────────────────────────────────────────────────────────────
// Email Verification
// ─────────────────────────────────────────────────────────────

pub async fn resend_verification(
    State(state): State<SharedState>,
    Extension(user): Extension<AuthUser>,
) -> Result<StatusCode, StatusCode> {
    let token = Uuid::new_v4().to_string();
    let expires = chrono::Utc::now() + chrono::Duration::hours(24);

    sqlx::query(
        "UPDATE users SET verification_token = $1, verification_token_expires_at = $2 WHERE id = $3",
    )
    .bind(&token)
    .bind(expires)
    .bind(user.user_id)
    .execute(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // In production, send email with link containing `token`.
    // For now, log it so it can be used in development.
    info!(user_id = %user.user_id, token = %token, "Email verification token generated (send via email in production)");

    Ok(StatusCode::OK)
}

pub async fn verify_email(
    State(state): State<SharedState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let token = params.get("token").ok_or(StatusCode::BAD_REQUEST)?;

    let result = sqlx::query(
        r#"
        UPDATE users SET email_verified = TRUE, verification_token = NULL, verification_token_expires_at = NULL
        WHERE verification_token = $1 AND verification_token_expires_at > NOW() AND email_verified = FALSE
        "#,
    )
    .bind(token)
    .execute(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if result.rows_affected() == 0 {
        return Err(StatusCode::BAD_REQUEST);
    }

    Ok(Json(serde_json::json!({ "verified": true })))
}

// ─────────────────────────────────────────────────────────────
// Session Config Update
// ─────────────────────────────────────────────────────────────

pub async fn update_session_config(
    State(state): State<SharedState>,
    Extension(_user): Extension<AuthUser>,
    Path(session_id): Path<String>,
    Json(body): Json<UpdateSessionConfigBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut client = SessionServiceClient::connect(state.session_addr.clone())
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    let resp = client
        .update_session_config(UpdateSessionConfigRequest {
            session_id,
            pov_percent: body.pov_percent,
            max_price_impact: body.max_price_impact,
            min_buy_trigger_usd: body.min_buy_trigger_usd,
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_inner();

    Ok(Json(session_info_to_json(&resp)))
}

// ─────────────────────────────────────────────────────────────
// Public Sharing Toggle
// ─────────────────────────────────────────────────────────────

pub async fn toggle_public_sharing(
    State(state): State<SharedState>,
    Extension(_user): Extension<AuthUser>,
    Path(session_id): Path<String>,
    Json(body): Json<ToggleSharingBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut client = SessionServiceClient::connect(state.session_addr.clone())
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    let resp = client
        .toggle_public_sharing(TogglePublicSharingRequest {
            session_id,
            enabled: body.enabled,
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_inner();

    Ok(Json(session_info_to_json(&resp)))
}

// ─────────────────────────────────────────────────────────────
// Public Session by Slug (no auth)
// ─────────────────────────────────────────────────────────────

pub async fn get_session_by_slug(
    State(state): State<SharedState>,
    Path(slug): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut client = SessionServiceClient::connect(state.session_addr.clone())
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    let resp = client
        .get_session_by_slug(GetSessionBySlugRequest { slug })
        .await
        .map_err(|e| {
            if e.code() == tonic::Code::NotFound {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        })?
        .into_inner();

    Ok(Json(session_info_to_json(&resp)))
}

// ─────────────────────────────────────────────────────────────
// User Profile
// ─────────────────────────────────────────────────────────────

pub async fn get_profile(
    State(state): State<SharedState>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let row = sqlx::query(
        "SELECT email, totp_enabled, email_verified, created_at FROM users WHERE id = $1",
    )
    .bind(user.user_id)
    .fetch_one(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let email: String = row.get("email");
    let totp_enabled: bool = row.get("totp_enabled");
    let email_verified: bool = row.try_get("email_verified").unwrap_or(false);
    let created_at: chrono::DateTime<chrono::Utc> = row.get("created_at");

    Ok(Json(serde_json::json!({
        "user_id": user.user_id.to_string(),
        "email": email,
        "totp_enabled": totp_enabled,
        "email_verified": email_verified,
        "created_at": created_at.to_rfc3339(),
    })))
}

// ─────────────────────────────────────────────────────────────
// Token Metadata (ERC20 on-chain lookup)
// ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct TokenMetadataQuery {
    pub chain: String,
    pub address: String,
}

#[derive(Serialize)]
pub struct TokenMetadataResponse {
    pub address: String,
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
}

pub async fn get_token_metadata(
    Extension(_user): Extension<AuthUser>,
    Query(query): Query<TokenMetadataQuery>,
) -> Result<Json<TokenMetadataResponse>, StatusCode> {
    // Validate address format
    let address = query.address.trim().to_lowercase();
    if !address.starts_with("0x") || address.len() != 42 {
        return Err(StatusCode::BAD_REQUEST);
    }
    // Validate hex
    hex::decode(&address[2..]).map_err(|_| StatusCode::BAD_REQUEST)?;

    let cfg = liquifier_config::Settings::global();
    let chain_cfg = cfg
        .chains
        .get(&query.chain)
        .ok_or(StatusCode::BAD_REQUEST)?;
    if chain_cfg.rpc_url.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let client = reqwest::Client::new();

    // ERC20 function selectors:
    // name()     = 0x06fdde03
    // symbol()   = 0x95d89b41
    // decimals() = 0x313ce567

    let (name, symbol, decimals) = tokio::try_join!(
        erc20_call_string(&client, &chain_cfg.rpc_url, &address, "0x06fdde03"),
        erc20_call_string(&client, &chain_cfg.rpc_url, &address, "0x95d89b41"),
        erc20_call_uint8(&client, &chain_cfg.rpc_url, &address, "0x313ce567"),
    )
    .map_err(|_| StatusCode::BAD_GATEWAY)?;

    Ok(Json(TokenMetadataResponse {
        address: query.address,
        name,
        symbol,
        decimals,
    }))
}

async fn eth_call(
    client: &reqwest::Client,
    rpc_url: &str,
    to: &str,
    data: &str,
) -> Result<String, anyhow::Error> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_call",
        "params": [{"to": to, "data": data}, "latest"],
        "id": 1
    });
    let resp: serde_json::Value = client
        .post(rpc_url)
        .json(&body)
        .send()
        .await?
        .json()
        .await?;
    resp.get("result")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("No result from eth_call"))
}

/// Decode an ABI-encoded string return value
fn decode_abi_string(hex_data: &str) -> Option<String> {
    let data = hex::decode(hex_data.strip_prefix("0x").unwrap_or(hex_data)).ok()?;
    if data.len() < 64 {
        return None;
    }
    // offset (first 32 bytes) -> length (next 32 bytes) -> string data
    let offset = usize::from_be_bytes({
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&data[24..32]);
        buf
    });
    if offset + 32 > data.len() {
        return None;
    }
    let length = usize::from_be_bytes({
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&data[offset + 24..offset + 32]);
        buf
    });
    if offset + 32 + length > data.len() {
        return None;
    }
    String::from_utf8(data[offset + 32..offset + 32 + length].to_vec()).ok()
}

async fn erc20_call_string(
    client: &reqwest::Client,
    rpc_url: &str,
    address: &str,
    selector: &str,
) -> Result<String, anyhow::Error> {
    let result = eth_call(client, rpc_url, address, selector).await?;
    decode_abi_string(&result).ok_or_else(|| anyhow::anyhow!("Failed to decode string"))
}

async fn erc20_call_uint8(
    client: &reqwest::Client,
    rpc_url: &str,
    address: &str,
    selector: &str,
) -> Result<u8, anyhow::Error> {
    let result = eth_call(client, rpc_url, address, selector).await?;
    let data = hex::decode(result.strip_prefix("0x").unwrap_or(&result))?;
    if data.len() < 32 {
        return Err(anyhow::anyhow!("Invalid uint8 response"));
    }
    Ok(data[31])
}

// ─────────────────────────────────────────────────────────────

fn session_info_to_json(s: &common::proto::SessionInfo) -> serde_json::Value {
    let pools: Vec<serde_json::Value> = s
        .pools
        .iter()
        .map(|p| {
            serde_json::json!({
                "pool_address": p.pool_address,
                "pool_type": p.pool_type,
                "dex_name": p.dex_name,
                "token0": p.token0,
                "token1": p.token1,
                "fee_tier": p.fee_tier,
                "reserve0": p.reserve0,
                "reserve1": p.reserve1,
                "liquidity": p.liquidity,
                "balance0": p.balance0,
                "balance1": p.balance1,
                "token0_price_usd": p.token0_price_usd,
                "token1_price_usd": p.token1_price_usd,
                "swap_path_json": p.swap_path_json,
            })
        })
        .collect();

    serde_json::json!({
        "session_id": s.session_id,
        "user_id": s.user_id,
        "wallet_id": s.wallet_id,
        "chain": s.chain,
        "status": s.status,
        "sell_token": s.sell_token,
        "sell_token_symbol": s.sell_token_symbol,
        "sell_token_decimals": s.sell_token_decimals,
        "target_token": s.target_token,
        "target_token_symbol": s.target_token_symbol,
        "target_token_decimals": s.target_token_decimals,
        "strategy": s.strategy,
        "total_amount": s.total_amount,
        "amount_sold": s.amount_sold,
        "pov_percent": s.pov_percent,
        "max_price_impact": s.max_price_impact,
        "min_buy_trigger_usd": s.min_buy_trigger_usd,
        "swap_path_json": s.swap_path_json,
        "public_slug": s.public_slug,
        "public_sharing_enabled": s.public_sharing_enabled,
        "created_at": s.created_at,
        "updated_at": s.updated_at,
        "pools": pools,
    })
}
