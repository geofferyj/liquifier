//! Wallet & Key Management Service (KMS)
//!
//! SECURITY: This service is air-gapped from the public internet.
//! Private keys are encrypted at rest with AES-256-GCM and only
//! decrypted transiently in memory during transaction signing.

use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    Aes256Gcm, Key, Nonce,
};
use alloy::{
    network::{EthereumWallet, TransactionBuilder},
    primitives::{Address, Bytes, U256},
    rpc::types::TransactionRequest,
    signers::local::PrivateKeySigner,
};
use anyhow::Context;
use base64::Engine as _;
use sqlx::PgPool;
use std::sync::Arc;
use tonic::{transport::Server, Request, Response, Status};
use tracing::info;
use uuid::Uuid;

// ─── Generated gRPC types ──────────────────────────────────────────────────
pub mod proto {
    tonic::include_proto!("liquifier");
}

use proto::wallet_kms_server::{WalletKms, WalletKmsServer};
use proto::*;

// ─── Cipher helpers ────────────────────────────────────────────────────────

/// Encrypts a private key (32 bytes) with AES-256-GCM.
/// Returns (ciphertext, nonce) both as raw byte vectors.
fn encrypt_key(plaintext: &[u8], master_key: &[u8; 32]) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let key    = Key::<Aes256Gcm>::from_slice(master_key);
    let cipher = Aes256Gcm::new(key);
    let nonce  = Aes256Gcm::generate_nonce(&mut OsRng);

    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|e| anyhow::anyhow!("AES-GCM encrypt: {e}"))?;

    Ok((ciphertext, nonce.to_vec()))
}

/// Decrypts a private key previously encrypted by [`encrypt_key`].
fn decrypt_key(
    ciphertext: &[u8],
    nonce_bytes: &[u8],
    master_key: &[u8; 32],
) -> anyhow::Result<Vec<u8>> {
    let key    = Key::<Aes256Gcm>::from_slice(master_key);
    let cipher = Aes256Gcm::new(key);
    let nonce  = Nonce::from_slice(nonce_bytes);

    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow::anyhow!("AES-GCM decrypt: {e}"))
}

// ─── Service state ─────────────────────────────────────────────────────────

struct KmsService {
    db:         PgPool,
    master_key: [u8; 32],
    rpc_url:    String,
}

// ─── gRPC implementation ───────────────────────────────────────────────────

#[tonic::async_trait]
impl WalletKms for KmsService {
    /// Generate a new EVM wallet, encrypt the private key, and persist.
    async fn generate_wallet(
        &self,
        req: Request<GenerateWalletRequest>,
    ) -> Result<Response<GenerateWalletResponse>, Status> {
        let body = req.into_inner();
        let user_id: Uuid = body
            .user_id
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid user_id"))?;

        // Generate a fresh random wallet
        let signer = PrivateKeySigner::random();
        let address = signer.address();
        let private_key_bytes = signer.credential().to_bytes();

        // Encrypt at rest
        let (encrypted_key, nonce) = encrypt_key(&private_key_bytes, &self.master_key)
            .map_err(|e| Status::internal(e.to_string()))?;

        let wallet_id = Uuid::new_v4();
        sqlx::query!(
            r#"
            INSERT INTO wallets (id, user_id, address, encrypted_key, key_nonce, label)
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
            wallet_id,
            user_id,
            format!("{address:#x}"),
            encrypted_key,
            nonce,
            if body.label.is_empty() { None } else { Some(body.label) },
        )
        .execute(&self.db)
        .await
        .map_err(|e| Status::internal(e.to_string()))?;

        info!(%wallet_id, %address, "Generated new wallet");

        Ok(Response::new(GenerateWalletResponse {
            wallet_id: wallet_id.to_string(),
            address:   format!("{address:#x}"),
        }))
    }

    /// Return the public address for a wallet.
    async fn get_address(
        &self,
        req: Request<GetAddressRequest>,
    ) -> Result<Response<GetAddressResponse>, Status> {
        let wallet_id: Uuid = req
            .into_inner()
            .wallet_id
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid wallet_id"))?;

        let address = sqlx::query_scalar!("SELECT address FROM wallets WHERE id = $1", wallet_id)
            .fetch_optional(&self.db)
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .ok_or_else(|| Status::not_found("Wallet not found"))?;

        Ok(Response::new(GetAddressResponse { address }))
    }

    /// Return native + ERC-20 balances.
    async fn get_balances(
        &self,
        req: Request<GetBalancesRequest>,
    ) -> Result<Response<GetBalancesResponse>, Status> {
        let body = req.into_inner();
        let wallet_id: Uuid = body
            .wallet_id
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid wallet_id"))?;

        let address_str =
            sqlx::query_scalar!("SELECT address FROM wallets WHERE id = $1", wallet_id)
                .fetch_optional(&self.db)
                .await
                .map_err(|e| Status::internal(e.to_string()))?
                .ok_or_else(|| Status::not_found("Wallet not found"))?;

        let address: Address = address_str
            .parse()
            .map_err(|_| Status::internal("Stored address is invalid"))?;

        let provider = alloy::providers::ProviderBuilder::new()
            .on_ws(alloy::providers::WsConnect::new(&body.chain_rpc_url))
            .await
            .map_err(|e| Status::unavailable(format!("RPC connect: {e}")))?;

        let native_balance = provider
            .get_balance(address)
            .await
            .map_err(|e| Status::unavailable(format!("get_balance: {e}")))?;

        let mut balances = vec![TokenBalance {
            token_address: "native".into(),
            balance_raw:   native_balance.to_string(),
            decimals:      "18".into(),
            symbol:        "ETH".into(),
        }];

        // ERC-20 balances via balanceOf(address) → bytes4 selector 0x70a08231
        for token_address in &body.token_addresses {
            let token: Address = token_address
                .parse()
                .map_err(|_| Status::invalid_argument("Invalid token address"))?;

            // Encode balanceOf(address) call
            let mut calldata = hex::decode("70a08231").expect("valid balanceOf selector");
            calldata.extend_from_slice(&[0u8; 12]); // 12-byte padding
            calldata.extend_from_slice(address.as_slice());

            let result = provider
                .call(
                    &TransactionRequest::default()
                        .to(token)
                        .input(Bytes::from(calldata).into()),
                )
                .await
                .unwrap_or_default();

            let balance = if result.len() >= 32 {
                U256::from_be_slice(&result[..32]).to_string()
            } else {
                "0".into()
            };

            balances.push(TokenBalance {
                token_address: token_address.clone(),
                balance_raw:   balance,
                decimals:      "18".into(), // caller should look this up separately
                symbol:        "ERC20".into(),
            });
        }

        Ok(Response::new(GetBalancesResponse { balances }))
    }

    /// Sign an EIP-1559 transaction.  The private key is decrypted only
    /// in this function scope and is never written to disk or network.
    async fn sign_transaction(
        &self,
        req: Request<SignTransactionRequest>,
    ) -> Result<Response<SignTransactionResponse>, Status> {
        let body = req.into_inner();
        let wallet_id: Uuid = body
            .wallet_id
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid wallet_id"))?;

        // Fetch encrypted key material
        let row = sqlx::query!(
            "SELECT encrypted_key, key_nonce FROM wallets WHERE id = $1",
            wallet_id
        )
        .fetch_optional(&self.db)
        .await
        .map_err(|e| Status::internal(e.to_string()))?
        .ok_or_else(|| Status::not_found("Wallet not found"))?;

        // Decrypt private key in-memory
        let private_key_bytes = decrypt_key(&row.encrypted_key, &row.key_nonce, &self.master_key)
            .map_err(|e| Status::internal(format!("Key decryption failed: {e}")))?;

        let signer = PrivateKeySigner::from_slice(&private_key_bytes)
            .map_err(|e| Status::internal(format!("Signer creation: {e}")))?;
        let wallet = EthereumWallet::from(signer);

        // Parse transaction fields
        let value: U256 = body
            .value
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid value"))?;
        let gas_limit: u64 = body
            .gas_limit
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid gas_limit"))?;
        let max_fee: u128 = body
            .max_fee_per_gas
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid max_fee_per_gas"))?;
        let priority_fee: u128 = body
            .max_priority_fee_per_gas
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid max_priority_fee_per_gas"))?;
        let nonce: u64 = body
            .nonce
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid nonce"))?;
        let to: Address = body
            .to
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid to address"))?;

        let tx = TransactionRequest::default()
            .with_chain_id(body.chain_id)
            .with_to(to)
            .with_value(value)
            .with_input(body.data)
            .with_gas_limit(gas_limit)
            .with_max_fee_per_gas(max_fee)
            .with_max_priority_fee_per_gas(priority_fee)
            .with_nonce(nonce);

        let signed = tx
            .build(&wallet)
            .await
            .map_err(|e| Status::internal(format!("Build/sign tx: {e}")))?;

        let raw_tx  = signed.encoded_2718();
        let tx_hash = format!("{:#x}", signed.tx_hash());

        info!(%tx_hash, %wallet_id, "Signed transaction");

        Ok(Response::new(SignTransactionResponse {
            raw_tx:  raw_tx.into(),
            tx_hash,
        }))
    }
}

// ─── Main ──────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL not set")?;
    let master_key_b64 =
        std::env::var("MASTER_ENCRYPTION_KEY").context("MASTER_ENCRYPTION_KEY not set")?;
    let rpc_url  = std::env::var("EVM_RPC_URL").context("EVM_RPC_URL not set")?;
    let bind_addr: std::net::SocketAddr = std::env::var("BIND_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:50051".into())
        .parse()
        .context("Invalid BIND_ADDR")?;

    // Decode 32-byte master key from base64
    let key_bytes = base64::engine::general_purpose::STANDARD
        .decode(&master_key_b64)
        .context("MASTER_ENCRYPTION_KEY is not valid base64")?;
    if key_bytes.len() != 32 {
        anyhow::bail!("MASTER_ENCRYPTION_KEY must decode to exactly 32 bytes");
    }
    let mut master_key = [0u8; 32];
    master_key.copy_from_slice(&key_bytes);

    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await
        .context("Cannot connect to PostgreSQL")?;

    let service = KmsService { db, master_key, rpc_url };

    info!("KMS gRPC server listening on {bind_addr}");
    Server::builder()
        .add_service(WalletKmsServer::new(service))
        .serve(bind_addr)
        .await
        .context("gRPC server error")?;

    Ok(())
}
