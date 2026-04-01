# 💧 Liquifier

> **DeFi SaaS platform** for systematically offloading large token positions over time without causing massive market price impact — built on EVM-compatible chains.

---

## Architecture Overview

Liquifier is built as a **microservices system** comprising six isolated Rust services, a PostgreSQL database, Redis, Apache Kafka, and a Next.js frontend.

```
┌─────────────────────────────────────────────────────────┐
│                    Next.js Frontend                      │
│   (App Router · TypeScript · Tailwind · Recharts)        │
└───────────────────────┬─────────────────────────────────┘
                        │  REST / WebSocket
┌───────────────────────▼─────────────────────────────────┐
│  Service 1: API Gateway & Auth                           │
│  (Axum · JWT · Argon2 · TOTP/2FA)                        │
└──────┬───────────────────────────┬──────────────────────┘
       │ gRPC                      │ gRPC
┌──────▼──────────┐    ┌──────────▼─────────────────────┐
│  Service 2: KMS │    │  Service 3: Sessions             │
│  (AES-256-GCM)  │    │  (CRUD · Top-5 Paths)           │
│  ★ Air-gapped   │    └─────────────────────────────────┘
└─────────────────┘
                              Kafka: evm.dex.swaps
┌────────────────────────────────────────────────────────┐
│  Service 4: Central Indexer                             │
│  (Alloy WebSocket · Uniswap V2/V3 events → Kafka)      │
└────────────────────────────────────────────────────────┘
                              Kafka: evm.dex.swaps
┌────────────────────────────────────────────────────────┐
│  Service 5: Execution Engine                            │
│  (POV · x·y=k Impact · gRPC→KMS · Flashbots)           │
└────────────────────────────────────────────────────────┘
                         Kafka: trades.completed
┌────────────────────────────────────────────────────────┐
│  Service 6: WS-Metrics                                  │
│  (Axum WebSocket · Live metrics · Public share links)   │
└────────────────────────────────────────────────────────┘
```

## Services

| # | Service | Port | Description |
|---|---------|------|-------------|
| 1 | **gateway** | 8080 | Public REST API, Auth (JWT + Argon2 + TOTP), routes to internal services via gRPC |
| 2 | **kms** | 50051 (internal) | Wallet key generation, AES-256-GCM encryption, transaction signing — never exposed publicly |
| 3 | **sessions** | 50052 (internal) | Session CRUD, top-5 swap path discovery via Uniswap V2 router |
| 4 | **indexer** | — | EVM WebSocket listener, decodes Swap events (V2 + V3), publishes to Kafka |
| 5 | **engine** | — | Subscribes to swaps, applies POV logic, calculates price impact, signs & submits via Flashbots |
| 6 | **ws-metrics** | 8081 | Consumes Kafka events, broadcasts live JSON over WebSocket to frontend |

## Quick Start

### Prerequisites

- Docker & Docker Compose v2
- (Optional) Rust 1.78+ for local development

### 1. Configure environment

Edit environment variables in `docker-compose.yml`:
- `MASTER_ENCRYPTION_KEY` — generate with `openssl rand -base64 32`
- `JWT_SECRET` — minimum 32 characters
- `EVM_RPC_URL` / `EVM_WS_URL` — your Infura/Alchemy endpoint

### 2. Start all services

```bash
docker compose up --build
```

### 3. Access

| Service | URL |
|---------|-----|
| Frontend | http://localhost:3000 |
| API Gateway | http://localhost:8080 |
| WS Metrics | ws://localhost:8081 |

## Tech Stack

**Backend (Rust Microservices):** Tokio · Axum · Tonic (gRPC) · Alloy (EVM) · SQLx · rdkafka · aes-gcm · argon2 · totp-rs · jsonwebtoken

**Frontend (Next.js):** Next.js 14 (App Router) · TypeScript · Tailwind CSS · Radix UI · TanStack Query · Zustand · Recharts · WebSocket API

**Infrastructure:** PostgreSQL 16 · Redis 7 · Apache Kafka

## Key Features

### POV (Percentage of Volume) Execution
Sells `X%` of incoming buy volume in the same pool where the buy occurred.

### Price Impact Protection (x·y=k)
```
impact_bps = (delta_in × 10,000) / (reserve_in + delta_in)
```
Skips trades exceeding `max_price_impact_bps`.

### Multi-Hop Async Routing
Background tasks route intermediate tokens to the final target token.

### Security
- KMS fully air-gapped (no public ports)
- AES-256-GCM encryption at rest; keys only decrypted in-memory during signing
- Flashbots relay for MEV protection

## API Reference

```
POST   /api/auth/signup
POST   /api/auth/login
POST   /api/auth/2fa/setup
POST   /api/auth/2fa/verify
POST   /api/wallets
GET    /api/wallets
GET    /api/wallets/:id/balances
POST   /api/sessions
GET    /api/sessions
GET    /api/sessions/:id
PUT    /api/sessions/:id
DELETE /api/sessions/:id
POST   /api/sessions/:id/start
POST   /api/sessions/:id/pause
GET    /api/public/:slug

ws://host:8081/ws/:session_id
ws://host:8081/ws/public/:slug
```

## License

MIT
