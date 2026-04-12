use anyhow::{bail, Context, Result};
use axum::{
    http::HeaderValue,
    middleware,
    routing::{get, post, put},
    Router,
};
use common::types::{
    DepositEvent, TradeCompletedEvent, SUBJECT_DEPOSITS, SUBJECT_TRADES_COMPLETED,
};
use futures::StreamExt;
use sqlx::postgres::PgPoolOptions;
use sqlx::Row;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};

mod auth;
mod email;
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
    pub email: Option<email::EmailSender>,
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
        // .json()
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

    // Seed admin user from env config on first start
    seed_admin_user(&db, &cfg.auth).await?;

    let redis_client =
        redis::Client::open(redis_url.as_str()).context("Failed to create Redis client")?;
    let redis_conn = redis::aio::ConnectionManager::new(redis_client)
        .await
        .context("Failed to connect to Redis")?;

    let email_sender = email::EmailSender::new(&cfg.smtp);

    let state = Arc::new(AppState {
        db,
        redis: redis_conn,
        jwt_secret,
        kms_addr,
        session_addr,
        ws_addr,
        email: email_sender,
    });

    let cors = build_cors_layer(&cfg.api_gateway.cors_allowed_origin)?;

    // Public routes (no auth required)
    let public_routes = Router::new()
        .route("/api/v1/auth/signup", post(routes::signup))
        .route("/api/v1/auth/login", post(routes::login))
        .route("/api/v1/auth/refresh", post(routes::refresh_token))
        .route("/api/v1/auth/verify-email", get(routes::verify_email))
        .route("/api/v1/public/{slug}", get(routes::get_session_by_slug))
        .route(
            "/api/v1/public/{slug}/trades",
            get(routes::get_public_session_trades),
        )
        .route("/api/v1/chains", get(routes::list_chains))
        .route("/api/v1/health", get(routes::health))
        .route("/api/v1/config", get(routes::get_platform_config));

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
            "/api/v1/sessions/{session_id}/trades",
            get(routes::get_session_trades),
        )
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
        .route(
            "/api/v1/sessions/pools/path",
            post(routes::compute_pool_path),
        )
        .route("/api/v1/tokens/metadata", get(routes::get_token_metadata))
        .route("/api/v1/tokens/usd-price", get(routes::get_token_usd_price))
        // Refund requests (common users create, all users list their own)
        .route("/api/v1/refunds", post(routes::create_refund_request))
        .route("/api/v1/refunds", get(routes::list_my_refund_requests))
        // Common user: sessions on their wallets
        .route("/api/v1/my/sessions", get(routes::list_my_wallet_sessions))
        // Common user: deposit history
        .route("/api/v1/my/deposits", get(routes::list_my_deposits))
        // Common user: start selling
        .route("/api/v1/my/start-selling", post(routes::start_selling))
        // Admin routes
        .route("/api/v1/admin/users", get(routes::admin_list_users))
        .route(
            "/api/v1/admin/users/{user_id}/wallets",
            get(routes::admin_get_user_wallets),
        )
        .route(
            "/api/v1/admin/users/{user_id}/wallets/{wallet_id}/export",
            post(routes::admin_export_user_wallet),
        )
        .route(
            "/api/v1/admin/users/{user_id}/sessions",
            get(routes::admin_get_user_sessions),
        )
        .route(
            "/api/v1/admin/refunds",
            get(routes::admin_list_refund_requests),
        )
        .route(
            "/api/v1/admin/refunds/{refund_id}",
            put(routes::admin_update_refund_status),
        )
        .route(
            "/api/v1/admin/users/{user_id}/role",
            put(routes::admin_update_user_role),
        )
        .route("/api/v1/admin/wallets", get(routes::admin_list_all_wallets))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            jwt_middleware::require_auth,
        ));

    let app = Router::new()
        .merge(public_routes)
        .merge(protected_routes)
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());

    info!(%listen_addr, "API Gateway starting");

    // Spawn background deposit-alert consumer
    {
        let db = state.db.clone();
        let email_sender = state.email.clone();
        let nats_url = cfg.nats.url.clone();
        tokio::spawn(async move {
            if let Err(e) = run_deposit_alert_consumer(&nats_url, &db, email_sender.as_ref()).await
            {
                error!(error = %e, "Deposit alert consumer exited with error");
            }
        });
    }

    // Spawn background trade-alert consumer (emails common users on each sale)
    {
        let db = state.db.clone();
        let email_sender = state.email.clone();
        let nats_url = cfg.nats.url.clone();
        tokio::spawn(async move {
            if let Err(e) = run_trade_alert_consumer(&nats_url, &db, email_sender.as_ref()).await {
                error!(error = %e, "Trade alert consumer exited with error");
            }
        });
    }

    let listener = tokio::net::TcpListener::bind(listen_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

fn build_cors_layer(allowed_origin: &str) -> Result<CorsLayer> {
    let origin = allowed_origin.trim();
    let base = CorsLayer::new().allow_methods(Any).allow_headers(Any);

    if origin.is_empty() {
        bail!("api_gateway.cors_allowed_origin cannot be empty");
    }

    if origin == "*" {
        bail!(
            "api_gateway.cors_allowed_origin must be a trusted origin, wildcard '*' is not allowed"
        );
    }

    let origin_header = HeaderValue::from_str(origin)
        .context("api_gateway.cors_allowed_origin must be a valid origin header value")?;
    Ok(base.allow_origin(AllowOrigin::exact(origin_header)))
}

// ─────────────────────────────────────────────────────────────
// Seed admin user from env config on first start
// ─────────────────────────────────────────────────────────────

async fn seed_admin_user(
    db: &sqlx::PgPool,
    auth_cfg: &liquifier_config::AuthSettings,
) -> Result<()> {
    if auth_cfg.admin_email.is_empty() || auth_cfg.admin_password.is_empty() {
        info!("No admin seed credentials configured, skipping admin seeding");
        return Ok(());
    }

    // Check if an admin already exists
    let existing = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users WHERE role = 'admin'")
        .fetch_one(db)
        .await
        .context("Failed to check for existing admin")?;

    if existing > 0 {
        info!("Admin user already exists, skipping admin seeding");
        return Ok(());
    }

    let password_hash = auth::hash_password(&auth_cfg.admin_password)
        .map_err(|e| anyhow::anyhow!("Failed to hash admin seed password: {e}"))?;

    let id = uuid::Uuid::new_v4();
    let verification_token = uuid::Uuid::new_v4().to_string();
    let expires_at = chrono::Utc::now() + chrono::Duration::hours(24);

    sqlx::query(
        "INSERT INTO users (id, email, password_hash, verification_token, verification_token_expires_at, role, email_verified) \
         VALUES ($1, $2, $3, $4, $5, 'admin'::user_role, false)"
    )
    .bind(id)
    .bind(&auth_cfg.admin_email)
    .bind(&password_hash)
    .bind(&verification_token)
    .bind(expires_at)
    .execute(db)
    .await
    .context("Failed to insert seed admin user")?;

    info!(email = %auth_cfg.admin_email, "Seed admin user created — verify email and set up 2FA on first login");

    Ok(())
}

// ─────────────────────────────────────────────────────────────
// Background: NATS deposit alert consumer
// ─────────────────────────────────────────────────────────────

async fn run_deposit_alert_consumer(
    nats_url: &str,
    db: &sqlx::PgPool,
    email_sender: Option<&email::EmailSender>,
) -> Result<()> {
    let nats_client = common::retry::retry("NATS (deposit consumer)", 10, || async {
        async_nats::connect(nats_url).await
    })
    .await
    .context("Failed to connect to NATS for deposit alerts")?;

    let jetstream = async_nats::jetstream::new(nats_client);

    // Ensure the DEPOSITS stream exists (idempotent)
    jetstream
        .get_or_create_stream(async_nats::jetstream::stream::Config {
            name: "DEPOSITS".to_string(),
            subjects: vec![SUBJECT_DEPOSITS.to_string()],
            retention: async_nats::jetstream::stream::RetentionPolicy::WorkQueue,
            max_age: std::time::Duration::from_secs(86400),
            ..Default::default()
        })
        .await
        .context("Failed to create DEPOSITS NATS stream")?;

    let consumer = jetstream
        .create_consumer_on_stream(
            async_nats::jetstream::consumer::pull::Config {
                durable_name: Some("api-gateway-deposit-alerts".to_string()),
                filter_subject: SUBJECT_DEPOSITS.to_string(),
                ..Default::default()
            },
            "DEPOSITS",
        )
        .await
        .context("Failed to create NATS deposit consumer")?;

    info!("Deposit alert consumer started");

    let mut messages = consumer.messages().await?;

    while let Some(Ok(msg)) = messages.next().await {
        let deposit: DepositEvent = match serde_json::from_slice(&msg.payload) {
            Ok(d) => d,
            Err(e) => {
                warn!(error = %e, "Failed to deserialize deposit event");
                let _ = msg.ack().await;
                continue;
            }
        };

        // Skip alerts for transfer events where the sender is a tracked pool or pair address.
        // These are usually session-driven sells and should not be treated as user deposits.
        match is_tracked_pool_or_pair_transfer(db, &deposit.chain, &deposit.from).await {
            Ok(true) => {
                info!(
                    chain = %deposit.chain,
                    from = %deposit.from,
                    to = %deposit.to,
                    tx = %deposit.tx_hash,
                    "Skipping deposit alert: transfer originated from tracked pool/pair"
                );
                let _ = msg.ack().await;
                continue;
            }
            Ok(false) => {}
            Err(e) => {
                warn!(
                    chain = %deposit.chain,
                    from = %deposit.from,
                    error = %e,
                    "Failed to check tracked pool transfer; continuing with alert processing"
                );
            }
        }

        // Record deposit to database (idempotent via unique constraint)
        let deposit_id = uuid::Uuid::new_v4();
        let wallet_uuid: uuid::Uuid = deposit.wallet_id.parse().unwrap_or_default();
        let user_uuid: uuid::Uuid = deposit.user_id.parse().unwrap_or_default();
        let _ = sqlx::query(
            "INSERT INTO deposits (id, user_id, wallet_id, chain, token_address, from_address, amount, tx_hash, block_number, log_index)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
             ON CONFLICT (chain, tx_hash, log_index) DO NOTHING",
        )
        .bind(deposit_id)
        .bind(user_uuid)
        .bind(wallet_uuid)
        .bind(&deposit.chain)
        .bind(&deposit.token_address)
        .bind(&deposit.from)
        .bind(&deposit.amount)
        .bind(&deposit.tx_hash)
        .bind(deposit.block_number as i64)
        .bind(deposit.log_index as i32)
        .execute(db)
        .await;

        // Look up user info
        let user_info = sqlx::query(
            "SELECT email, COALESCE(username, email) as display_name FROM users WHERE id = $1::uuid",
        )
        .bind(&deposit.user_id)
        .fetch_optional(db)
        .await;

        let (user_email, username) = match user_info {
            Ok(Some(row)) => {
                let email: String = row.get("email");
                let name: String = row.get("display_name");
                (email, name)
            }
            _ => {
                warn!(user_id = %deposit.user_id, "User not found for deposit alert");
                let _ = msg.ack().await;
                continue;
            }
        };

        let wallet_address = deposit.to.clone();

        // Get all admin emails
        let admin_rows = sqlx::query_scalar::<_, String>(
            "SELECT email FROM users WHERE role = 'admin' AND email_verified = true",
        )
        .fetch_all(db)
        .await;

        let admin_emails = match admin_rows {
            Ok(emails) => emails,
            Err(e) => {
                warn!(error = %e, "Failed to query admin emails");
                let _ = msg.ack().await;
                continue;
            }
        };

        if let Some(sender) = email_sender {
            // Send deposit alert to admins
            for admin_email in &admin_emails {
                sender
                    .send_deposit_alert(
                        admin_email,
                        &username,
                        &user_email,
                        &wallet_address,
                        &deposit.amount,
                        &deposit.token_address,
                        &deposit.tx_hash,
                        &deposit.chain,
                    )
                    .await;
            }
            // Also send deposit alert to the user themselves
            sender
                .send_user_deposit_alert(
                    &user_email,
                    &username,
                    &wallet_address,
                    &deposit.amount,
                    &deposit.token_address,
                    &deposit.tx_hash,
                    &deposit.chain,
                )
                .await;
            info!(
                user = %username,
                token = %deposit.token_address,
                amount = %deposit.amount,
                admins_notified = admin_emails.len(),
                "Deposit alert emails sent (admin + user)"
            );
        } else {
            info!(
                user = %username,
                token = %deposit.token_address,
                amount = %deposit.amount,
                tx = %deposit.tx_hash,
                "Deposit detected (SMTP not configured, skipping email)"
            );
        }

        let _ = msg.ack().await;
    }

    Ok(())
}

/// Check if the sender address is a tracked pool or pair (from any active session).
/// This covers both session_pools entries and any known DEX pair addresses.
async fn is_tracked_pool_or_pair_transfer(
    db: &sqlx::PgPool,
    chain: &str,
    from_address: &str,
) -> Result<bool> {
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
            SELECT 1
            FROM session_pools sp
            JOIN sessions s ON s.id = sp.session_id
            WHERE (LOWER(sp.pool_address) = LOWER($1)
                   OR LOWER(sp.token0) = LOWER($1)
                   OR LOWER(sp.token1) = LOWER($1))
              AND s.chain::text = $2
              AND s.status IN ('active', 'pending')
        )",
    )
    .bind(from_address)
    .bind(chain)
    .fetch_one(db)
    .await
    .context("Failed querying tracked pool/pair address")?;

    Ok(exists)
}

// ─────────────────────────────────────────────────────────────
// Background: NATS trade alert consumer (emails common users)
// ─────────────────────────────────────────────────────────────

async fn run_trade_alert_consumer(
    nats_url: &str,
    db: &sqlx::PgPool,
    email_sender: Option<&email::EmailSender>,
) -> Result<()> {
    let nats_client = common::retry::retry("NATS (trade alert consumer)", 10, || async {
        async_nats::connect(nats_url).await
    })
    .await
    .context("Failed to connect to NATS for trade alerts")?;

    let jetstream = async_nats::jetstream::new(nats_client);

    // Ensure the TRADES_COMPLETED stream exists (idempotent)
    jetstream
        .get_or_create_stream(async_nats::jetstream::stream::Config {
            name: "TRADES_COMPLETED".to_string(),
            subjects: vec![SUBJECT_TRADES_COMPLETED.to_string()],
            max_age: std::time::Duration::from_secs(3600),
            ..Default::default()
        })
        .await
        .context("Failed to create TRADES_COMPLETED NATS stream")?;

    let consumer = jetstream
        .create_consumer_on_stream(
            async_nats::jetstream::consumer::pull::Config {
                durable_name: Some("api-gateway-trade-alerts".to_string()),
                filter_subject: SUBJECT_TRADES_COMPLETED.to_string(),
                ..Default::default()
            },
            "TRADES_COMPLETED",
        )
        .await
        .context("Failed to create NATS trade consumer")?;

    info!("Trade alert consumer started");

    let mut messages = consumer.messages().await?;

    while let Some(Ok(msg)) = messages.next().await {
        let trade: TradeCompletedEvent = match serde_json::from_slice(&msg.payload) {
            Ok(t) => t,
            Err(e) => {
                warn!(error = %e, "Failed to deserialize trade completed event");
                let _ = msg.ack().await;
                continue;
            }
        };

        // Look up session → wallet → user (common users only)
        let user_row = sqlx::query(
            "SELECT u.email, COALESCE(u.username, u.email) AS display_name
             FROM sessions s
             JOIN wallets w ON w.id = s.wallet_id
             JOIN users u ON u.id = w.user_id
             WHERE s.id = $1::uuid
               AND u.role = 'common'
               AND u.email_verified = true",
        )
        .bind(&trade.session_id)
        .fetch_optional(db)
        .await;

        let (user_email, username) = match user_row {
            Ok(Some(row)) => {
                let email: String = row.get("email");
                let name: String = row.get("display_name");
                (email, name)
            }
            Ok(None) => {
                // Not a common user session or session not found — skip silently
                let _ = msg.ack().await;
                continue;
            }
            Err(e) => {
                warn!(session_id = %trade.session_id, error = %e, "Failed to look up user for trade alert");
                let _ = msg.ack().await;
                continue;
            }
        };

        if let Some(sender) = email_sender {
            sender
                .send_trade_alert(
                    &user_email,
                    &username,
                    &trade.trade_id,
                    &trade.session_id,
                    &trade.chain,
                    &trade.sell_amount,
                    &trade.received_amount,
                    &trade.tx_hash,
                    &trade.status,
                    trade.price_impact_bps,
                    trade.failure_reason.as_deref(),
                )
                .await;
            info!(
                user = %username,
                trade_id = %trade.trade_id,
                status = %trade.status,
                "Trade alert email sent to common user"
            );
        } else {
            info!(
                user = %username,
                trade_id = %trade.trade_id,
                status = %trade.status,
                tx = %trade.tx_hash,
                "Trade completed (SMTP not configured, skipping email)"
            );
        }

        let _ = msg.ack().await;
    }

    Ok(())
}
