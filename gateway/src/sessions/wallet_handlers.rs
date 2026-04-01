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
pub struct CreateWalletRequest {
    pub label: Option<String>,
}

#[derive(Serialize)]
pub struct WalletResponse {
    pub id:         String,
    pub address:    String,
    pub label:      Option<String>,
    pub created_at: String,
}

// ─── Handlers ──────────────────────────────────────────────────────────────

/// POST /api/wallets
pub async fn create_wallet(
    State(state): State<Arc<AppState>>,
    axum::Extension(claims): axum::Extension<Claims>,
    Json(body): Json<CreateWalletRequest>,
) -> Result<Json<WalletResponse>, AppError> {
    let user_id: Uuid = claims.sub.parse()
        .map_err(|_| AppError::Unauthorized("Invalid token".into()))?;

    // Delegate wallet generation to the KMS
    let mut kms = state.kms_client.clone();
    let resp = kms
        .generate_wallet(proto::GenerateWalletRequest {
            user_id: user_id.to_string(),
            label:   body.label.clone().unwrap_or_default(),
        })
        .await?
        .into_inner();

    // Fetch full wallet record from DB (KMS persists it)
    let row = sqlx::query!(
        "SELECT id, address, label, created_at FROM wallets WHERE id = $1",
        Uuid::parse_str(&resp.wallet_id)
            .map_err(|_| AppError::Internal(anyhow::anyhow!("KMS returned invalid wallet ID")))?
    )
    .fetch_one(&state.db)
    .await?;

    Ok(Json(WalletResponse {
        id:         row.id.to_string(),
        address:    row.address,
        label:      row.label,
        created_at: row.created_at.to_rfc3339(),
    }))
}

/// GET /api/wallets
pub async fn list_wallets(
    State(state): State<Arc<AppState>>,
    axum::Extension(claims): axum::Extension<Claims>,
) -> Result<Json<Vec<WalletResponse>>, AppError> {
    let user_id: Uuid = claims.sub.parse()
        .map_err(|_| AppError::Unauthorized("Invalid token".into()))?;

    let rows = sqlx::query!(
        "SELECT id, address, label, created_at FROM wallets WHERE user_id = $1 ORDER BY created_at",
        user_id
    )
    .fetch_all(&state.db)
    .await?;

    Ok(Json(
        rows.into_iter()
            .map(|r| WalletResponse {
                id:         r.id.to_string(),
                address:    r.address,
                label:      r.label,
                created_at: r.created_at.to_rfc3339(),
            })
            .collect(),
    ))
}

/// GET /api/wallets/:id/balances
pub async fn get_balances(
    State(state): State<Arc<AppState>>,
    axum::Extension(claims): axum::Extension<Claims>,
    Path(wallet_id): Path<Uuid>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Vec<proto::TokenBalance>>, AppError> {
    let user_id: Uuid = claims.sub.parse()
        .map_err(|_| AppError::Unauthorized("Invalid token".into()))?;

    // Ensure wallet belongs to user
    let exists = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM wallets WHERE id = $1 AND user_id = $2",
        wallet_id,
        user_id
    )
    .fetch_one(&state.db)
    .await?
    .unwrap_or(0) > 0;

    if !exists {
        return Err(AppError::NotFound("Wallet not found".into()));
    }

    let chain_rpc_url = params
        .get("rpc_url")
        .cloned()
        .ok_or_else(|| AppError::BadRequest("rpc_url query param required".into()))?;

    let token_addresses: Vec<String> = params
        .get("tokens")
        .map(|t| t.split(',').map(str::to_string).collect())
        .unwrap_or_default();

    let mut kms = state.kms_client.clone();
    let resp = kms
        .get_balances(proto::GetBalancesRequest {
            wallet_id:       wallet_id.to_string(),
            chain_rpc_url,
            token_addresses,
        })
        .await?
        .into_inner();

    Ok(Json(resp.balances))
}
