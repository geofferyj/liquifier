use crate::{AppError, AppState, auth::{self, Claims}};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use argon2::password_hash::{SaltString, rand_core::OsRng};
use axum::{extract::State, response::Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

// ─── Request / Response DTOs ───────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SignupRequest {
    email:    String,
    password: String,
}

#[derive(Serialize)]
pub struct AuthResponse {
    token:        String,
    user_id:      String,
    totp_enabled: bool,
}

#[derive(Deserialize)]
pub struct LoginRequest {
    email:    String,
    password: String,
    totp_code: Option<String>,
}

#[derive(Deserialize)]
pub struct TotpVerifyRequest {
    code: String,
}

#[derive(Serialize)]
pub struct TotpSetupResponse {
    secret:   String,
    otpauth_url: String,
}

// ─── Handlers ──────────────────────────────────────────────────────────────

/// POST /api/auth/signup
pub async fn signup(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SignupRequest>,
) -> Result<Json<AuthResponse>, AppError> {
    // Validate
    if body.email.is_empty() || body.password.len() < 8 {
        return Err(AppError::BadRequest("Email required and password must be ≥8 chars".into()));
    }

    // Hash password with argon2id
    let salt         = SaltString::generate(&mut OsRng);
    let argon2       = Argon2::default();
    let password_hash = argon2
        .hash_password(body.password.as_bytes(), &salt)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("hash: {e}")))?
        .to_string();

    let user_id = Uuid::new_v4();
    sqlx::query!(
        r#"INSERT INTO users (id, email, password_hash) VALUES ($1, $2, $3)"#,
        user_id,
        body.email.to_lowercase(),
        password_hash,
    )
    .execute(&state.db)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(ref db_err) if db_err.constraint() == Some("users_email_key") => {
            AppError::Conflict("Email already registered".into())
        }
        other => AppError::Database(other),
    })?;

    let claims = Claims::new(user_id, false, 3600 * 24);
    let token  = auth::encode_jwt(&claims, &state.jwt_secret)?;

    Ok(Json(AuthResponse { token, user_id: user_id.to_string(), totp_enabled: false }))
}

/// POST /api/auth/login
pub async fn login(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LoginRequest>,
) -> Result<Json<AuthResponse>, AppError> {
    let row = sqlx::query!(
        r#"SELECT id, password_hash, totp_secret, totp_enabled FROM users WHERE email = $1"#,
        body.email.to_lowercase(),
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::Unauthorized("Invalid credentials".into()))?;

    // Verify password
    let parsed = PasswordHash::new(&row.password_hash)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("{e}")))?;
    Argon2::default()
        .verify_password(body.password.as_bytes(), &parsed)
        .map_err(|_| AppError::Unauthorized("Invalid credentials".into()))?;

    // Verify TOTP if enabled
    if row.totp_enabled {
        let secret = row.totp_secret
            .as_deref()
            .ok_or_else(|| AppError::Internal(anyhow::anyhow!("TOTP secret missing")))?;

        let code = body.totp_code
            .as_deref()
            .ok_or_else(|| AppError::Unauthorized("TOTP code required".into()))?;

        let totp = totp_rs::TOTP::new(
            totp_rs::Algorithm::SHA1,
            6,
            1,
            30,
            totp_rs::Secret::Raw(secret.as_bytes().to_vec()).to_bytes().unwrap(),
        )
        .map_err(|e| AppError::Internal(anyhow::anyhow!("TOTP: {e}")))?;

        if !totp.check_current(code).unwrap_or(false) {
            return Err(AppError::Unauthorized("Invalid TOTP code".into()));
        }
    }

    let claims = Claims::new(row.id, row.totp_enabled, 3600 * 24);
    let token  = auth::encode_jwt(&claims, &state.jwt_secret)?;

    Ok(Json(AuthResponse {
        token,
        user_id:      row.id.to_string(),
        totp_enabled: row.totp_enabled,
    }))
}

/// POST /api/auth/2fa/setup   (requires authenticated user)
pub async fn setup_2fa(
    State(state): State<Arc<AppState>>,
    axum::Extension(claims): axum::Extension<Claims>,
) -> Result<Json<TotpSetupResponse>, AppError> {
    let user_id: Uuid = claims.sub.parse()
        .map_err(|_| AppError::Unauthorized("Invalid user ID in token".into()))?;

    // Generate TOTP secret
    let secret = totp_rs::Secret::generate_secret();
    let secret_str = secret.to_encoded().to_string();

    // Fetch email for the otpauth URL
    let email = sqlx::query_scalar!("SELECT email FROM users WHERE id = $1", user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("User not found".into()))?;

    let totp = totp_rs::TOTP::new(
        totp_rs::Algorithm::SHA1,
        6,
        1,
        30,
        secret.to_bytes().unwrap(),
    )
    .map_err(|e| AppError::Internal(anyhow::anyhow!("TOTP: {e}")))?;

    let otpauth_url = totp.get_url("Liquifier", &email);

    // Persist (not yet enabled; user must verify before activation)
    sqlx::query!(
        "UPDATE users SET totp_secret = $1 WHERE id = $2",
        secret_str,
        user_id,
    )
    .execute(&state.db)
    .await?;

    Ok(Json(TotpSetupResponse { secret: secret_str, otpauth_url }))
}

/// POST /api/auth/2fa/verify  (requires authenticated user)
pub async fn verify_2fa(
    State(state): State<Arc<AppState>>,
    axum::Extension(claims): axum::Extension<Claims>,
    Json(body): Json<TotpVerifyRequest>,
) -> Result<Json<AuthResponse>, AppError> {
    let user_id: Uuid = claims.sub.parse()
        .map_err(|_| AppError::Unauthorized("Invalid user ID in token".into()))?;

    let row = sqlx::query!(
        "SELECT email, totp_secret FROM users WHERE id = $1",
        user_id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("User not found".into()))?;

    let secret_str = row.totp_secret
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("2FA not set up yet. Call /2fa/setup first".into()))?;

    let secret_bytes = totp_rs::Secret::Encoded(secret_str.to_string())
        .to_bytes()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("TOTP decode: {e}")))?;

    let totp = totp_rs::TOTP::new(totp_rs::Algorithm::SHA1, 6, 1, 30, secret_bytes)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("TOTP: {e}")))?;

    if !totp.check_current(&body.code).unwrap_or(false) {
        return Err(AppError::Unauthorized("Invalid TOTP code".into()));
    }

    // Enable 2FA
    sqlx::query!(
        "UPDATE users SET totp_enabled = TRUE WHERE id = $1",
        user_id
    )
    .execute(&state.db)
    .await?;

    let new_claims = Claims::new(user_id, true, 3600 * 24);
    let token      = auth::encode_jwt(&new_claims, &state.jwt_secret)?;

    Ok(Json(AuthResponse {
        token,
        user_id:      user_id.to_string(),
        totp_enabled: true,
    }))
}
