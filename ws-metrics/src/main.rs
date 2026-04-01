//! Real-Time WebSocket / Metrics Service
//!
//! Consumes `trades.completed` and `session.updates` Kafka topics
//! and broadcasts live JSON metrics to connected Next.js WebSocket clients.
//! Also supports read-only public share links via `/ws/public/:slug`.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use rdkafka::{
    config::ClientConfig,
    consumer::{Consumer, StreamConsumer},
    message::Message as KafkaMessage,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::{
    collections::HashMap,
    sync::Arc,
    time::Duration,
};
use tokio::sync::{broadcast, RwLock};
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::{error, info, warn};
use uuid::Uuid;

// ─── Event payload ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeCompleted {
    pub session_id:       String,
    pub tx_hash:          String,
    pub pool_address:     String,
    pub amount_in:        String,
    pub amount_out:       String,
    pub price_impact_bps: u32,
    pub status:           String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetrics {
    pub session_id:        String,
    pub total_amount:      String,
    pub amount_sold:       String,
    pub remaining:         String,
    pub trade_count:       i64,
    pub last_trade_at:     Option<String>,
    pub status:            String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", content = "data")]
pub enum WsEvent {
    TradeUpdate(TradeCompleted),
    SessionMetrics(SessionMetrics),
    Ping,
}

// ─── App state ─────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AppState {
    db:        PgPool,
    /// Per-session broadcast channel; keyed by session_id
    channels:  Arc<RwLock<HashMap<String, broadcast::Sender<String>>>>,
}

impl AppState {
    fn new(db: PgPool) -> Self {
        Self {
            db,
            channels: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn broadcast(&self, session_id: &str, msg: &str) {
        let channels = self.channels.read().await;
        if let Some(tx) = channels.get(session_id) {
            let _ = tx.send(msg.to_string());
        }
    }

    async fn get_or_create_channel(&self, session_id: &str) -> broadcast::Receiver<String> {
        {
            let channels = self.channels.read().await;
            if let Some(tx) = channels.get(session_id) {
                return tx.subscribe();
            }
        }
        let mut channels = self.channels.write().await;
        let (tx, rx) = broadcast::channel(256);
        channels.insert(session_id.to_string(), tx);
        rx
    }
}

// ─── WebSocket handlers ────────────────────────────────────────────────────

/// GET /ws/:session_id   — authenticated session dashboard
async fn ws_session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state, session_id, false))
}

/// GET /ws/public/:slug  — read-only public share link
async fn ws_public(
    State(state): State<Arc<AppState>>,
    Path(slug): Path<String>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    // Resolve slug → session_id
    let session_id = {
        let row = sqlx::query_scalar!(
            "SELECT id::text FROM sessions WHERE public_slug = $1",
            slug
        )
        .fetch_optional(&state.db)
        .await;

        match row {
            Ok(Some(id)) => id,
            _ => {
                // Respond with 404-equivalent close frame
                return ws.on_upgrade(|mut socket| async move {
                    let _ = socket.send(Message::Text(
                        r#"{"error":"Session not found"}"#.into(),
                    )).await;
                });
            }
        }
    };

    ws.on_upgrade(move |socket| handle_ws(socket, state, session_id, true))
}

async fn handle_ws(
    mut socket: WebSocket,
    state: Arc<AppState>,
    session_id: String,
    read_only: bool,
) {
    info!(%session_id, read_only, "WebSocket client connected");

    // Send current session metrics immediately
    if let Ok(metrics) = fetch_session_metrics(&state.db, &session_id).await {
        let payload = serde_json::to_string(&WsEvent::SessionMetrics(metrics)).unwrap_or_default();
        if socket.send(Message::Text(payload)).await.is_err() {
            return;
        }
    }

    // Subscribe to live updates
    let mut rx = state.get_or_create_channel(&session_id).await;

    let ping_interval = Duration::from_secs(30);
    let mut ping_ticker = tokio::time::interval(ping_interval);

    loop {
        tokio::select! {
            // Broadcast from Kafka consumer
            Ok(msg) = rx.recv() => {
                if socket.send(Message::Text(msg)).await.is_err() {
                    break;
                }
            }
            // Periodic ping to keep connection alive
            _ = ping_ticker.tick() => {
                let ping = serde_json::to_string(&WsEvent::Ping).unwrap_or_default();
                if socket.send(Message::Text(ping)).await.is_err() {
                    break;
                }
            }
            // Messages from client (pause/resume in authenticated mode)
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) if !read_only => {
                        handle_client_message(&state, &session_id, &text).await;
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }

    info!(%session_id, "WebSocket client disconnected");
}

async fn handle_client_message(state: &AppState, session_id: &str, text: &str) {
    // Expect { "action": "pause" | "resume" }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
        let action = v.get("action").and_then(|a| a.as_str()).unwrap_or("");
        let new_status = match action {
            "pause"  => "paused",
            "resume" => "active",
            _ => return,
        };
        if let Ok(id) = session_id.parse::<Uuid>() {
            let _ = sqlx::query!(
                "UPDATE sessions SET status = $1::session_status WHERE id = $2",
                new_status as _,
                id
            )
            .execute(&state.db)
            .await;
        }
    }
}

async fn fetch_session_metrics(db: &PgPool, session_id: &str) -> anyhow::Result<SessionMetrics> {
    let id: Uuid = session_id.parse()?;

    let row = sqlx::query!(
        r#"
        SELECT s.id, s.total_amount, s.amount_sold,
               s.status as "status: String",
               COUNT(t.id) as trade_count,
               MAX(t.created_at)::text as last_trade_at
        FROM sessions s
        LEFT JOIN trades t ON t.session_id = s.id
        WHERE s.id = $1
        GROUP BY s.id
        "#,
        id
    )
    .fetch_one(db)
    .await?;

    let total: bigdecimal::BigDecimal = row.total_amount;
    let sold:  bigdecimal::BigDecimal = row.amount_sold;
    let remaining = total.clone() - sold.clone();

    Ok(SessionMetrics {
        session_id:    session_id.to_string(),
        total_amount:  total.to_string(),
        amount_sold:   sold.to_string(),
        remaining:     remaining.to_string(),
        trade_count:   row.trade_count.unwrap_or(0),
        last_trade_at: row.last_trade_at,
        status:        row.status,
    })
}

// ─── Kafka consumer loop ───────────────────────────────────────────────────

async fn kafka_consumer_loop(state: Arc<AppState>, brokers: String, topics: Vec<String>) {
    let consumer: StreamConsumer = ClientConfig::new()
        .set("bootstrap.servers", &brokers)
        .set("group.id",          "ws-metrics")
        .set("auto.offset.reset", "latest")
        .create()
        .expect("Cannot create Kafka consumer");

    let topic_refs: Vec<&str> = topics.iter().map(String::as_str).collect();
    consumer.subscribe(&topic_refs).expect("subscribe");

    info!("WS-Metrics Kafka consumer started");

    loop {
        match consumer.recv().await {
            Err(e) => error!("Kafka recv: {e}"),
            Ok(msg) => {
                if let Some(payload) = msg.payload() {
                    if let Ok(trade) = serde_json::from_slice::<TradeCompleted>(payload) {
                        let session_id = trade.session_id.clone();
                        let event = WsEvent::TradeUpdate(trade);
                        let msg = serde_json::to_string(&event).unwrap_or_default();
                        state.broadcast(&session_id, &msg).await;

                        // Fetch and broadcast updated metrics
                        if let Ok(metrics) = fetch_session_metrics(&state.db, &session_id).await {
                            let msg =
                                serde_json::to_string(&WsEvent::SessionMetrics(metrics))
                                    .unwrap_or_default();
                            state.broadcast(&session_id, &msg).await;
                        }
                    }
                }
            }
        }
    }
}

// ─── Main ──────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let database_url    = std::env::var("DATABASE_URL")?;
    let brokers         = std::env::var("KAFKA_BROKERS")?;
    let topic_trades    = std::env::var("KAFKA_TOPIC_TRADES_COMPLETED")
                            .unwrap_or_else(|_| "trades.completed".into());
    let topic_sessions  = std::env::var("KAFKA_TOPIC_SESSION_UPDATES")
                            .unwrap_or_else(|_| "session.updates".into());
    let bind_addr       = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8081".into());

    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await?;

    let state = Arc::new(AppState::new(db));

    // Spawn Kafka consumer background task
    {
        let state   = state.clone();
        let brokers = brokers.clone();
        let topics  = vec![topic_trades, topic_sessions];
        tokio::spawn(async move {
            kafka_consumer_loop(state, brokers, topics).await;
        });
    }

    let app = Router::new()
        .route("/ws/:session_id",       get(ws_session))
        .route("/ws/public/:slug",      get(ws_public))
        .route("/health",               get(|| async { "ok" }))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    info!("WS-Metrics service listening on {bind_addr}");
    axum::serve(listener, app).await?;

    Ok(())
}
