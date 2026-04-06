# liquifier
# Liquifier

A SaaS DeFi platform for systematically offloading large token positions over time on EVM-compatible chains (Ethereum, Base, Arbitrum, BSC, etc.) without causing massive market price impact.

## Architecture

Six Rust microservices communicating over gRPC (internal) and NATS JetStream (events), with a Next.js frontend.

```
┌─────────────┐     ┌──────────────┐     ┌──────────────────┐
│  Frontend   │────▶│  API Gateway │────▶│   Session API    │
│  (Next.js)  │     │  (Axum HTTP) │     │   (gRPC server)  │
└──────┬──────┘     └──────┬───────┘     └──────────────────┘
       │                   │
       │ WebSocket         │ gRPC
       ▼                   ▼
┌──────────────┐     ┌──────────┐     ┌──────────────────────┐
│  WebSocket   │◀────│   KMS    │     │  Execution Engine    │
│  Service     │NATS │ (gRPC)   │◀────│  (NATS consumer)     │
└──────────────┘     └──────────┘     └───────────┬──────────┘
                                                  │ NATS
                                      ┌───────────▼──────────┐
                                      │      Indexer         │
                                      │  (EVM WS listener)   │
                                      └──────────────────────┘
```

### Services

| Service | Port | Description |
|---|---|---|
| **API Gateway** | 8080 | Public HTTP API — auth, wallet CRUD, session management |
| **WebSocket Service** | 8081 | Live session updates via WebSocket + internal gRPC metrics receiver |
| **KMS** | 50051 (internal) | Key management — wallet generation, AES-256-GCM encryption, transaction signing |
| **Session API** | 50052 (internal) | Session CRUD, swap path computation |
| **Indexer** | — | Subscribes to EVM WebSocket RPCs, parses DEX swap events, publishes to NATS |
| **Execution Engine** | — | Consumes swap events from NATS, executes trades for active sessions |

### Infrastructure

- **PostgreSQL** — Relational store (users, wallets, sessions, trades, audit log)
- **Redis** — Session caching and connection management
- **NATS JetStream** — Event streaming between services

## Tech Stack

**Backend:** Rust, Tokio, Axum, Tonic (gRPC), Alloy (EVM), SQLx, NATS  
**Frontend:** Next.js 15, TypeScript, Tailwind CSS, React Query, Zustand, Recharts  
**Security:** AES-256-GCM key encryption, Argon2id password hashing, JWT auth, TOTP 2FA

## Getting Started

### Prerequisites

- Rust 1.75+
- Node.js 20+
- Docker & Docker Compose
- `protoc` (Protocol Buffers compiler)

### Setup

```bash
# Clone and configure
cp .env.example .env
# Edit .env with your values:
#   - Generate JWT_SECRET:           openssl rand -base64 64
#   - Generate MASTER_ENCRYPTION_KEY: openssl rand -hex 32
#   - Add EVM RPC WebSocket URLs

# Start infrastructure
docker compose up -d postgres redis nats

# Run database migrations
psql $DATABASE_URL -f migrations/init.sql

# Build and run services (development)
cargo build
cargo run -p api-gateway &
cargo run -p kms &
cargo run -p session-api &
cargo run -p indexer &
cargo run -p execution-engine &
cargo run -p websocket-service &

# Frontend
cd frontend
npm install
npm run dev
```

### Docker (all services)

```bash
docker compose up --build
```

## Project Structure

```
├── crates/
│   ├── common/              # Shared types, proto bindings, error types
│   ├── api-gateway/         # Public HTTP API (Axum)
│   ├── kms/                 # Key Management Service (gRPC)
│   ├── session-api/         # Session CRUD (gRPC)
│   ├── indexer/             # EVM swap event listener
│   ├── execution-engine/    # Trade execution (NATS consumer)
│   └── websocket-service/   # WebSocket + metrics (gRPC)
├── frontend/                # Next.js app
├── proto/                   # Protobuf definitions
├── migrations/              # PostgreSQL schema
├── docker-compose.yml
└── Dockerfile.rust
```

## Environment Variables

See [`.env.example`](.env.example) for the full list. Key variables:

| Variable | Description |
|---|---|
| `POSTGRES_PASSWORD` | PostgreSQL password |
| `JWT_SECRET` | Secret for signing JWT tokens |
| `MASTER_ENCRYPTION_KEY` | 32-byte hex key for AES-256-GCM wallet encryption |
| `EVM_WS_URLS` | Comma-separated EVM WebSocket RPC endpoints |
| `EVM_RPC_URLS` | Comma-separated EVM HTTP RPC endpoints |
| `APP__APPLICATION__ENVIRONMENT` | Active config profile (`production` loads `config/production.yml`) |
| `APP__SMTP__BASE_URL` | Public base URL for verification links in emails |
| `APP__API_GATEWAY__CORS_ALLOWED_ORIGIN` | Trusted browser origin for API CORS |
| `APP__WEBSOCKET__CORS_ALLOWED_ORIGIN` | Trusted browser origin for WebSocket CORS |
| `NEXT_PUBLIC_API_URL` | Frontend API base URL (for Caddy setup use `https://liquifier.penitools.com`) |
| `NEXT_PUBLIC_WS_URL` | Frontend WS base URL (for Caddy setup use `wss://liquifier.penitools.com`) |

## License

Private — All rights reserved.
