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
use common::types::{
    display_token_for_chain, is_native_token_placeholder, normalize_token_for_chain,
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
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default = "default_role")]
    pub role: String,
}

fn default_role() -> String {
    "common".to_string()
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email_verified: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub totp_enabled: Option<bool>,
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
pub struct ExportWalletBody {
    pub totp_code: Option<String>,
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

#[derive(Deserialize, Default)]
pub struct GetBalanceQuery {
    #[serde(default)]
    pub token_address: String,
    #[serde(default)]
    pub rpc_url: String,
}

fn default_trades_limit() -> i64 {
    50
}

fn clamp_trades_limit(limit: i64) -> i64 {
    limit.clamp(1, 200)
}

#[derive(Deserialize)]
pub struct TradesQuery {
    #[serde(default = "default_trades_limit")]
    pub limit: i64,
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
) -> Result<Json<serde_json::Value>, StatusCode> {
    // Validate email format (basic)
    if !body.email.contains('@') || body.email.len() > 256 {
        return Err(StatusCode::BAD_REQUEST);
    }
    if body.password.len() < 8 || body.password.len() > 128 {
        return Err(StatusCode::BAD_REQUEST);
    }

    let role = match body.role.as_str() {
        "common" => "common",
        _ => "common", // Only common users can sign up via the API
    };

    // Common users must provide a username
    if role == "common" {
        match &body.username {
            Some(u) if u.len() >= 3 && u.len() <= 50 => {}
            _ => return Err(StatusCode::BAD_REQUEST),
        }
    }

    let password_hash =
        auth::hash_password(&body.password).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Check if user already exists with unverified email
    let existing = sqlx::query(
        "SELECT id, email_verified, totp_enabled, role::text as role FROM users WHERE email = $1",
    )
    .bind(&body.email)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if let Some(row) = existing {
        let user_id: Uuid = row.get("id");
        let email_verified: bool = row.get("email_verified");
        let totp_enabled: bool = row.get("totp_enabled");
        let existing_role: String = row.get("role");

        if !email_verified {
            // Re-generate verification token for existing unverified user
            let token = Uuid::new_v4().to_string();
            let expires = chrono::Utc::now() + chrono::Duration::hours(24);
            sqlx::query(
                "UPDATE users SET verification_token = $1, verification_token_expires_at = $2 WHERE id = $3",
            )
            .bind(&token)
            .bind(expires)
            .bind(user_id)
            .execute(&state.db)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

            info!(user_id = %user_id, token = %token, "Re-sent verification token for existing unverified user");

            if let Some(ref email_sender) = state.email {
                email_sender
                    .send_verification_email(&body.email, &token)
                    .await;
            }

            return Ok(Json(serde_json::json!({
                "status": "email_verification_required",
                "user_id": user_id.to_string(),
                "role": existing_role,
                "message": "Please check your email to verify your account."
            })));
        }

        if existing_role == "admin" && email_verified && !totp_enabled {
            // Issue temporary token so they can set up TOTP
            let access_token = auth::create_access_token(&user_id, &state.jwt_secret)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            return Ok(Json(serde_json::json!({
                "status": "totp_setup_required",
                "user_id": user_id.to_string(),
                "role": "admin",
                "access_token": access_token,
                "message": "Email verified. Please set up 2FA to continue."
            })));
        }

        // Fully set up user trying to re-register
        return Err(StatusCode::CONFLICT);
    }

    let user_id = Uuid::new_v4();
    let verification_token = Uuid::new_v4().to_string();
    let expires = chrono::Utc::now() + chrono::Duration::hours(24);

    sqlx::query(
        "INSERT INTO users (id, email, password_hash, verification_token, verification_token_expires_at, role, username) VALUES ($1, $2, $3, $4, $5, $6::user_role, $7)",
    )
    .bind(user_id)
    .bind(&body.email)
    .bind(&password_hash)
    .bind(&verification_token)
    .bind(expires)
    .bind(role)
    .bind(&body.username)
    .execute(&state.db)
    .await
    .map_err(|e| {
        if e.to_string().contains("duplicate key") {
            StatusCode::CONFLICT
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    })?;

    info!(user_id = %user_id, email = %body.email, role = %role, token = %verification_token, "User signed up — verification token generated");

    if let Some(ref email_sender) = state.email {
        email_sender
            .send_verification_email(&body.email, &verification_token)
            .await;
    }

    Ok(Json(serde_json::json!({
        "status": "email_verification_required",
        "user_id": user_id.to_string(),
        "role": role,
        "message": "Account created. Please check your email to verify your account."
    })))
}

pub async fn login(
    State(state): State<SharedState>,
    Json(body): Json<LoginRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let row = sqlx::query(
        "SELECT id, password_hash, email_verified, totp_enabled, totp_secret, role::text as role FROM users WHERE email = $1",
    )
    .bind(&body.email)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::UNAUTHORIZED)?;

    let user_id: Uuid = row.get("id");
    let password_hash: String = row.get("password_hash");
    let email_verified: bool = row.get("email_verified");
    let totp_enabled: bool = row.get("totp_enabled");
    let role: String = row.get("role");

    let valid = auth::verify_password(&body.password, &password_hash)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !valid {
        return Err(StatusCode::UNAUTHORIZED);
    }

    // Step 1: Email must be verified
    if !email_verified {
        // Re-generate verification token
        let token = Uuid::new_v4().to_string();
        let expires = chrono::Utc::now() + chrono::Duration::hours(24);
        sqlx::query(
            "UPDATE users SET verification_token = $1, verification_token_expires_at = $2 WHERE id = $3",
        )
        .bind(&token)
        .bind(expires)
        .bind(user_id)
        .execute(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        info!(user_id = %user_id, token = %token, "Login attempt with unverified email — re-sent verification token");

        if let Some(ref email_sender) = state.email {
            email_sender
                .send_verification_email(&body.email, &token)
                .await;
        }

        return Ok(Json(serde_json::json!({
            "status": "email_verification_required",
            "user_id": user_id.to_string(),
            "role": role,
            "message": "Please verify your email before signing in. A new verification link has been sent."
        })));
    }

    // Common users: no 2FA required — issue tokens directly after email verification
    if role == "common" {
        let access_token =
            auth::create_access_token_with_role(&user_id, &state.jwt_secret, "common")
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let refresh_token =
            auth::create_refresh_token_with_role(&user_id, &state.jwt_secret, "common")
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        return Ok(Json(serde_json::json!({
            "status": "authenticated",
            "access_token": access_token,
            "refresh_token": refresh_token,
            "user_id": user_id.to_string(),
            "role": "common"
        })));
    }

    // Admin users: Step 2: TOTP must be set up
    if !totp_enabled {
        // Issue a temporary access token so user can call /2fa/setup and /2fa/verify
        let access_token = auth::create_access_token(&user_id, &state.jwt_secret)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        return Ok(Json(serde_json::json!({
            "status": "totp_setup_required",
            "user_id": user_id.to_string(),
            "role": "admin",
            "access_token": access_token,
            "message": "Please set up 2FA to continue."
        })));
    }

    // Step 3: Verify TOTP code
    let totp_code = match body.totp_code.as_deref() {
        Some(code) if !code.is_empty() => code,
        _ => {
            return Ok(Json(serde_json::json!({
                "status": "totp_required",
                "user_id": user_id.to_string(),
                "role": "admin",
                "message": "Please enter your 2FA code."
            })));
        }
    };

    let totp_secret: Vec<u8> = row.get("totp_secret");
    let totp = TOTP::new(Algorithm::SHA1, 6, 1, 30, totp_secret, None, String::new())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !totp
        .check_current(totp_code)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let access_token = auth::create_access_token_with_role(&user_id, &state.jwt_secret, "admin")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let refresh_token = auth::create_refresh_token_with_role(&user_id, &state.jwt_secret, "admin")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(serde_json::json!({
        "status": "authenticated",
        "access_token": access_token,
        "refresh_token": refresh_token,
        "user_id": user_id.to_string(),
        "role": "admin"
    })))
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

    let role = token_data.claims.role.clone();

    // Verify user still exists
    let exists = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)")
        .bind(user_id)
        .fetch_one(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !exists {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let access_token = auth::create_access_token_with_role(&user_id, &state.jwt_secret, &role)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let refresh_token = auth::create_refresh_token_with_role(&user_id, &state.jwt_secret, &role)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(AuthResponse {
        access_token,
        refresh_token,
        user_id: user_id.to_string(),
        status: None,
        email_verified: None,
        totp_enabled: None,
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
) -> Result<Json<serde_json::Value>, StatusCode> {
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

    Ok(Json(serde_json::json!({ "status": "2fa_enabled" })))
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
            chain: body.chain.unwrap_or_else(|| "bsc".to_string()),
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
    Json(body): Json<ExportWalletBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // Require TOTP verification
    let row = sqlx::query("SELECT totp_enabled, totp_secret FROM users WHERE id = $1")
        .bind(user.user_id)
        .fetch_one(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let totp_enabled: bool = row.get("totp_enabled");
    if !totp_enabled {
        return Err(StatusCode::FORBIDDEN);
    }

    let totp_code = body.totp_code.as_deref().ok_or(StatusCode::BAD_REQUEST)?;
    let totp_secret: Vec<u8> = row.get("totp_secret");

    let totp = TOTP::new(Algorithm::SHA1, 6, 1, 30, totp_secret, None, String::new())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !totp
        .check_current(totp_code)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        return Err(StatusCode::UNAUTHORIZED);
    }

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
    Query(query): Query<GetBalanceQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut client = WalletKmsClient::connect(state.kms_addr.clone())
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    let token_address = if is_native_token_placeholder(&query.token_address) {
        // Wallet balance does not include chain in query; BSC is the active chain.
        normalize_token_for_chain("bsc", &query.token_address)
    } else {
        query.token_address.clone()
    };

    let resp = client
        .get_balance(GetBalanceRequest {
            wallet_id,
            token_address,
            rpc_url: query.rpc_url,
        })
        .await
        .map_err(|e| match e.code() {
            tonic::Code::InvalidArgument => StatusCode::BAD_REQUEST,
            tonic::Code::NotFound => StatusCode::NOT_FOUND,
            tonic::Code::FailedPrecondition => StatusCode::BAD_REQUEST,
            tonic::Code::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        })?
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

    let chain = body.chain.clone();
    let sell_token = normalize_token_for_chain(&chain, &body.sell_token);
    let target_token = normalize_token_for_chain(&chain, &body.target_token);
    let pools = body.pools.unwrap_or_default();

    if pools.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    if pools
        .iter()
        .any(|p| !validate_pool_swap_path_json(&p.swap_path_json, &p.pool_address))
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    let grpc_pools: Vec<PoolInfo> = pools
        .into_iter()
        .map(|p| PoolInfo {
            pool_address: p.pool_address,
            pool_type: p.pool_type,
            dex_name: p.dex_name,
            token0: normalize_token_for_chain(&chain, &p.token0),
            token1: normalize_token_for_chain(&chain, &p.token1),
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
        .collect();

    // ── Check wallet balance for sell token ──
    let mut kms_client = WalletKmsClient::connect(state.kms_addr.clone())
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    let balance_resp = kms_client
        .get_balance(GetBalanceRequest {
            wallet_id: body.wallet_id.clone(),
            token_address: sell_token.clone(),
            rpc_url: String::new(),
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_inner();

    let wallet_balance = balance_resp.balance.parse::<u128>().unwrap_or(0);
    let requested_amount = body
        .total_amount
        .parse::<u128>()
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    if requested_amount > wallet_balance {
        return Err(StatusCode::BAD_REQUEST);
    }

    let resp = client
        .create_session(CreateSessionRequest {
            user_id: if user.role == "admin" {
                // Admin creating session on another user's wallet: look up wallet owner
                let owner_row = sqlx::query_scalar::<_, String>(
                    "SELECT user_id::text FROM wallets WHERE id = $1::uuid",
                )
                .bind(&body.wallet_id)
                .fetch_optional(&state.db)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
                .ok_or(StatusCode::NOT_FOUND)?;
                owner_row
            } else {
                user.user_id.to_string()
            },
            wallet_id: body.wallet_id,
            chain: chain.clone(),
            sell_token,
            sell_token_symbol: body.sell_token_symbol,
            sell_token_decimals: body.sell_token_decimals,
            target_token,
            target_token_symbol: body.target_token_symbol,
            target_token_decimals: body.target_token_decimals,
            strategy: "pov".to_string(),
            total_amount: body.total_amount,
            pov_percent: body.pov_percent,
            max_price_impact: body.max_price_impact,
            min_buy_trigger_usd: body.min_buy_trigger_usd,
            swap_path_json: body.swap_path_json.unwrap_or_default(),
            pools: grpc_pools,
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
            user_id: if user.role == "admin" {
                String::new() // Admin sees all sessions
            } else {
                user.user_id.to_string()
            },
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

    let chain = body.chain.clone();
    let sell_token = normalize_token_for_chain(&chain, &body.sell_token);
    let target_token = normalize_token_for_chain(&chain, &body.target_token);

    let resp = client
        .get_swap_paths(GetSwapPathsRequest {
            chain: chain.clone(),
            sell_token,
            target_token,
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
                "hop_tokens": p.hop_tokens.iter().map(|t| display_token_for_chain(&chain, t)).collect::<Vec<_>>(),
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

    let chain = body.chain.clone();
    let token_address = normalize_token_for_chain(&chain, &body.token_address);

    let resp = client
        .discover_pools(DiscoverPoolsRequest {
            chain: chain.clone(),
            token_address,
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
                "token0": display_token_for_chain(&chain, &p.token0),
                "token1": display_token_for_chain(&chain, &p.token1),
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
    let mut client = SessionServiceClient::connect(state.session_addr.clone())
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    let chain = body.chain.clone();
    let sell_token = normalize_token_for_chain(&chain, &body.sell_token);
    let target_token = normalize_token_for_chain(&chain, &body.target_token);
    let token0 = normalize_token_for_chain(&chain, &body.token0);
    let token1 = normalize_token_for_chain(&chain, &body.token1);

    let resp = client
        .compute_pool_path(ComputePoolPathRequest {
            chain: chain.clone(),
            sell_token,
            target_token,
            pool_address: body.pool_address,
            pool_type: body.pool_type,
            token0,
            token1,
            fee_tier: body.fee_tier,
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_inner();

    let path = resp.path.map(|p| {
        serde_json::json!({
            "rank": p.rank,
            "hops": p.hops,
            "hop_tokens": p.hop_tokens.iter().map(|t| display_token_for_chain(&chain, t)).collect::<Vec<_>>(),
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

    // Send verification email (falls back to log-only if SMTP not configured)
    info!(user_id = %user.user_id, token = %token, "Email verification token generated");

    if let Some(ref email_sender) = state.email {
        let email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
            .bind(user.user_id)
            .fetch_one(&state.db)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        email_sender.send_verification_email(&email, &token).await;
    }

    Ok(StatusCode::OK)
}

pub async fn verify_email(
    State(state): State<SharedState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let token = params.get("token").ok_or(StatusCode::BAD_REQUEST)?;

    // Get the user before verifying so we know their role
    let user_row = sqlx::query(
        "SELECT id, role::text as role FROM users WHERE verification_token = $1 AND verification_token_expires_at > NOW() AND email_verified = FALSE",
    )
    .bind(token)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::BAD_REQUEST)?;

    let user_id: Uuid = user_row.get("id");
    let role: String = user_row.get("role");

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

    // Auto-create wallet for common users
    if role == "common" {
        if let Ok(mut client) = WalletKmsClient::connect(state.kms_addr.clone()).await {
            let _ = client
                .create_wallet(CreateWalletRequest {
                    user_id: user_id.to_string(),
                    chain: "bsc".to_string(),
                })
                .await;
            info!(user_id = %user_id, "Auto-created wallet for common user");
        }
    }

    Ok(Json(serde_json::json!({ "verified": true, "role": role })))
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

pub async fn get_session_trades(
    State(state): State<SharedState>,
    Extension(user): Extension<AuthUser>,
    Path(session_id): Path<String>,
    Query(query): Query<TradesQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let session_id = session_id
        .parse::<Uuid>()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let limit = clamp_trades_limit(query.limit);

    let rows = sqlx::query(
        r#"
        SELECT
            t.id::text AS trade_id,
            t.session_id::text AS session_id,
            t.chain::text AS chain,
            t.status::text AS status,
            t.sell_amount::text AS sell_amount,
            COALESCE(t.final_received, t.trigger_buy_amount)::text AS received_amount,
            COALESCE(NULLIF(t.sell_tx_hash, ''), NULLIF(t.route_tx_hash, ''), t.trigger_tx_hash, '') AS tx_hash,
            COALESCE(t.price_impact_bps, 0) AS price_impact_bps,
            t.failure_reason AS failure_reason,
            COALESCE(t.executed_at, t.created_at) AS executed_at
        FROM trades t
        INNER JOIN sessions s ON s.id = t.session_id
        WHERE t.session_id = $1 AND s.user_id = $2
        ORDER BY COALESCE(t.executed_at, t.created_at) DESC
        LIMIT $3
        "#,
    )
    .bind(session_id)
    .bind(user.user_id)
    .bind(limit)
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let trades: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            let executed_at: chrono::DateTime<chrono::Utc> = row.get("executed_at");
            serde_json::json!({
                "trade_id": row.get::<String, _>("trade_id"),
                "session_id": row.get::<String, _>("session_id"),
                "chain": row.get::<String, _>("chain"),
                "status": row.get::<String, _>("status"),
                "sell_amount": row.get::<String, _>("sell_amount"),
                "received_amount": row.get::<String, _>("received_amount"),
                "tx_hash": row.get::<String, _>("tx_hash"),
                "price_impact_bps": row.get::<i32, _>("price_impact_bps"),
                "failure_reason": row.get::<Option<String>, _>("failure_reason"),
                "executed_at": executed_at.to_rfc3339(),
            })
        })
        .collect();

    Ok(Json(serde_json::json!({ "trades": trades })))
}

pub async fn get_public_session_trades(
    State(state): State<SharedState>,
    Path(slug): Path<String>,
    Query(query): Query<TradesQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let limit = clamp_trades_limit(query.limit);

    let rows = sqlx::query(
        r#"
        SELECT
            t.id::text AS trade_id,
            t.session_id::text AS session_id,
            t.chain::text AS chain,
            t.status::text AS status,
            t.sell_amount::text AS sell_amount,
            COALESCE(t.final_received, t.trigger_buy_amount)::text AS received_amount,
            COALESCE(NULLIF(t.sell_tx_hash, ''), NULLIF(t.route_tx_hash, ''), t.trigger_tx_hash, '') AS tx_hash,
            COALESCE(t.price_impact_bps, 0) AS price_impact_bps,
            t.failure_reason AS failure_reason,
            COALESCE(t.executed_at, t.created_at) AS executed_at
        FROM trades t
        INNER JOIN sessions s ON s.id = t.session_id
        WHERE s.public_slug = $1 AND s.public_sharing_enabled = TRUE
        ORDER BY COALESCE(t.executed_at, t.created_at) DESC
        LIMIT $2
        "#,
    )
    .bind(slug)
    .bind(limit)
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let trades: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            let executed_at: chrono::DateTime<chrono::Utc> = row.get("executed_at");
            serde_json::json!({
                "trade_id": row.get::<String, _>("trade_id"),
                "session_id": row.get::<String, _>("session_id"),
                "chain": row.get::<String, _>("chain"),
                "status": row.get::<String, _>("status"),
                "sell_amount": row.get::<String, _>("sell_amount"),
                "received_amount": row.get::<String, _>("received_amount"),
                "tx_hash": row.get::<String, _>("tx_hash"),
                "price_impact_bps": row.get::<i32, _>("price_impact_bps"),
                "failure_reason": row.get::<Option<String>, _>("failure_reason"),
                "executed_at": executed_at.to_rfc3339(),
            })
        })
        .collect();

    Ok(Json(serde_json::json!({ "trades": trades })))
}

// ─────────────────────────────────────────────────────────────
// User Profile
// ─────────────────────────────────────────────────────────────

pub async fn get_profile(
    State(state): State<SharedState>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let row = sqlx::query(
        "SELECT email, username, totp_enabled, email_verified, role::text as role, created_at FROM users WHERE id = $1",
    )
    .bind(user.user_id)
    .fetch_one(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let email: String = row.get("email");
    let username: Option<String> = row.get("username");
    let totp_enabled: bool = row.get("totp_enabled");
    let email_verified: bool = row.try_get("email_verified").unwrap_or(false);
    let role: String = row.get("role");
    let created_at: chrono::DateTime<chrono::Utc> = row.get("created_at");

    Ok(Json(serde_json::json!({
        "user_id": user.user_id.to_string(),
        "email": email,
        "username": username,
        "totp_enabled": totp_enabled,
        "email_verified": email_verified,
        "role": role,
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
    let chain = query.chain.trim().to_lowercase();

    // Validate address format
    let input_address = query.address.trim().to_lowercase();
    if !input_address.starts_with("0x") || input_address.len() != 42 {
        return Err(StatusCode::BAD_REQUEST);
    }
    // Validate hex
    hex::decode(&input_address[2..]).map_err(|_| StatusCode::BAD_REQUEST)?;

    if chain == "bsc" && is_native_token_placeholder(&input_address) {
        return Ok(Json(TokenMetadataResponse {
            address: query.address,
            name: "BNB".to_string(),
            symbol: "BNB".to_string(),
            decimals: 18,
        }));
    }

    let address = normalize_token_for_chain(&chain, &input_address).to_lowercase();

    let cfg = liquifier_config::Settings::global();
    let chain_cfg = cfg.chains.get(&chain).ok_or(StatusCode::BAD_REQUEST)?;
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

fn validate_pool_swap_path_json(path: &str, pool_address: &str) -> bool {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return false;
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return false;
    };

    let Some(hops) = value.get("hops").and_then(|v| v.as_array()) else {
        return false;
    };
    if hops.is_empty() {
        return false;
    }

    let Some(first_hop) = hops.first().and_then(|v| v.as_str()) else {
        return false;
    };
    if !first_hop.eq_ignore_ascii_case(pool_address) {
        return false;
    }

    let Some(hop_tokens) = value.get("hop_tokens").and_then(|v| v.as_array()) else {
        return false;
    };

    hop_tokens.len() == hops.len().saturating_add(1)
}

// ─────────────────────────────────────────────────────────────

fn session_info_to_json(s: &common::proto::SessionInfo) -> serde_json::Value {
    let sell_token = display_token_for_chain(&s.chain, &s.sell_token);
    let target_token = display_token_for_chain(&s.chain, &s.target_token);

    let pools: Vec<serde_json::Value> = s
        .pools
        .iter()
        .map(|p| {
            serde_json::json!({
                "pool_address": p.pool_address,
                "pool_type": p.pool_type,
                "dex_name": p.dex_name,
                "token0": display_token_for_chain(&s.chain, &p.token0),
                "token1": display_token_for_chain(&s.chain, &p.token1),
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
        "sell_token": sell_token,
        "sell_token_symbol": s.sell_token_symbol,
        "sell_token_decimals": s.sell_token_decimals,
        "target_token": target_token,
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

// ─────────────────────────────────────────────────────────────
// Refund Requests
// ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateRefundRequest {
    pub wallet_id: String,
    pub amount: String,
    pub token_address: String,
    pub token_symbol: String,
}

#[derive(Deserialize)]
pub struct UpdateRefundStatusBody {
    pub status: String,
    pub admin_note: Option<String>,
}

pub async fn create_refund_request(
    State(state): State<SharedState>,
    Extension(user): Extension<AuthUser>,
    Json(body): Json<CreateRefundRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // Only common users can request refunds
    if user.role != "common" {
        return Err(StatusCode::FORBIDDEN);
    }

    let wallet_id: Uuid = body
        .wallet_id
        .parse()
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    // Verify wallet belongs to user
    let wallet_exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM wallets WHERE id = $1 AND user_id = $2)",
    )
    .bind(wallet_id)
    .bind(user.user_id)
    .fetch_one(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !wallet_exists {
        return Err(StatusCode::NOT_FOUND);
    }

    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO refund_requests (id, user_id, wallet_id, amount, token_address, token_symbol) VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(user.user_id)
    .bind(wallet_id)
    .bind(&body.amount)
    .bind(&body.token_address)
    .bind(&body.token_symbol)
    .execute(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(serde_json::json!({
        "refund_id": id.to_string(),
        "status": "pending",
        "message": "Refund request submitted."
    })))
}

pub async fn list_my_refund_requests(
    State(state): State<SharedState>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let rows = sqlx::query(
        r#"
        SELECT id::text, wallet_id::text, amount, token_address, token_symbol, status, admin_note, created_at, updated_at
        FROM refund_requests WHERE user_id = $1 ORDER BY created_at DESC
        "#,
    )
    .bind(user.user_id)
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let refunds: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let created_at: chrono::DateTime<chrono::Utc> = r.get("created_at");
            let updated_at: chrono::DateTime<chrono::Utc> = r.get("updated_at");
            serde_json::json!({
                "refund_id": r.get::<String, _>("id"),
                "wallet_id": r.get::<String, _>("wallet_id"),
                "amount": r.get::<String, _>("amount"),
                "token_address": r.get::<String, _>("token_address"),
                "token_symbol": r.get::<String, _>("token_symbol"),
                "status": r.get::<String, _>("status"),
                "admin_note": r.get::<Option<String>, _>("admin_note"),
                "created_at": created_at.to_rfc3339(),
                "updated_at": updated_at.to_rfc3339(),
            })
        })
        .collect();

    Ok(Json(serde_json::json!({ "refunds": refunds })))
}

// ─────────────────────────────────────────────────────────────
// Admin: User Management
// ─────────────────────────────────────────────────────────────

pub async fn admin_list_users(
    State(state): State<SharedState>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if user.role != "admin" {
        return Err(StatusCode::FORBIDDEN);
    }

    let rows = sqlx::query(
        r#"
        SELECT u.id::text, u.email, u.username, u.role::text as role, u.email_verified, u.totp_enabled, u.created_at,
               COUNT(DISTINCT w.id) as wallet_count,
               COUNT(DISTINCT s.id) as session_count
        FROM users u
        LEFT JOIN wallets w ON w.user_id = u.id
        LEFT JOIN sessions s ON s.wallet_id = w.id
        GROUP BY u.id
        ORDER BY u.created_at DESC
        "#,
    )
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let users: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let created_at: chrono::DateTime<chrono::Utc> = r.get("created_at");
            serde_json::json!({
                "user_id": r.get::<String, _>("id"),
                "email": r.get::<String, _>("email"),
                "username": r.get::<Option<String>, _>("username"),
                "role": r.get::<String, _>("role"),
                "email_verified": r.get::<bool, _>("email_verified"),
                "totp_enabled": r.get::<bool, _>("totp_enabled"),
                "wallet_count": r.get::<i64, _>("wallet_count"),
                "session_count": r.get::<i64, _>("session_count"),
                "created_at": created_at.to_rfc3339(),
            })
        })
        .collect();

    Ok(Json(serde_json::json!({ "users": users })))
}

pub async fn admin_get_user_wallets(
    State(state): State<SharedState>,
    Extension(user): Extension<AuthUser>,
    Path(target_user_id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if user.role != "admin" {
        return Err(StatusCode::FORBIDDEN);
    }

    let target_id: Uuid = target_user_id
        .parse()
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    let mut client = WalletKmsClient::connect(state.kms_addr.clone())
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    let resp = client
        .list_wallets(ListWalletsRequest {
            user_id: target_id.to_string(),
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

pub async fn admin_export_user_wallet(
    State(state): State<SharedState>,
    Extension(user): Extension<AuthUser>,
    Path((target_user_id, wallet_id)): Path<(String, String)>,
    Json(body): Json<ExportWalletBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if user.role != "admin" {
        return Err(StatusCode::FORBIDDEN);
    }

    // Admin must verify their own TOTP
    let row = sqlx::query("SELECT totp_enabled, totp_secret FROM users WHERE id = $1")
        .bind(user.user_id)
        .fetch_one(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let totp_enabled: bool = row.get("totp_enabled");
    if !totp_enabled {
        return Err(StatusCode::FORBIDDEN);
    }

    let totp_code = body.totp_code.as_deref().ok_or(StatusCode::BAD_REQUEST)?;
    let totp_secret: Vec<u8> = row.get("totp_secret");

    let totp = TOTP::new(Algorithm::SHA1, 6, 1, 30, totp_secret, None, String::new())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !totp
        .check_current(totp_code)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let mut client = WalletKmsClient::connect(state.kms_addr.clone())
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    let resp = client
        .export_private_key(ExportPrivateKeyRequest {
            wallet_id,
            user_id: target_user_id,
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_inner();

    Ok(Json(serde_json::json!({
        "private_key": resp.private_key,
        "address": resp.address,
    })))
}

pub async fn admin_list_refund_requests(
    State(state): State<SharedState>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if user.role != "admin" {
        return Err(StatusCode::FORBIDDEN);
    }

    let rows = sqlx::query(
        r#"
        SELECT r.id::text, r.user_id::text, r.wallet_id::text, r.amount, r.token_address,
               r.token_symbol, r.status, r.admin_note, r.created_at, r.updated_at,
               u.email, u.username
        FROM refund_requests r
        JOIN users u ON u.id = r.user_id
        ORDER BY r.created_at DESC
        "#,
    )
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let refunds: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let created_at: chrono::DateTime<chrono::Utc> = r.get("created_at");
            let updated_at: chrono::DateTime<chrono::Utc> = r.get("updated_at");
            serde_json::json!({
                "refund_id": r.get::<String, _>("id"),
                "user_id": r.get::<String, _>("user_id"),
                "wallet_id": r.get::<String, _>("wallet_id"),
                "email": r.get::<String, _>("email"),
                "username": r.get::<Option<String>, _>("username"),
                "amount": r.get::<String, _>("amount"),
                "token_address": r.get::<String, _>("token_address"),
                "token_symbol": r.get::<String, _>("token_symbol"),
                "status": r.get::<String, _>("status"),
                "admin_note": r.get::<Option<String>, _>("admin_note"),
                "created_at": created_at.to_rfc3339(),
                "updated_at": updated_at.to_rfc3339(),
            })
        })
        .collect();

    Ok(Json(serde_json::json!({ "refunds": refunds })))
}

pub async fn admin_update_refund_status(
    State(state): State<SharedState>,
    Extension(user): Extension<AuthUser>,
    Path(refund_id): Path<String>,
    Json(body): Json<UpdateRefundStatusBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if user.role != "admin" {
        return Err(StatusCode::FORBIDDEN);
    }

    match body.status.as_str() {
        "approved" | "rejected" | "completed" => {}
        _ => return Err(StatusCode::BAD_REQUEST),
    }

    let id: Uuid = refund_id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;

    let result = sqlx::query(
        "UPDATE refund_requests SET status = $1, admin_note = $2, updated_at = NOW() WHERE id = $3",
    )
    .bind(&body.status)
    .bind(&body.admin_note)
    .bind(id)
    .execute(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if result.rows_affected() == 0 {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(Json(serde_json::json!({
        "refund_id": refund_id,
        "status": body.status,
    })))
}

// ─────────────────────────────────────────────────────────────
// Admin: User Sessions
// ─────────────────────────────────────────────────────────────

pub async fn admin_get_user_sessions(
    State(state): State<SharedState>,
    Extension(user): Extension<AuthUser>,
    Path(target_user_id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if user.role != "admin" {
        return Err(StatusCode::FORBIDDEN);
    }

    let target_id: Uuid = target_user_id
        .parse()
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    let rows = sqlx::query(
        r#"
        SELECT s.id::text AS session_id, s.status::text AS status,
               s.sell_token_symbol, s.target_token_symbol, s.chain::text AS chain,
               s.total_amount::text AS total_amount, s.amount_sold::text AS amount_sold,
               s.pov_percent::float8 AS pov_percent, s.created_at, s.updated_at,
               s.sell_token_decimals, s.target_token_decimals,
               s.strategy, s.public_slug,
               w.address AS wallet_address
        FROM sessions s
        JOIN wallets w ON s.wallet_id = w.id
        WHERE w.user_id = $1
        ORDER BY s.created_at DESC
        "#,
    )
    .bind(target_id)
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let sessions: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let created_at: chrono::DateTime<chrono::Utc> = r.get("created_at");
            let updated_at: chrono::DateTime<chrono::Utc> = r.get("updated_at");
            serde_json::json!({
                "session_id": r.get::<String, _>("session_id"),
                "status": r.get::<String, _>("status"),
                "sell_token_symbol": r.get::<String, _>("sell_token_symbol"),
                "target_token_symbol": r.get::<String, _>("target_token_symbol"),
                "sell_token_decimals": r.get::<i32, _>("sell_token_decimals"),
                "target_token_decimals": r.get::<i32, _>("target_token_decimals"),
                "chain": r.get::<String, _>("chain"),
                "total_amount": r.get::<String, _>("total_amount"),
                "amount_sold": r.get::<String, _>("amount_sold"),
                "pov_percent": r.get::<f64, _>("pov_percent"),
                "strategy": r.get::<String, _>("strategy"),
                "wallet_address": r.get::<String, _>("wallet_address"),
                "public_slug": r.get::<Option<String>, _>("public_slug"),
                "created_at": created_at.to_rfc3339(),
                "updated_at": updated_at.to_rfc3339(),
            })
        })
        .collect();

    Ok(Json(serde_json::json!({ "sessions": sessions })))
}

// ─────────────────────────────────────────────────────────────
// Common User: Sessions on their wallets
// ─────────────────────────────────────────────────────────────

pub async fn list_my_wallet_sessions(
    State(state): State<SharedState>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // Find all sessions that use wallets owned by this user
    let rows = sqlx::query(
        r#"
        SELECT s.id::text AS session_id, s.status::text AS status,
               s.sell_token_symbol, s.target_token_symbol, s.chain::text AS chain,
               s.total_amount::text AS total_amount, s.amount_sold::text AS amount_sold,
               s.pov_percent::float8 AS pov_percent, s.created_at, s.public_slug,
               w.address AS wallet_address
        FROM sessions s
        JOIN wallets w ON s.wallet_id = w.id
        WHERE w.user_id = $1
        ORDER BY s.created_at DESC
        "#,
    )
    .bind(user.user_id)
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let sessions: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let created_at: chrono::DateTime<chrono::Utc> = r.get("created_at");
            serde_json::json!({
                "session_id": r.get::<String, _>("session_id"),
                "status": r.get::<String, _>("status"),
                "sell_token_symbol": r.get::<String, _>("sell_token_symbol"),
                "target_token_symbol": r.get::<String, _>("target_token_symbol"),
                "chain": r.get::<String, _>("chain"),
                "total_amount": r.get::<String, _>("total_amount"),
                "amount_sold": r.get::<String, _>("amount_sold"),
                "pov_percent": r.get::<f64, _>("pov_percent"),
                "wallet_address": r.get::<String, _>("wallet_address"),
                "public_slug": r.get::<Option<String>, _>("public_slug"),
                "created_at": created_at.to_rfc3339(),
            })
        })
        .collect();

    Ok(Json(serde_json::json!({ "sessions": sessions })))
}

// ─────────────────────────────────────────────────────────────
// Admin: Update User Role
// ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct UpdateRoleRequest {
    pub role: String,
}

pub async fn admin_update_user_role(
    State(state): State<SharedState>,
    Extension(user): Extension<AuthUser>,
    Path(target_user_id): Path<String>,
    Json(body): Json<UpdateRoleRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if user.role != "admin" {
        return Err(StatusCode::FORBIDDEN);
    }

    let new_role = match body.role.as_str() {
        "admin" | "common" => body.role.as_str(),
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    let target_id: Uuid = target_user_id
        .parse()
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    // Prevent admin from changing their own role
    if target_id == user.user_id {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Both upgrade and downgrade: reset TOTP.
    // Upgrading to admin: totp_enabled = false triggers 2FA setup on next login.
    // Downgrading to common: clears any stale TOTP data.
    sqlx::query(
        "UPDATE users SET role = $1::user_role, totp_enabled = false, totp_secret = NULL WHERE id = $2",
    )
    .bind(new_role)
    .bind(target_id)
    .execute(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    info!(admin = %user.user_id, target = %target_id, new_role = %new_role, "User role updated");

    Ok(Json(serde_json::json!({
        "user_id": target_user_id,
        "role": new_role,
    })))
}

// ─────────────────────────────────────────────────────────────
// Admin: List All Wallets (with owner info)
// ─────────────────────────────────────────────────────────────

pub async fn admin_list_all_wallets(
    State(state): State<SharedState>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if user.role != "admin" {
        return Err(StatusCode::FORBIDDEN);
    }

    let rows = sqlx::query(
        r#"
        SELECT w.id::text AS wallet_id, w.address, w.chain::text AS chain, w.created_at,
               u.id::text AS owner_id, COALESCE(u.username, u.email) AS owner_name
        FROM wallets w
        JOIN users u ON u.id = w.user_id
        ORDER BY u.username, w.created_at DESC
        "#,
    )
    .fetch_all(&state.db)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let wallets: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let created_at: chrono::DateTime<chrono::Utc> = r.get("created_at");
            serde_json::json!({
                "wallet_id": r.get::<String, _>("wallet_id"),
                "address": r.get::<String, _>("address"),
                "chain": r.get::<String, _>("chain"),
                "created_at": created_at.to_rfc3339(),
                "owner_id": r.get::<String, _>("owner_id"),
                "owner_name": r.get::<String, _>("owner_name"),
            })
        })
        .collect();

    Ok(Json(serde_json::json!({ "wallets": wallets })))
}
