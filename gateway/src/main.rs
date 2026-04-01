use anyhow::Context;
use axum::{
    extract::State,
    http::StatusCode,
    middleware,
    response::{IntoResponse, Json},
    routing::{get, post, put, delete},
    Router,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::info;

mod auth;
mod error;
mod middleware as mw;
mod sessions;

pub use error::AppError;

// ─── Generated gRPC clients ────────────────────────────────────────────────
pub mod proto {
    tonic::include_proto!("liquifier");
}

// ─── Application State ─────────────────────────────────────────────────────

#[derive(Clone)]
pub struct AppState {
    pub db:          PgPool,
    pub redis:       redis::aio::ConnectionManager,
    pub jwt_secret:  String,
    pub kms_client:  proto::wallet_kms_client::WalletKmsClient<tonic::transport::Channel>,
    pub sess_client: proto::session_service_client::SessionServiceClient<tonic::transport::Channel>,
}

// ─── Main ──────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let database_url  = std::env::var("DATABASE_URL").context("DATABASE_URL not set")?;
    let redis_url     = std::env::var("REDIS_URL").context("REDIS_URL not set")?;
    let jwt_secret    = std::env::var("JWT_SECRET").context("JWT_SECRET not set")?;
    let kms_addr      = std::env::var("KMS_GRPC_ADDR").context("KMS_GRPC_ADDR not set")?;
    let sessions_addr = std::env::var("SESSIONS_GRPC_ADDR").context("SESSIONS_GRPC_ADDR not set")?;
    let bind_addr     = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());

    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(20)
        .connect(&database_url)
        .await
        .context("Cannot connect to PostgreSQL")?;

    let redis_client = redis::Client::open(redis_url)?;
    let redis = redis::aio::ConnectionManager::new(redis_client).await?;

    let kms_client = proto::wallet_kms_client::WalletKmsClient::connect(kms_addr)
        .await
        .context("Cannot connect to KMS gRPC")?;

    let sess_client = proto::session_service_client::SessionServiceClient::connect(sessions_addr)
        .await
        .context("Cannot connect to Sessions gRPC")?;

    let state = Arc::new(AppState {
        db,
        redis,
        jwt_secret,
        kms_client,
        sess_client,
    });

    let app = Router::new()
        // Auth
        .route("/api/auth/signup",         post(auth::handlers::signup))
        .route("/api/auth/login",          post(auth::handlers::login))
        .route("/api/auth/2fa/setup",      post(auth::handlers::setup_2fa))
        .route("/api/auth/2fa/verify",     post(auth::handlers::verify_2fa))
        // Wallets
        .route("/api/wallets",             post(sessions::wallet_handlers::create_wallet))
        .route("/api/wallets",             get(sessions::wallet_handlers::list_wallets))
        .route("/api/wallets/:id/balances",get(sessions::wallet_handlers::get_balances))
        // Sessions
        .route("/api/sessions",            post(sessions::session_handlers::create_session))
        .route("/api/sessions",            get(sessions::session_handlers::list_sessions))
        .route("/api/sessions/:id",        get(sessions::session_handlers::get_session))
        .route("/api/sessions/:id",        put(sessions::session_handlers::update_session))
        .route("/api/sessions/:id",        delete(sessions::session_handlers::delete_session))
        .route("/api/sessions/:id/start",  post(sessions::session_handlers::start_session))
        .route("/api/sessions/:id/pause",  post(sessions::session_handlers::pause_session))
        // Public share link (no auth required)
        .route("/api/public/:slug",        get(sessions::session_handlers::public_session))
        .layer(middleware::from_fn_with_state(state.clone(), mw::auth::authenticate))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    info!("Gateway listening on {bind_addr}");
    axum::serve(listener, app).await?;

    Ok(())
}
