use crate::{AppError, AppState, auth::Claims, proto};
use axum::{
    extract::{Path, State},
    response::Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

// ─── DTOs ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateSessionRequest {
    pub wallet_id:             String,
    pub chain_id:              i64,
    pub token_address:         String,
    pub target_token_address:  String,
    /// Raw token amount as decimal string (U256)
    pub total_amount:          String,
    pub strategy:              String,
    pub pov_percentage:        Option<f64>,
    pub max_price_impact_bps:  Option<i32>,
    pub min_buy_trigger_usd:   Option<f64>,
}

#[derive(Serialize)]
pub struct SessionResponse {
    pub id:                   String,
    pub wallet_id:            String,
    pub chain_id:             i64,
    pub token_address:        String,
    pub target_token_address: String,
    pub total_amount:         String,
    pub amount_sold:          String,
    pub strategy:             String,
    pub pov_percentage:       Option<f64>,
    pub max_price_impact_bps: Option<i32>,
    pub min_buy_trigger_usd:  String,
    pub status:               String,
    pub public_slug:          String,
    pub created_at:           String,
}

// ─── Handlers ──────────────────────────────────────────────────────────────

/// POST /api/sessions
pub async fn create_session(
    State(state): State<Arc<AppState>>,
    axum::Extension(claims): axum::Extension<Claims>,
    Json(body): Json<CreateSessionRequest>,
) -> Result<Json<SessionResponse>, AppError> {
    let user_id: Uuid = claims.sub.parse()
        .map_err(|_| AppError::Unauthorized("Invalid token".into()))?;
    let wallet_id: Uuid = body.wallet_id.parse()
        .map_err(|_| AppError::BadRequest("Invalid wallet_id".into()))?;

    // Ensure the wallet belongs to this user
    let wallet_exists = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM wallets WHERE id = $1 AND user_id = $2",
        wallet_id,
        user_id
    )
    .fetch_one(&state.db)
    .await?
    .unwrap_or(0) > 0;

    if !wallet_exists {
        return Err(AppError::NotFound("Wallet not found".into()));
    }

    let total_amount: bigdecimal::BigDecimal = body.total_amount.parse()
        .map_err(|_| AppError::BadRequest("Invalid total_amount".into()))?;

    let min_buy_usd = bigdecimal::BigDecimal::from(
        body.min_buy_trigger_usd.unwrap_or(100.0) as i64
    );

    let session_id = Uuid::new_v4();
    let row = sqlx::query!(
        r#"
        INSERT INTO sessions (
            id, user_id, wallet_id, chain_id, token_address, target_token_address,
            total_amount, strategy, pov_percentage, max_price_impact_bps,
            min_buy_trigger_usd
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8::execution_strategy, $9, $10, $11
        )
        RETURNING id, public_slug, amount_sold, status, created_at, min_buy_trigger_usd
        "#,
        session_id,
        user_id,
        wallet_id,
        body.chain_id,
        body.token_address.to_lowercase(),
        body.target_token_address.to_lowercase(),
        total_amount,
        body.strategy as _,
        body.pov_percentage.map(|v| bigdecimal::BigDecimal::try_from(v).ok()).flatten(),
        body.max_price_impact_bps,
        min_buy_usd,
    )
    .fetch_one(&state.db)
    .await?;

    Ok(Json(SessionResponse {
        id:                   session_id.to_string(),
        wallet_id:            wallet_id.to_string(),
        chain_id:             body.chain_id,
        token_address:        body.token_address,
        target_token_address: body.target_token_address,
        total_amount:         body.total_amount,
        amount_sold:          row.amount_sold.to_string(),
        strategy:             body.strategy,
        pov_percentage:       body.pov_percentage,
        max_price_impact_bps: body.max_price_impact_bps,
        min_buy_trigger_usd:  row.min_buy_trigger_usd.to_string(),
        status:               row.status.to_string(),
        public_slug:          row.public_slug,
        created_at:           row.created_at.to_rfc3339(),
    }))
}

/// GET /api/sessions
pub async fn list_sessions(
    State(state): State<Arc<AppState>>,
    axum::Extension(claims): axum::Extension<Claims>,
) -> Result<Json<Vec<SessionResponse>>, AppError> {
    let user_id: Uuid = claims.sub.parse()
        .map_err(|_| AppError::Unauthorized("Invalid token".into()))?;

    let rows = sqlx::query!(
        r#"
        SELECT s.id, s.wallet_id, s.chain_id, s.token_address, s.target_token_address,
               s.total_amount, s.amount_sold, s.strategy as "strategy: String",
               s.pov_percentage, s.max_price_impact_bps, s.min_buy_trigger_usd,
               s.status as "status: String", s.public_slug, s.created_at
        FROM sessions s
        WHERE s.user_id = $1
        ORDER BY s.created_at DESC
        "#,
        user_id
    )
    .fetch_all(&state.db)
    .await?;

    let sessions = rows
        .into_iter()
        .map(|r| SessionResponse {
            id:                   r.id.to_string(),
            wallet_id:            r.wallet_id.to_string(),
            chain_id:             r.chain_id,
            token_address:        r.token_address,
            target_token_address: r.target_token_address,
            total_amount:         r.total_amount.to_string(),
            amount_sold:          r.amount_sold.to_string(),
            strategy:             r.strategy,
            pov_percentage:       r.pov_percentage.and_then(|v| v.to_string().parse().ok()),
            max_price_impact_bps: r.max_price_impact_bps,
            min_buy_trigger_usd:  r.min_buy_trigger_usd.to_string(),
            status:               r.status,
            public_slug:          r.public_slug,
            created_at:           r.created_at.to_rfc3339(),
        })
        .collect();

    Ok(Json(sessions))
}

/// GET /api/sessions/:id
pub async fn get_session(
    State(state): State<Arc<AppState>>,
    axum::Extension(claims): axum::Extension<Claims>,
    Path(id): Path<Uuid>,
) -> Result<Json<SessionResponse>, AppError> {
    let user_id: Uuid = claims.sub.parse()
        .map_err(|_| AppError::Unauthorized("Invalid token".into()))?;

    let r = sqlx::query!(
        r#"
        SELECT s.id, s.wallet_id, s.chain_id, s.token_address, s.target_token_address,
               s.total_amount, s.amount_sold, s.strategy as "strategy: String",
               s.pov_percentage, s.max_price_impact_bps, s.min_buy_trigger_usd,
               s.status as "status: String", s.public_slug, s.created_at
        FROM sessions s
        WHERE s.id = $1 AND s.user_id = $2
        "#,
        id,
        user_id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Session not found".into()))?;

    Ok(Json(SessionResponse {
        id:                   r.id.to_string(),
        wallet_id:            r.wallet_id.to_string(),
        chain_id:             r.chain_id,
        token_address:        r.token_address,
        target_token_address: r.target_token_address,
        total_amount:         r.total_amount.to_string(),
        amount_sold:          r.amount_sold.to_string(),
        strategy:             r.strategy,
        pov_percentage:       r.pov_percentage.and_then(|v| v.to_string().parse().ok()),
        max_price_impact_bps: r.max_price_impact_bps,
        min_buy_trigger_usd:  r.min_buy_trigger_usd.to_string(),
        status:               r.status,
        public_slug:          r.public_slug,
        created_at:           r.created_at.to_rfc3339(),
    }))
}

/// PUT /api/sessions/:id
pub async fn update_session(
    State(state): State<Arc<AppState>>,
    axum::Extension(claims): axum::Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    let user_id: Uuid = claims.sub.parse()
        .map_err(|_| AppError::Unauthorized("Invalid token".into()))?;

    // Only update fields that are provided; here we support pov_percentage and max_price_impact_bps
    if let Some(pov) = body.get("pov_percentage").and_then(|v| v.as_f64()) {
        sqlx::query!(
            "UPDATE sessions SET pov_percentage = $1 WHERE id = $2 AND user_id = $3",
            bigdecimal::BigDecimal::try_from(pov).ok(),
            id,
            user_id
        )
        .execute(&state.db)
        .await?;
    }

    Ok(Json(serde_json::json!({ "success": true })))
}

/// DELETE /api/sessions/:id
pub async fn delete_session(
    State(state): State<Arc<AppState>>,
    axum::Extension(claims): axum::Extension<Claims>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let user_id: Uuid = claims.sub.parse()
        .map_err(|_| AppError::Unauthorized("Invalid token".into()))?;

    let result = sqlx::query!(
        "DELETE FROM sessions WHERE id = $1 AND user_id = $2 AND status = 'pending'",
        id,
        user_id
    )
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Session not found or cannot be deleted in current state".into()));
    }

    Ok(Json(serde_json::json!({ "success": true })))
}

/// POST /api/sessions/:id/start
pub async fn start_session(
    State(state): State<Arc<AppState>>,
    axum::Extension(claims): axum::Extension<Claims>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let user_id: Uuid = claims.sub.parse()
        .map_err(|_| AppError::Unauthorized("Invalid token".into()))?;

    sqlx::query!(
        "UPDATE sessions SET status = 'active' WHERE id = $1 AND user_id = $2 AND status IN ('pending','paused')",
        id,
        user_id
    )
    .execute(&state.db)
    .await?;

    Ok(Json(serde_json::json!({ "success": true })))
}

/// POST /api/sessions/:id/pause
pub async fn pause_session(
    State(state): State<Arc<AppState>>,
    axum::Extension(claims): axum::Extension<Claims>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let user_id: Uuid = claims.sub.parse()
        .map_err(|_| AppError::Unauthorized("Invalid token".into()))?;

    sqlx::query!(
        "UPDATE sessions SET status = 'paused' WHERE id = $1 AND user_id = $2 AND status = 'active'",
        id,
        user_id
    )
    .execute(&state.db)
    .await?;

    Ok(Json(serde_json::json!({ "success": true })))
}

/// GET /api/public/:slug  — no authentication required
pub async fn public_session(
    State(state): State<Arc<AppState>>,
    Path(slug): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let r = sqlx::query!(
        r#"
        SELECT id, chain_id, token_address, target_token_address,
               total_amount, amount_sold, status as "status: String",
               created_at
        FROM sessions
        WHERE public_slug = $1
        "#,
        slug
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Session not found".into()))?;

    Ok(Json(serde_json::json!({
        "id":                   r.id,
        "chain_id":             r.chain_id,
        "token_address":        r.token_address,
        "target_token_address": r.target_token_address,
        "total_amount":         r.total_amount.to_string(),
        "amount_sold":          r.amount_sold.to_string(),
        "status":               r.status,
        "created_at":           r.created_at.to_rfc3339(),
    })))
}
