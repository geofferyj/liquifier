use anyhow::{Context, Result};
use axum::{
    middleware,
    routing::{get, post, put},
    Router,
};
use sqlx::postgres::PgPoolOptions;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::info;

mod auth;
mod jwt_middleware;
mod routes;

// ─────────────────────────────────────────────────────────────
// Application State
// ─────────────────────────────────────────────────────────────

pub struct AppState {
    pub db: sqlx::PgPool,
    pub redis: redis::aio::ConnectionManager,
    pub jwt_secret: String,
    pub kms_addr: String,
    pub session_addr: String,
    pub ws_addr: String,
}

pub type SharedState = Arc<AppState>;

// ─────────────────────────────────────────────────────────────
// Main
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
    let redis_url = &cfg.redis.url;
    let jwt_secret = cfg.auth.jwt_secret.clone();
    let kms_addr = cfg.kms.grpc_addr.clone();
    let session_addr = cfg.session_api.grpc_addr.clone();
    let ws_addr = cfg.websocket.grpc_addr.clone();
    let listen_addr: SocketAddr = SocketAddr::from(([0, 0, 0, 0], cfg.api_gateway.http_port));

    let db = PgPoolOptions::new()
        .max_connections(20)
        .connect(database_url)
        .await
        .context("Failed to connect to PostgreSQL")?;

    sqlx::migrate!("../../migrations")
        .run(&db)
        .await
        .context("Failed to run database migrations")?;

    let redis_client =
        redis::Client::open(redis_url.as_str()).context("Failed to create Redis client")?;
    let redis_conn = redis::aio::ConnectionManager::new(redis_client)
        .await
        .context("Failed to connect to Redis")?;

    let state = Arc::new(AppState {
        db,
        redis: redis_conn,
        jwt_secret,
        kms_addr,
        session_addr,
        ws_addr,
    });

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // Public routes (no auth required)
    let public_routes = Router::new()
        .route("/api/v1/auth/signup", post(routes::signup))
        .route("/api/v1/auth/login", post(routes::login))
        .route("/api/v1/auth/refresh", post(routes::refresh_token))
        .route("/api/v1/auth/verify-email", get(routes::verify_email))
        .route("/api/v1/public/{slug}", get(routes::get_session_by_slug))
        .route("/api/v1/chains", get(routes::list_chains))
        .route("/api/v1/health", get(routes::health));

    // Protected routes (JWT required)
    let protected_routes = Router::new()
        .route("/api/v1/auth/2fa/setup", post(routes::setup_2fa))
        .route("/api/v1/auth/2fa/verify", post(routes::verify_2fa))
        .route(
            "/api/v1/auth/resend-verification",
            post(routes::resend_verification),
        )
        .route("/api/v1/profile", get(routes::get_profile))
        .route("/api/v1/wallets", post(routes::create_wallet))
        .route("/api/v1/wallets", get(routes::list_wallets))
        .route(
            "/api/v1/wallets/{wallet_id}/balance",
            get(routes::get_balance),
        )
        .route(
            "/api/v1/wallets/{wallet_id}/export",
            post(routes::export_wallet),
        )
        .route("/api/v1/sessions", post(routes::create_session))
        .route("/api/v1/sessions", get(routes::list_sessions))
        .route("/api/v1/sessions/{session_id}", get(routes::get_session))
        .route(
            "/api/v1/sessions/{session_id}/status",
            put(routes::update_session_status),
        )
        .route(
            "/api/v1/sessions/{session_id}/config",
            put(routes::update_session_config),
        )
        .route(
            "/api/v1/sessions/{session_id}/sharing",
            put(routes::toggle_public_sharing),
        )
        .route("/api/v1/sessions/paths", post(routes::get_swap_paths))
        .route(
            "/api/v1/sessions/pools/discover",
            post(routes::discover_pools),
        )
        .route("/api/v1/tokens/metadata", get(routes::get_token_metadata))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            jwt_middleware::require_auth,
        ));

    let app = Router::new()
        .merge(public_routes)
        .merge(protected_routes)
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    info!(%listen_addr, "API Gateway starting");

    let listener = tokio::net::TcpListener::bind(listen_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
