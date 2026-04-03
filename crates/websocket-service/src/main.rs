use anyhow::{Context, Result};
use axum::{
    extract::{
        ws::{Message, WebSocket},
        Path, Query, State, WebSocketUpgrade,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use common::{
    proto::{
        metrics_service_server::{MetricsService, MetricsServiceServer},
        PushAck, SessionUpdateEvent, TradeCompletedEvent,
    },
    types::{SUBJECT_SESSION_UPDATES, SUBJECT_TRADES_COMPLETED},
};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use sqlx::postgres::PgPoolOptions;
use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use tokio::sync::{broadcast, RwLock};
use tonic::{transport::Server as GrpcServer, Request, Response, Status};
use tower_http::cors::{Any, CorsLayer};
use tracing::{error, info};

// ─────────────────────────────────────────────────────────────
// Shared State
// ─────────────────────────────────────────────────────────────

type SessionChannels = Arc<RwLock<HashMap<String, broadcast::Sender<String>>>>;

struct WsState {
    channels: SessionChannels,
    jwt_secret: String,
    db: sqlx::PgPool,
}

type SharedWsState = Arc<WsState>;

// ─────────────────────────────────────────────────────────────
// Metrics gRPC Service (internal push)
// ─────────────────────────────────────────────────────────────

struct MetricsServiceImpl {
    channels: SessionChannels,
}

#[tonic::async_trait]
impl MetricsService for MetricsServiceImpl {
    async fn push_session_update(
        &self,
        request: Request<SessionUpdateEvent>,
    ) -> Result<Response<PushAck>, Status> {
        let event = request.into_inner();
        let payload = serde_json::to_string(&serde_json::json!({
            "type": "session_update",
            "session_id": event.session_id,
            "status": event.status,
            "amount_sold": event.amount_sold,
            "remaining": event.remaining,
            "converted_value_usd": event.converted_value_usd,
        }))
        .unwrap_or_default();

        self.broadcast(&event.session_id, &payload).await;
        Ok(Response::new(PushAck { ok: true }))
    }

    async fn push_trade_completed(
        &self,
        request: Request<TradeCompletedEvent>,
    ) -> Result<Response<PushAck>, Status> {
        let event = request.into_inner();
        let payload = serde_json::to_string(&serde_json::json!({
            "type": "trade_completed",
            "trade_id": event.trade_id,
            "session_id": event.session_id,
            "chain": event.chain,
            "sell_amount": event.sell_amount,
            "received_amount": event.received_amount,
            "tx_hash": event.tx_hash,
            "price_impact_bps": event.price_impact_bps,
            "executed_at": event.executed_at,
        }))
        .unwrap_or_default();

        self.broadcast(&event.session_id, &payload).await;
        Ok(Response::new(PushAck { ok: true }))
    }
}

impl MetricsServiceImpl {
    async fn broadcast(&self, session_id: &str, payload: &str) {
        let channels = self.channels.read().await;
        if let Some(tx) = channels.get(session_id) {
            let _ = tx.send(payload.to_string());
        }
    }
}

// ─────────────────────────────────────────────────────────────
// WebSocket handlers
// ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct WsQuery {
    token: Option<String>,
}

/// Authenticated WebSocket: /ws/session/{session_id}?token=JWT
async fn ws_session_handler(
    ws: WebSocketUpgrade,
    Path(session_id): Path<String>,
    Query(query): Query<WsQuery>,
    State(state): State<SharedWsState>,
) -> impl IntoResponse {
    // Validate JWT
    let token = query.token.unwrap_or_default();
    let valid = jsonwebtoken::decode::<serde_json::Value>(
        &token,
        &jsonwebtoken::DecodingKey::from_secret(state.jwt_secret.as_bytes()),
        &jsonwebtoken::Validation::default(),
    )
    .is_ok();

    if !valid {
        return axum::http::Response::builder()
            .status(401)
            .body(axum::body::Body::empty())
            .unwrap()
            .into_response();
    }

    ws.on_upgrade(move |socket| handle_ws(socket, session_id, state))
        .into_response()
}

/// Public WebSocket for shared links: /ws/public/{slug}
async fn ws_public_handler(
    ws: WebSocketUpgrade,
    Path(slug): Path<String>,
    State(state): State<SharedWsState>,
) -> impl IntoResponse {
    // Look up the session by public_slug
    let session_id: Option<String> =
        sqlx::query_scalar("SELECT id::text FROM sessions WHERE public_slug = $1")
            .bind(&slug)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();

    match session_id {
        Some(sid) => ws
            .on_upgrade(move |socket| handle_ws(socket, sid, state))
            .into_response(),
        None => axum::http::Response::builder()
            .status(404)
            .body(axum::body::Body::empty())
            .unwrap()
            .into_response(),
    }
}

async fn handle_ws(socket: WebSocket, session_id: String, state: SharedWsState) {
    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Get or create a broadcast channel for this session
    let rx = {
        let mut channels = state.channels.write().await;
        let tx = channels
            .entry(session_id.clone())
            .or_insert_with(|| broadcast::channel(256).0);
        tx.subscribe()
    };

    let mut rx = rx;

    info!(session_id = %session_id, "WebSocket client connected");

    // Forward broadcast messages to the WebSocket client
    let send_task = tokio::spawn(async move {
        while let Ok(msg) = rx.recv().await {
            if ws_sender.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    });

    // Read from WebSocket (we mostly ignore client messages, but handle pings)
    let recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_receiver.next().await {
            match msg {
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    // Wait for either task to finish
    tokio::select! {
        _ = send_task => {}
        _ = recv_task => {}
    }

    info!(session_id = %session_id, "WebSocket client disconnected");
}

// ─────────────────────────────────────────────────────────────
// NATS consumer (bridges NATS → WebSocket broadcast)
// ─────────────────────────────────────────────────────────────

async fn nats_to_ws_bridge(nats_js: async_nats::jetstream::Context, channels: SessionChannels) {
    // Ensure subjects stream exists
    let _ = nats_js
        .get_or_create_stream(async_nats::jetstream::stream::Config {
            name: "TRADES_COMPLETED".to_string(),
            subjects: vec![
                SUBJECT_TRADES_COMPLETED.to_string(),
                SUBJECT_SESSION_UPDATES.to_string(),
            ],
            max_age: std::time::Duration::from_secs(3600),
            ..Default::default()
        })
        .await;

    let consumer = match nats_js.get_stream("TRADES_COMPLETED").await.and_then(|s| {
        // We need to use create_consumer from the stream, but the return is sync here.
        // Use a block_in_place or just log.
        Ok(s)
    }) {
        Ok(stream) => {
            match stream
                .get_or_create_consumer(
                    "ws-bridge",
                    async_nats::jetstream::consumer::pull::Config {
                        durable_name: Some("ws-bridge".to_string()),
                        filter_subjects: vec![
                            SUBJECT_TRADES_COMPLETED.to_string(),
                            SUBJECT_SESSION_UPDATES.to_string(),
                        ],
                        ..Default::default()
                    },
                )
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    error!(error = %e, "Failed to create NATS consumer for WS bridge");
                    return;
                }
            }
        }
        Err(e) => {
            error!(error = %e, "Failed to get NATS stream for WS bridge");
            return;
        }
    };

    let mut messages = match consumer.messages().await {
        Ok(m) => m,
        Err(e) => {
            error!(error = %e, "Failed to start NATS message stream");
            return;
        }
    };

    while let Some(Ok(msg)) = messages.next().await {
        let _ = msg.ack().await;

        // Parse the session_id from the payload and broadcast
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&msg.payload) {
            if let Some(session_id) = value.get("session_id").and_then(|v| v.as_str()) {
                let payload = serde_json::to_string(&value).unwrap_or_default();
                let channels = channels.read().await;
                if let Some(tx) = channels.get(session_id) {
                    let _ = tx.send(payload);
                }
            }
        }
    }
}

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

    let nats_url = &cfg.nats.url;
    let database_url = &cfg.database.url;
    let jwt_secret = cfg.auth.jwt_secret.clone();
    let listen_addr: SocketAddr = SocketAddr::from(([0, 0, 0, 0], cfg.websocket.http_port));
    let grpc_addr: SocketAddr = SocketAddr::from(([0, 0, 0, 0], cfg.websocket.grpc_port));

    let db = PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await
        .context("Failed to connect to PostgreSQL")?;

    let channels: SessionChannels = Arc::new(RwLock::new(HashMap::new()));

    // NATS
    let nats_client =
        common::retry::retry("NATS", 10, || async { async_nats::connect(nats_url).await })
            .await
            .context("Failed to connect to NATS")?;
    let nats_js = async_nats::jetstream::new(nats_client);

    let ws_state = Arc::new(WsState {
        channels: channels.clone(),
        jwt_secret,
        db,
    });

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/ws/session/{session_id}", get(ws_session_handler))
        .route("/ws/public/{slug}", get(ws_public_handler))
        .layer(cors)
        .with_state(ws_state);

    // Spawn NATS → WS bridge
    let bridge_channels = channels.clone();
    tokio::spawn(async move {
        nats_to_ws_bridge(nats_js, bridge_channels).await;
    });

    // Spawn gRPC server for internal push
    let grpc_channels = channels.clone();
    tokio::spawn(async move {
        info!(%grpc_addr, "Metrics gRPC server starting");
        let service = MetricsServiceImpl {
            channels: grpc_channels,
        };
        if let Err(e) = GrpcServer::builder()
            .add_service(MetricsServiceServer::new(service))
            .serve(grpc_addr)
            .await
        {
            error!(error = %e, "Metrics gRPC server failed");
        }
    });

    info!(%listen_addr, "WebSocket service starting");
    let listener = tokio::net::TcpListener::bind(listen_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
