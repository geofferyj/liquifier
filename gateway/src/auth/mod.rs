use crate::{AppError, AppState};
use axum::{
    extract::State,
    response::Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

pub mod handlers;

// ─── JWT Claims ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub:           String,          // user UUID
    pub exp:           i64,             // unix timestamp
    pub totp_verified: bool,
}

impl Claims {
    pub fn new(user_id: Uuid, totp_verified: bool, ttl_secs: i64) -> Self {
        Self {
            sub:           user_id.to_string(),
            exp:           chrono::Utc::now().timestamp() + ttl_secs,
            totp_verified,
        }
    }
}

pub fn encode_jwt(claims: &Claims, secret: &str) -> Result<String, AppError> {
    jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        claims,
        &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| AppError::Internal(anyhow::anyhow!("JWT encode: {e}")))
}

pub fn decode_jwt(token: &str, secret: &str) -> Result<Claims, AppError> {
    let validation = jsonwebtoken::Validation::default();
    jsonwebtoken::decode::<Claims>(
        token,
        &jsonwebtoken::DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .map(|d| d.claims)
    .map_err(|_| AppError::Unauthorized("Invalid or expired token".into()))
}
