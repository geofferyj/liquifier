use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use anyhow::{Context, Result};
use common::proto::{
    wallet_kms_server::{WalletKms, WalletKmsServer},
    CreateWalletRequest, CreateWalletResponse, GetBalanceRequest, GetBalanceResponse,
    GetWalletRequest, ListWalletsRequest, ListWalletsResponse, SignTransactionRequest,
    SignTransactionResponse, WalletInfo,
};
use liquifier_config::Settings;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::net::SocketAddr;
use tonic::{transport::Server, Request, Response, Status};
use tracing::info;
use uuid::Uuid;

sol! {
    #[sol(rpc)]
    interface IERC20 {
        function balanceOf(address account) external view returns (uint256);
        function decimals() external view returns (uint8);
    }
}

mod crypto;

// ─────────────────────────────────────────────────────────────
// KMS Service State
// ─────────────────────────────────────────────────────────────

pub struct KmsService {
    db: PgPool,
    master_key: [u8; 32],
    settings: Settings,
}

impl KmsService {
    pub fn new(db: PgPool, master_key: [u8; 32], settings: Settings) -> Self {
        Self {
            db,
            master_key,
            settings,
        }
    }
}

// ─────────────────────────────────────────────────────────────
// gRPC Implementation
// ─────────────────────────────────────────────────────────────

#[tonic::async_trait]
impl WalletKms for KmsService {
    async fn create_wallet(
        &self,
        request: Request<CreateWalletRequest>,
    ) -> Result<Response<CreateWalletResponse>, Status> {
        let req = request.into_inner();
        let user_id: Uuid = req
            .user_id
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid user_id"))?;

        // All EVM chains share the same address type — store as "ethereum"
        let chain = "ethereum".to_string();

        // Generate a new random private key using alloy
        let signer = PrivateKeySigner::random();
        let address = format!("{:?}", signer.address());
        let private_key_bytes = signer.credential().to_bytes();

        // Encrypt the private key with AES-256-GCM
        let (ciphertext, nonce_bytes) =
            crypto::encrypt_key(&self.master_key, &private_key_bytes)
                .map_err(|e| Status::internal(format!("Encryption error: {e}")))?;

        let wallet_id = Uuid::new_v4();

        sqlx::query(
            r#"
            INSERT INTO wallets (id, user_id, chain, address, encrypted_key, nonce)
            VALUES ($1, $2, $3::chain_id, $4, $5, $6)
            "#,
        )
        .bind(wallet_id)
        .bind(user_id)
        .bind(&chain)
        .bind(&address)
        .bind(&ciphertext)
        .bind(&nonce_bytes)
        .execute(&self.db)
        .await
        .map_err(|e| Status::internal(format!("DB error: {e}")))?;

        info!(wallet_id = %wallet_id, address = %address, "Wallet created");

        Ok(Response::new(CreateWalletResponse {
            wallet_id: wallet_id.to_string(),
            address,
        }))
    }

    async fn get_wallet(
        &self,
        request: Request<GetWalletRequest>,
    ) -> Result<Response<WalletInfo>, Status> {
        let wallet_id: Uuid = request
            .into_inner()
            .wallet_id
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid wallet_id"))?;

        let row = sqlx::query_as::<_, (Uuid, String, String, chrono::DateTime<chrono::Utc>)>(
            "SELECT id, address, chain::text, created_at FROM wallets WHERE id = $1",
        )
        .bind(wallet_id)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| Status::internal(format!("DB error: {e}")))?
        .ok_or_else(|| Status::not_found("Wallet not found"))?;

        Ok(Response::new(WalletInfo {
            wallet_id: row.0.to_string(),
            address: row.1,
            chain: row.2,
            created_at: row.3.to_rfc3339(),
        }))
    }

    async fn list_wallets(
        &self,
        request: Request<ListWalletsRequest>,
    ) -> Result<Response<ListWalletsResponse>, Status> {
        let user_id: Uuid = request
            .into_inner()
            .user_id
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid user_id"))?;

        let rows = sqlx::query_as::<_, (Uuid, String, String, chrono::DateTime<chrono::Utc>)>(
            "SELECT id, address, chain::text, created_at FROM wallets WHERE user_id = $1 ORDER BY created_at",
        )
        .bind(user_id)
        .fetch_all(&self.db)
        .await
        .map_err(|e| Status::internal(format!("DB error: {e}")))?;

        let wallets = rows
            .into_iter()
            .map(|r| WalletInfo {
                wallet_id: r.0.to_string(),
                address: r.1,
                chain: r.2,
                created_at: r.3.to_rfc3339(),
            })
            .collect();

        Ok(Response::new(ListWalletsResponse { wallets }))
    }

    async fn get_balance(
        &self,
        request: Request<GetBalanceRequest>,
    ) -> Result<Response<GetBalanceResponse>, Status> {
        let req = request.into_inner();
        let wallet_id: Uuid = req
            .wallet_id
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid wallet_id"))?;

        // Look up the wallet address and chain
        let (address_str, chain_str): (String, String) =
            sqlx::query_as("SELECT address, chain::text FROM wallets WHERE id = $1")
                .bind(wallet_id)
                .fetch_optional(&self.db)
                .await
                .map_err(|e| Status::internal(format!("DB error: {e}")))?
                .ok_or_else(|| Status::not_found("Wallet not found"))?;

        // Determine RPC URL: use request override, or look up from chain config
        let rpc_url = if !req.rpc_url.is_empty() {
            req.rpc_url
        } else {
            self.settings
                .chains
                .get(&chain_str)
                .map(|c| c.rpc_url.clone())
                .ok_or_else(|| {
                    Status::failed_precondition(format!(
                        "No RPC URL configured for chain {chain_str}"
                    ))
                })?
        };

        let provider = ProviderBuilder::new().connect_http(
            rpc_url
                .parse()
                .map_err(|_| Status::internal("Invalid RPC URL"))?,
        );

        let address: Address = address_str
            .parse()
            .map_err(|_| Status::internal("Invalid wallet address in DB"))?;

        if req.token_address.is_empty() {
            // Native balance
            let balance: U256 = provider
                .get_balance(address)
                .await
                .map_err(|e| Status::internal(format!("RPC error: {e}")))?;
            Ok(Response::new(GetBalanceResponse {
                balance: balance.to_string(),
                decimals: 18,
            }))
        } else {
            // ERC20 balance
            let token: Address = req
                .token_address
                .parse()
                .map_err(|_| Status::invalid_argument("Invalid token_address"))?;
            let contract = IERC20::new(token, &provider);
            let decimals: u8 = contract
                .decimals()
                .call()
                .await
                .map_err(|e| Status::internal(format!("decimals() call failed: {e}")))?;
            let balance: U256 = contract
                .balanceOf(address)
                .call()
                .await
                .map_err(|e| Status::internal(format!("balanceOf() call failed: {e}")))?;
            Ok(Response::new(GetBalanceResponse {
                balance: balance.to_string(),
                decimals: decimals as u32,
            }))
        }
    }

    async fn sign_transaction(
        &self,
        request: Request<SignTransactionRequest>,
    ) -> Result<Response<SignTransactionResponse>, Status> {
        let req = request.into_inner();
        let wallet_id: Uuid = req
            .wallet_id
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid wallet_id"))?;

        // Fetch encrypted key from DB
        let row = sqlx::query_as::<_, (Vec<u8>, Vec<u8>)>(
            "SELECT encrypted_key, nonce FROM wallets WHERE id = $1",
        )
        .bind(wallet_id)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| Status::internal(format!("DB error: {e}")))?
        .ok_or_else(|| Status::not_found("Wallet not found"))?;

        // Decrypt the private key IN MEMORY ONLY
        let private_key_bytes = crypto::decrypt_key(&self.master_key, &row.0, &row.1)
            .map_err(|e| Status::internal(format!("Decryption error: {e}")))?;

        // Reconstruct the signer
        let signer = PrivateKeySigner::from_slice(&private_key_bytes)
            .map_err(|e| Status::internal(format!("Key reconstruction error: {e}")))?;

        // Sign the raw transaction bytes
        use alloy::signers::Signer;
        let signature = signer
            .sign_hash(&alloy::primitives::B256::from_slice(&req.unsigned_tx))
            .await
            .map_err(|e| Status::internal(format!("Signing error: {e}")))?;

        let sig_bytes = {
            let mut buf = Vec::with_capacity(65);
            buf.extend_from_slice(&signature.r().to_be_bytes::<32>());
            buf.extend_from_slice(&signature.s().to_be_bytes::<32>());
            buf.push(signature.v() as u8);
            buf
        };

        info!(wallet_id = %wallet_id, "Transaction signed");

        Ok(Response::new(SignTransactionResponse {
            signed_tx: sig_bytes,
            tx_hash: String::new(), // caller computes final tx hash after assembly
        }))
    }

    async fn export_private_key(
        &self,
        request: Request<common::proto::ExportPrivateKeyRequest>,
    ) -> Result<Response<common::proto::ExportPrivateKeyResponse>, Status> {
        let req = request.into_inner();
        let wallet_id: Uuid = req
            .wallet_id
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid wallet_id"))?;
        let user_id: Uuid = req
            .user_id
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid user_id"))?;

        // Fetch encrypted key — verify ownership via user_id
        let row = sqlx::query_as::<_, (Vec<u8>, Vec<u8>, String)>(
            "SELECT encrypted_key, nonce, address FROM wallets WHERE id = $1 AND user_id = $2",
        )
        .bind(wallet_id)
        .bind(user_id)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| Status::internal(format!("DB error: {e}")))?
        .ok_or_else(|| Status::not_found("Wallet not found or not owned by user"))?;

        let private_key_bytes = crypto::decrypt_key(&self.master_key, &row.0, &row.1)
            .map_err(|e| Status::internal(format!("Decryption error: {e}")))?;

        let hex_key = format!("0x{}", hex::encode(&private_key_bytes));

        info!(wallet_id = %wallet_id, "Private key exported");

        Ok(Response::new(common::proto::ExportPrivateKeyResponse {
            private_key: hex_key,
            address: row.2,
        }))
    }
}

// ─────────────────────────────────────────────────────────────
// Main entrypoint
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
    let master_key_hex = &cfg.kms.master_encryption_key;
    let listen_addr: SocketAddr = SocketAddr::from(([0, 0, 0, 0], cfg.kms.grpc_port));

    // Parse 32-byte master key from hex
    let master_key_bytes =
        hex::decode(&master_key_hex).context("MASTER_ENCRYPTION_KEY must be valid hex")?;
    if master_key_bytes.len() != 32 {
        anyhow::bail!("MASTER_ENCRYPTION_KEY must be exactly 32 bytes (64 hex chars)");
    }
    let mut master_key = [0u8; 32];
    master_key.copy_from_slice(&master_key_bytes);

    let db = PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await
        .context("Failed to connect to PostgreSQL")?;

    info!(%listen_addr, "KMS gRPC server starting");

    let service = KmsService::new(db, master_key, cfg.clone());

    Server::builder()
        .add_service(WalletKmsServer::new(service))
        .serve(listen_addr)
        .await
        .context("gRPC server failed")?;

    Ok(())
}
