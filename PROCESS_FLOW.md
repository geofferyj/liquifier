# Liquifier — Complete Process Flow Documentation

This document describes every step of data flow through the Liquifier platform, from raw blockchain events to user-facing WebSocket updates. Each section names the exact functions, NATS subjects, gRPC methods, SQL queries, and data structures involved.

---

## Table of Contents

1. [System Architecture Overview](#1-system-architecture-overview)
2. [Database Schema](#2-database-schema)
3. [Service 1: Indexer — On-Chain Event Ingestion](#3-service-1-indexer--on-chain-event-ingestion)
4. [Service 2: Execution Engine — Event Processing & Trade Execution](#4-service-2-execution-engine--event-processing--trade-execution)
5. [Service 3: Session API — Session Management & Pool Discovery](#5-service-3-session-api--session-management--pool-discovery)
6. [Service 4: KMS — Key Management & Transaction Signing](#6-service-4-kms--key-management--transaction-signing)
7. [Service 5: API Gateway — HTTP Interface & Authentication](#7-service-5-api-gateway--http-interface--authentication)
8. [Service 6: WebSocket Service — Real-Time Client Updates](#8-service-6-websocket-service--real-time-client-updates)
9. [End-to-End Flow: User Creates a Session](#9-end-to-end-flow-user-creates-a-session)
10. [End-to-End Flow: A DEX Swap Triggers a Trade](#10-end-to-end-flow-a-dex-swap-triggers-a-trade)
11. [Inter-Service Communication Map](#11-inter-service-communication-map)

---

## 1. System Architecture Overview

Liquifier is a DeFi execution platform that automates "Percentage of Volume" (POV) selling strategies. When a user creates a session to sell Token A for Token B, the system watches on-chain DEX swap activity. Every time someone buys Token A on a watched DEX pool, Liquifier automatically sells a proportional amount (e.g., 10% of the observed buy volume) to achieve gradual, low-impact liquidation.

### Services

| Service | Language | Transport | Port(s) | Purpose |
|---------|----------|-----------|---------|---------|
| **Indexer** | Rust | NATS publish | — | Streams raw EVM swap logs into NATS |
| **Execution Engine** | Rust (×2 replicas) | NATS consume, gRPC client | — | Processes swap events, decides trades, signs & submits txns |
| **Session API** | Rust | gRPC server | 50052 | CRUD for sessions, pool discovery, active session queries |
| **KMS** | Rust | gRPC server | 50051 | Wallet generation, key encryption/decryption, transaction signing |
| **API Gateway** | Rust | HTTP (Axum) | 8080 | REST API for frontend, JWT auth, proxies to gRPC services |
| **WebSocket Service** | Rust | HTTP WebSocket + gRPC server | 8081 / 50053 | Real-time session updates to browser clients |

### Infrastructure

| Component | Role |
|-----------|------|
| **PostgreSQL 16** | Persistent storage for users, wallets, sessions, trades, audit logs, session pools |
| **NATS 2.10 (JetStream)** | Message bus. Two streams: `DEX_SWAPS` (indexer → engine) and `TRADES_COMPLETED` (engine → websocket) |
| **Redis 7** | Connection state caching (used by API Gateway and Execution Engine) |

### Communication Protocols

- **Indexer → Execution Engine**: NATS JetStream, subject `evm.dex.swaps`, stream `DEX_SWAPS` with `WorkQueue` retention (each message consumed by exactly one engine worker)
- **Execution Engine → WebSocket Service**: NATS JetStream, subject `trades.completed`, stream `TRADES_COMPLETED`
- **Execution Engine → Session API**: gRPC call `GetActiveSessionsForToken`
- **Execution Engine → KMS**: gRPC call `SignTransaction`
- **API Gateway → Session API**: gRPC calls (`CreateSession`, `GetSession`, `ListSessions`, `UpdateSessionStatus`, `GetSwapPaths`, `DiscoverPools`)
- **API Gateway → KMS**: gRPC calls (`CreateWallet`, `ListWallets`, `GetBalance`)
- **WebSocket Service → Clients**: Axum WebSocket over HTTP upgrade

---

## 2. Database Schema

Defined in `migrations/init.sql` and `migrations/002_session_pools.sql`.

### Enums

```sql
chain_id:       'ethereum' | 'base' | 'arbitrum' | 'bsc' | 'polygon' | 'optimism'
session_status: 'pending' | 'active' | 'paused' | 'completed' | 'cancelled' | 'error'
trade_status:   'submitted' | 'confirmed' | 'failed' | 'routing'
strategy_type:  'pov'
pool_type:      'v2' | 'v3'
```

### Tables

**`users`** — Application users. Fields: `id` (UUID), `email` (unique), `password_hash` (Argon2id), `totp_secret` (BYTEA, nullable), `totp_enabled` (bool), timestamps.

**`wallets`** — Per-user custodial wallets. Fields: `id` (UUID), `user_id` (FK→users), `chain` (chain_id enum), `address` (checksummed hex), `encrypted_key` (AES-256-GCM ciphertext), `nonce` (12-byte GCM nonce), `created_at`. Unique constraint on `(chain, address)`.

**`sessions`** — Liquidation campaigns. Fields: `id` (UUID), `user_id`, `wallet_id` (FK→wallets), `chain`, `status`, `sell_token`/`target_token` addresses + symbols + decimals, `strategy` ('pov'), `total_amount` (NUMERIC(78,0) for full U256 range), `amount_sold`, `pov_percent` (e.g. 10.00), `max_price_impact` (e.g. 1.00%), `min_buy_trigger_usd`, `swap_path` (JSONB), `public_slug` (for shareable links), timestamps. Indexed on `(chain, sell_token) WHERE status = 'active'` for the hot-path query.

**`session_pools`** — DEX pools watched per session. Fields: `id`, `session_id` (FK→sessions), `pool_address`, `pool_type` (v2/v3), `dex_name`, `token0`, `token1`, `fee_tier` (bps for v3, 0 for v2). Unique on `(session_id, pool_address)`.

**`trades`** — Individual trade executions. Fields: `id`, `session_id`, `chain`, `status`, trigger info (`trigger_tx_hash`, `trigger_pool`, `trigger_buy_amount`), execution info (`sell_amount`, `sell_tx_hash`, `sell_pool`, `price_impact_bps`), routing info (`route_tx_hash`, `final_received`, `final_token`), gas info, timestamps.

**`audit_log`** — Append-only security events. Fields: `id` (BIGSERIAL), `user_id`, `action`, `metadata` (JSONB), `ip_address` (INET), `created_at`.

---

## 3. Service 1: Indexer — On-Chain Event Ingestion

**Source**: `crates/indexer/src/main.rs`, `crates/indexer/src/parser.rs`

### Startup Sequence

1. Reads `NATS_URL` and `EVM_WS_URLS` from environment.
2. Connects to NATS and creates (or gets) the `DEX_SWAPS` JetStream stream:
   - Stream name: `DEX_SWAPS`
   - Subject: `evm.dex.swaps`
   - Retention: `WorkQueue` (each message delivered to exactly one consumer, then deleted)
   - Max age: 3600 seconds (1 hour)
3. Parses `EVM_WS_URLS` as a comma-separated list. Each entry is either:
   - `chain_name=wss://...` — explicit chain name
   - `wss://...` — defaults to chain name `"ethereum"`
4. Spawns one `tokio::spawn` task per chain. Each task runs an infinite reconnection loop calling `index_chain()`.

### Per-Chain Indexing Loop (`index_chain`)

1. Opens a WebSocket connection to the EVM node using `alloy::providers::ProviderBuilder::connect_ws`.
2. Creates a log subscription filter matching two event signatures:
   - **Uniswap V2 Swap**: `d78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822`
     - Signature: `Swap(address indexed sender, uint256 amount0In, uint256 amount1In, uint256 amount0Out, uint256 amount1Out, address indexed to)`
   - **Uniswap V3 Swap**: `c42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67`
     - Signature: `Swap(address indexed sender, address indexed recipient, int256 amount0, int256 amount1, uint160 sqrtPriceX96, uint128 liquidity, int24 tick)`
3. The filter has **no address restriction** — it matches Swap events from every contract on the chain. This catches all Uniswap V2-compatible and V3-compatible DEX pools.
4. For each incoming log, calls `parser::parse_swap_log(chain_name, &log)`.

### Log Parsing (`parser.rs`)

**`parse_swap_log(chain_name, log)`** — Entry point. Checks `topic0` to determine V2 vs V3, then delegates.

**V2 Parsing (`parse_v2_swap`)**:
- Topics layout: `[event_sig, sender(indexed), to(indexed)]`
- Data layout: 4 × 32-byte words = `[amount0In, amount1In, amount0Out, amount1Out]`
- Direction detection: If `amount0In > 0`, then token0 is the input token and `amount1Out` is the output. Otherwise, `amount1In` is input and `amount0Out` is output.
- **Note**: `token_in` and `token_out` addresses are left as empty strings. The indexer does not resolve which token is token0/token1 — that mapping is deferred to the execution engine and session matching.

**V3 Parsing (`parse_v3_swap`)**:
- Topics layout: `[event_sig, sender(indexed), recipient(indexed)]`
- Data layout: 5 × 32-byte words = `[amount0(int256), amount1(int256), sqrtPriceX96, liquidity, tick]`
- Direction detection via sign bit: A positive `amount0` means token0 was sent in (input). A negative `amount0` (bit 255 set) means token0 was sent out (output). Two's complement is used to compute the absolute value of the negative amount.

**Output**: A `DexSwapEvent` struct:

```rust
DexSwapEvent {
    chain: String,          // "ethereum", "bsc", etc.
    block_number: u64,
    tx_hash: String,
    log_index: u32,
    pool_address: String,   // contract that emitted the Swap event
    dex_type: String,       // "uniswap_v2" or "uniswap_v3"
    token_in: String,       // empty — not resolved by indexer
    token_out: String,      // empty — not resolved by indexer
    amount_in: String,      // U256 decimal string
    amount_out: String,     // U256 decimal string
    sender: String,
    recipient: String,
    timestamp: u64,         // 0 — not resolved by indexer
}
```

### Publishing to NATS

The serialized `DexSwapEvent` (JSON bytes) is published to NATS subject `evm.dex.swaps` via `jetstream.publish(SUBJECT_DEX_SWAPS, payload)`. On failure, the error is logged but the indexer continues processing subsequent logs.

### Reconnection

If the WebSocket stream ends or errors, the outer loop in `main()` sleeps 5 seconds and re-enters `index_chain()`, re-establishing the provider and subscription.

---

## 4. Service 2: Execution Engine — Event Processing & Trade Execution

**Source**: `crates/execution-engine/src/main.rs`, `crates/execution-engine/src/impact.rs`, `crates/execution-engine/src/router.rs`

The execution engine runs as 2 replicas (configured via `deploy.replicas: 2` in docker-compose). Both replicas consume from the same NATS durable consumer, so messages are load-balanced across them.

### Startup Sequence

1. Connects to PostgreSQL, Redis, NATS, and two gRPC services (KMS at `KMS_GRPC_ADDR`, Session API at `SESSION_GRPC_ADDR`).
2. Gets or creates the `DEX_SWAPS` stream (same config as indexer, idempotent).
3. Creates a **durable pull consumer** named `"execution-engine"` on the `DEX_SWAPS` stream.
4. Enters a message processing loop.

### Message Processing Loop

For each NATS message:
1. **Acknowledge immediately** (at-most-once delivery semantics — the message is removed from the stream before processing completes).
2. Deserialize the payload as `DexSwapEvent`.
3. Spawn a new `tokio::spawn` task calling `process_swap_event(state, event)`.

### Core Processing Logic (`process_swap_event`)

This is the heart of the system. Each invocation handles one observed DEX swap.

**Step 1 — Query Matching Sessions**

Calls Session API via gRPC:
```
SessionServiceClient::get_active_sessions_for_token(ActiveSessionsQuery {
    chain: event.chain,              // e.g. "ethereum"
    token_address: "",               // not used when pool_address is set
    pool_address: event.pool_address // match sessions watching this specific pool
})
```
This returns all sessions where:
- `chain` matches the event's chain
- The session has a row in `session_pools` whose `pool_address` matches `event.pool_address` (case-insensitive)
- `status = 'active'`

The query uses an `INNER JOIN session_pools` to find sessions that are explicitly watching the pool where the swap occurred. This avoids the need for the indexer to resolve token addresses — the pool address is always known from the log's emitting contract.

Each returned `SessionInfo` includes its `pools` (from `session_pools` table).

If no sessions match, processing stops (returns `Ok(())`).

**Step 2 — For Each Matching Session:**

**2a. Minimum Buy Trigger Check:- TODO(add price oracle integration)**

Computes a simplified USD threshold check:
```rust
min_trigger = session.min_buy_trigger_usd × 10^18
```
If the observed `amount_out` (the buy volume) is below this threshold, skip. This prevents reacting to dust-level trades.

**2b. POV Sell Amount Calculation**

```rust
pov_bps = session.pov_percent × 100    // convert percent to basis points
sell_amount = (buy_amount × pov_bps) / 10,000
```
For example, if `pov_percent = 10.0` and someone bought 1000 tokens, sell_amount = 100 tokens.

The sell amount is capped at the session's remaining balance:
```rust
remaining = total_amount - amount_sold
sell_amount = min(sell_amount, remaining)
```
If remaining is zero, skip. TODO(add session completion logic when total_amount is fully sold).

**2c. Price Impact Calculation:- TODO(add v3 integration)**

Calls `impact::calculate_price_impact_v2(pool_address, sell_amount)`.

The current implementation uses **placeholder reserves** (1M and 500K tokens). In production, this would call the pool's `getReserves()` on-chain.

The formula for constant-product AMM (x × y = k):
```
price_impact_bps = sell_amount × 10,000 / (reserve_x + sell_amount)
```
This is the price impact in basis points (1 bps = 0.01%).

If `price_impact_bps > session.max_price_impact × 100`, the trade is skipped with a warning log.

**2d. Transaction Construction**

Calls `build_swap_calldata(pool_address, sell_amount)` — currently a **placeholder** that produces a dummy 36-byte payload (4-byte selector + 32-byte amount). Production would ABI-encode a proper DEX router `swap()` call.

**2e. Transaction Signing via KMS**

Calls KMS via gRPC:
```
WalletKmsClient::sign_transaction(SignTransactionRequest {
    wallet_id: session.wallet_id,
    unsigned_tx: tx_bytes,
    chain_id: chain_name_to_id(&session.chain)  // "ethereum" → 1, "bsc" → 56, etc.
})
```
On success, receives `SignTransactionResponse { signed_tx, tx_hash }`.

**2f. Transaction Submission**

Currently a **placeholder** — the comment indicates production would use Flashbots or MEV-Share bundles to prevent frontrunning.

**2g. Record Trade in Database**

Inserts into the `trades` table:
```sql
INSERT INTO trades (
    session_id, chain, status, trigger_tx_hash, trigger_pool,
    trigger_buy_amount, sell_amount, sell_pool, price_impact_bps, executed_at
)
VALUES ($1, $2::chain_id, 'confirmed', $3, $4, $5, $6, $7, $8, NOW())
```

Updates the session's cumulative sold amount:
```sql
UPDATE sessions SET amount_sold = amount_sold + $1 WHERE id = $2
```

**2h. Async Routing (if needed)**

Checks `needs_routing(session, event)` — returns `true` if `event.token_in` (the token received from the pool swap) does not match `session.target_token`. If routing is needed, spawns `router::route_to_target_token(db, session_id)` in a background task.

**Routing Flow** (`router.rs`):
1. Queries trades with `status = 'confirmed'` and `route_tx_hash IS NULL` for the session.
2. For each pending trade, marks it as `status = 'routing'`, `route_tx_hash = 'pending_routing'`.
3. Production would: determine the intermediate token, find the best route to the target token via a DEX aggregator, build and sign the swap, submit, await confirmation, and update the trade with the final `route_tx_hash` and `final_received`.

**2i. Publish Trade Completion to NATS**

Publishes a JSON event to subject `trades.completed`:
```json
{
    "session_id": "...",
    "chain": "ethereum",
    "sell_amount": "100000000000000000",
    "price_impact_bps": 15,
    "pool": "0x...",
    "trigger_tx": "0x..."
}
```
This is consumed by the WebSocket Service for real-time client updates.

---

## 5. Service 3: Session API — Session Management & Pool Discovery

**Source**: `crates/session-api/src/main.rs`, `crates/session-api/src/pools.rs`

A gRPC server on port 50052 implementing the `SessionService` protobuf service.

### gRPC Methods

**`CreateSession(CreateSessionRequest) → SessionInfo`**

1. Generates a UUID session ID and a 12-character random alphanumeric `public_slug`.
2. Inserts into `sessions` table with all token config, strategy params, and initial `status = 'pending'`, `amount_sold = 0`.
3. For each `PoolInfo` in the request's `pools` field, inserts into `session_pools`:
   ```sql
   INSERT INTO session_pools (session_id, pool_address, pool_type, dex_name, token0, token1, fee_tier)
   VALUES ($1, $2, $3::pool_type, $4, $5, $6, $7)
   ON CONFLICT (session_id, pool_address) DO NOTHING
   ```
4. Returns the full `SessionInfo` including the generated slug and timestamps.

**`GetSession(GetSessionRequest) → SessionInfo`**

Fetches a single session by UUID from the `sessions` table, then fetches its pools from `session_pools`. Returns the merged result.

**`ListSessions(ListSessionsRequest) → ListSessionsResponse`**

Fetches all sessions for a `user_id`, ordered by `created_at DESC`. For each session, also fetches its pools. Returns the list.

**`UpdateSessionStatus(UpdateSessionStatusRequest) → SessionInfo`**

Validates that the new status is one of `active`, `paused`, `cancelled`, or `completed`. Executes:
```sql
UPDATE sessions SET status = $1::session_status WHERE id = $2
```
Returns the updated session.

**`GetActiveSessionsForToken(ActiveSessionsQuery) → ActiveSessionsResponse`**

The hot-path query used by the Execution Engine on every swap event:
```sql
SELECT ... FROM sessions
WHERE chain = $1::chain_id
  AND sell_token = $2
  AND status = 'active'
```
Where `$1` is the chain name (e.g. "ethereum") cast to the `chain_id` enum, and `$2` is the token address. This query hits the partial index `idx_sessions_chain_sell ON sessions (chain, sell_token) WHERE status = 'active'`.

For each matching session, also fetches its pools from `session_pools` and attaches them to the response.

**`GetSwapPaths(GetSwapPathsRequest) → GetSwapPathsResponse`**

Currently returns **mock paths** — 3 hardcoded swap paths with placeholder pool addresses. Production would query on-chain DEX routers to find optimal multi-hop routes.

**`DiscoverPools(DiscoverPoolsRequest) → DiscoverPoolsResponse`**

Calls `pools::discover_pools(provider, chain, token_address)`.

### Pool Discovery (`pools.rs`)

Discovers all DEX liquidity pools containing a given token by querying factory contracts on-chain.

**Input**: An RPC provider, chain name, and token address.

**Process**:
1. Looks up pre-configured DEX factories for the chain via `common::types::dex_factories_for_chain(chain)`. Each factory has:
   - `name`: "uniswap", "sushiswap", "pancakeswap", "aerodrome", "camelot", "quickswap", "velodrome"
   - `factory_address`: the on-chain factory contract
   - `pool_type`: V2 or V3
   - `fee_tiers`: for V3 pools, the specific fee tiers to query (e.g., 100, 500, 3000, 10000 bps)

2. Gets the chain's **common base tokens** (WETH, USDC, USDT, DAI, WBTC, etc. — specific addresses per chain). For example, Ethereum has:
   - `0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2` (WETH)
   - `0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48` (USDC)
   - `0xdAC17F958D2ee523a2206206994597C13D831ec7` (USDT)
   - `0x6B175474E89094C44Da98b954EedeAC495271d0F` (DAI)
   - `0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599` (WBTC)

3. For each factory × base token pair:
   - **V2 pools**: Calls `IUniswapV2Factory::getPair(token, base_token)`. If the returned address is not `0x0`, the pool exists.
   - **V3 pools**: For each fee tier (e.g., 500, 3000), calls `IUniswapV3Factory::getPool(token, base_token, fee)`. If the returned address is not `0x0`, the pool exists.

4. Deduplicates results by pool address (case-insensitive).

5. Returns `Vec<DiscoveredPool>`, each containing: `pool_address`, `pool_type` (v2/v3), `dex_name`, `token0`, `token1`, `fee_tier`.

**Supported Chains & DEXes**:

| Chain | DEXes |
|-------|-------|
| Ethereum | Uniswap V2, Uniswap V3, SushiSwap V2 |
| Base | Uniswap V3, Aerodrome V2 |
| Arbitrum | Uniswap V3, SushiSwap V2, Camelot V2 |
| BSC | PancakeSwap V2, PancakeSwap V3 |
| Polygon | Uniswap V3, QuickSwap V2 |
| Optimism | Uniswap V3, Velodrome V2 |

---

## 6. Service 4: KMS — Key Management & Transaction Signing

**Source**: `crates/kms/src/main.rs`, `crates/kms/src/crypto.rs`

A gRPC server on port 50051 implementing the `WalletKms` protobuf service. This service holds the `MASTER_ENCRYPTION_KEY` (a 32-byte AES-256 key provided as 64 hex characters in the environment).

### gRPC Methods

**`CreateWallet(CreateWalletRequest) → CreateWalletResponse`**

1. Generates a random secp256k1 private key using `alloy::signers::local::PrivateKeySigner::random()`.
2. Extracts the Ethereum address from the key.
3. Extracts the raw private key bytes (32 bytes).
4. Encrypts the private key using AES-256-GCM:
   - Generates a random 12-byte nonce via `Aes256Gcm::generate_nonce(&mut OsRng)`
   - Encrypts: `cipher.encrypt(&nonce, plaintext)` → produces ciphertext with appended 16-byte authentication tag
5. Stores in `wallets` table: UUID, user_id, chain, address, ciphertext, nonce.
6. Returns `wallet_id` and `address`.

**Private key bytes never leave the KMS process memory in plaintext. They are encrypted before database storage and only decrypted transiently during `SignTransaction`.**

**`GetWallet(GetWalletRequest) → WalletInfo`**

Simple lookup by wallet UUID. Returns public info only (id, address, chain, created_at). Does not expose or decrypt the key.

**`ListWallets(ListWalletsRequest) → ListWalletsResponse`**

Fetches all wallets for a user, ordered by creation time. Returns public info only.

**`GetBalance(GetBalanceRequest) → GetBalanceResponse`**

Currently a **placeholder** returning `balance: "0", decimals: 18`. Production would create an alloy HTTP provider and call the ERC-20 `balanceOf()` or fetch native balance.

**`SignTransaction(SignTransactionRequest) → SignTransactionResponse`**

1. Fetches `encrypted_key` and `nonce` from the `wallets` table by `wallet_id`.
2. Decrypts the private key in memory using AES-256-GCM:
   ```rust
   crypto::decrypt_key(&self.master_key, &encrypted_key, &nonce)
   ```
3. Reconstructs the `PrivateKeySigner` from the decrypted bytes.
4. Signs the provided `unsigned_tx` bytes by treating them as a hash and calling `signer.sign_hash()`.
5. Serializes the signature as 65 bytes: `r (32 bytes) || s (32 bytes) || v (1 byte)`.
6. Returns the signature. The private key bytes are dropped when the function returns (Rust's ownership system ensures no copies persist).

### Crypto Module (`crypto.rs`)

**`encrypt_key(master_key, plaintext) → (ciphertext, nonce)`**: AES-256-GCM encryption with a random nonce. The ciphertext includes a 16-byte authentication tag appended by the AEAD.

**`decrypt_key(master_key, ciphertext, nonce) → plaintext`**: AES-256-GCM decryption. If the master key is wrong or the ciphertext has been tampered with, decryption fails with an authentication error.

---

## 7. Service 5: API Gateway — HTTP Interface & Authentication

**Source**: `crates/api-gateway/src/main.rs`, `crates/api-gateway/src/routes.rs`, `crates/api-gateway/src/auth.rs`, `crates/api-gateway/src/jwt_middleware.rs`

An Axum HTTP server on port 8080 serving the REST API.

### Authentication System

**Password Hashing**: Uses Argon2id (via the `argon2` crate) with a random salt. The hash string (including algorithm params, salt, and hash) is stored in `users.password_hash`.

**JWT Tokens**: Two token types issued:
- **Access token**: 1-hour expiry, `token_type: "access"`. Used for API authentication.
- **Refresh token**: 7-day expiry, `token_type: "refresh"`. Used to obtain new access tokens.

Both are HMAC-SHA256 JWTs signed with the `JWT_SECRET` environment variable. Claims include `sub` (user UUID), `iat`, `exp`, and `token_type`.

**JWT Middleware** (`jwt_middleware.rs`): The `require_auth` middleware extracts the `Authorization: Bearer <token>` header, validates the JWT, checks that `token_type == "access"`, parses the `sub` claim as a UUID, and injects an `AuthUser { user_id }` into request extensions. Returns 401 on any validation failure.

### Route Map

**Public (no auth)**:
- `POST /api/v1/auth/signup` — Create account. Validates email contains `@`, password 8-128 chars. Hashes password, inserts user, returns JWT pair.
- `POST /api/v1/auth/login` — Authenticate. Verifies password via Argon2. If 2FA is enabled, requires `totp_code` in the request body. Returns JWT pair.
- `POST /api/v1/auth/refresh` — Exchange refresh token for new JWT pair. Validates the refresh token, checks user still exists.
- `GET /api/v1/health` — Returns `"ok"`.

**Protected (JWT required)**:
- `POST /api/v1/auth/2fa/setup` — Generates a TOTP secret (SHA1, 6 digits, 30s period), stores it in `users.totp_secret`, returns the secret, otpauth URL, and QR code as base64.
- `POST /api/v1/auth/2fa/verify` — Verifies a TOTP code against the stored secret. On success, sets `users.totp_enabled = TRUE`.
- `POST /api/v1/wallets` — Proxies to `KMS::CreateWallet`. Body: `{ chain: "ethereum" }`.
- `GET /api/v1/wallets` — Proxies to `KMS::ListWallets`.
- `GET /api/v1/wallets/{wallet_id}/balance` — Proxies to `KMS::GetBalance`.
- `POST /api/v1/sessions` — Proxies to `SessionService::CreateSession`. Accepts full session config including optional `pools[]` array.
- `GET /api/v1/sessions` — Proxies to `SessionService::ListSessions`.
- `GET /api/v1/sessions/{session_id}` — Proxies to `SessionService::GetSession`.
- `PUT /api/v1/sessions/{session_id}/status` — Proxies to `SessionService::UpdateSessionStatus`. Validates status is one of `active`, `paused`, `cancelled`.
- `POST /api/v1/sessions/paths` — Proxies to `SessionService::GetSwapPaths`.
- `POST /api/v1/sessions/pools/discover` — Proxies to `SessionService::DiscoverPools`. Body: `{ chain, token_address }`.

All gRPC proxy calls create a new `tonic` client connection per request (connecting to the service's internal Docker hostname).

---

## 8. Service 6: WebSocket Service — Real-Time Client Updates

**Source**: `crates/websocket-service/src/main.rs`

Runs two servers simultaneously:
- **Axum WebSocket server** on port 8081 for browser clients
- **gRPC server** on port 50053 implementing `MetricsService` for internal push

### Client Connection

Two WebSocket endpoints:

**`/ws/session/{session_id}?token=JWT`** (Authenticated)
1. Validates the JWT from the `token` query parameter.
2. On upgrade, calls `handle_ws(socket, session_id, state)`.

**`/ws/public/{slug}`** (Public, no auth)
1. Looks up the session by `public_slug` in the database: `SELECT id::text FROM sessions WHERE public_slug = $1`.
2. On upgrade, calls `handle_ws(socket, session_id, state)`.

### WebSocket Handling (`handle_ws`)

1. Splits the WebSocket into sender and receiver halves.
2. Gets or creates a `broadcast::channel(256)` for the `session_id` in a `HashMap<String, broadcast::Sender<String>>` protected by `RwLock`.
3. Subscribes to the broadcast channel.
4. Spawns two tasks:
   - **Send task**: Forwards messages from the broadcast receiver to the WebSocket sender. Breaks on WebSocket send error.
   - **Receive task**: Reads from the WebSocket. Only processes `Close` messages. Client-to-server messages are otherwise ignored.
5. Uses `tokio::select!` to wait for either task to finish, then the connection cleans up.

### Data Ingestion (Two Paths)

**Path 1 — gRPC Push** (`MetricsService`):

Other services call `PushSessionUpdate` or `PushTradeCompleted` via gRPC:
- `PushSessionUpdate(SessionUpdateEvent)` — Serializes the event as JSON with `"type": "session_update"` and broadcasts to the session's channel.
- `PushTradeCompleted(TradeCompletedEvent)` — Serializes with `"type": "trade_completed"` and broadcasts.

**Path 2 — NATS Bridge** (`nats_to_ws_bridge`):

A background task consumes from the `TRADES_COMPLETED` NATS JetStream stream:
1. Creates/gets a stream named `TRADES_COMPLETED` covering subjects `trades.completed` and `session.updates`.
2. Creates a durable consumer `"ws-bridge"`.
3. For each message, extracts the `session_id` from the JSON payload and broadcasts the full JSON to the corresponding session's broadcast channel.

This means trade completion events published by the Execution Engine to NATS `trades.completed` are automatically forwarded to all connected WebSocket clients for that session.

### Message Format to Clients

Session update:
```json
{
    "type": "session_update",
    "session_id": "uuid",
    "status": "active",
    "amount_sold": "1000000",
    "remaining": "9000000",
    "converted_value_usd": "500.00"
}
```

Trade completed:
```json
{
    "type": "trade_completed",
    "trade_id": "uuid",
    "session_id": "uuid",
    "chain": "ethereum",
    "sell_amount": "100000000000000000",
    "received_amount": "50000000",
    "tx_hash": "0x...",
    "price_impact_bps": 15,
    "executed_at": "2026-04-02T12:00:00Z"
}
```

---

## 9. End-to-End Flow: User Creates a Session

This traces the exact path from user action to database state.

1. **Frontend** sends `POST /api/v1/sessions/pools/discover` with `{ chain: "ethereum", token_address: "0xTOKEN" }` and a valid JWT.

2. **API Gateway** (`routes::discover_pools`):
   - Validates JWT via middleware → extracts `user_id`.
   - Opens gRPC connection to Session API.
   - Calls `SessionServiceClient::discover_pools(DiscoverPoolsRequest { chain, token_address })`.

3. **Session API** (`SessionServiceImpl::discover_pools`):
   - Creates an alloy HTTP provider from `EVM_RPC_URL`.
   - Calls `pools::discover_pools(provider, "ethereum", "0xTOKEN")`.
   - For Ethereum, queries 3 factories (Uniswap V2, Uniswap V3, SushiSwap V2) × 5 base tokens (WETH, USDC, USDT, DAI, WBTC).
   - V2: calls `factory.getPair(token, base)` — 1 RPC call per factory-base pair.
   - V3: calls `factory.getPool(token, base, fee)` — 1 RPC call per factory-base-fee combination. Uniswap V3 has 4 fee tiers → 4 × 5 = 20 calls for that factory alone.
   - Filters out zero-address results, deduplicates, returns list of discovered pools.

4. **API Gateway** returns the pool list as JSON to the frontend.

5. **Frontend** displays pools to user. User selects pools and configures session parameters.

6. **Frontend** sends `POST /api/v1/sessions` with full config including `pools[]`.

7. **API Gateway** (`routes::create_session`):
   - Validates JWT, extracts `user_id`.
   - Converts `PoolInfoBody[]` to protobuf `PoolInfo[]`.
   - Calls `SessionServiceClient::create_session(CreateSessionRequest { ... })`.

8. **Session API** (`SessionServiceImpl::create_session`):
   - Generates UUID `session_id` and 12-char `public_slug`.
   - Inserts into `sessions` table with `status = 'pending'`.
   - For each pool, inserts into `session_pools` with `ON CONFLICT DO NOTHING`.
   - Returns `SessionInfo`.

9. **Frontend** receives the session. User activates it.

10. **Frontend** sends `PUT /api/v1/sessions/{session_id}/status` with `{ status: "active" }`.

11. **Session API** updates `sessions.status = 'active'`. The session is now eligible for trade execution — it will appear in `GetActiveSessionsForToken` queries.

---

## 10. End-to-End Flow: A DEX Swap Triggers a Trade

This traces the exact path from an on-chain event to a recorded trade.

1. **On Ethereum mainnet**: Someone swaps Token B → Token A (buying Token A) on a Uniswap V3 pool at address `0xPOOL`. The EVM emits a Swap log with `topic0 = c42079f9...`.

2. **Indexer** receives the log via WebSocket subscription:
   - `parse_v3_swap()` extracts: `sender`, `recipient`, signed `amount0` and `amount1`.
   - Determines `amount_in` and `amount_out` from sign bits.
   - Creates `DexSwapEvent { chain: "ethereum", pool_address: "0xPOOL", dex_type: "uniswap_v3", amount_in: "...", amount_out: "...", ... }`.
   - Publishes JSON to NATS subject `evm.dex.swaps`.

3. **NATS JetStream** delivers the message to exactly one Execution Engine replica (WorkQueue retention, durable consumer `"execution-engine"`).

4. **Execution Engine** (`process_swap_event`):
   - Acks the message immediately.
   - Calls Session API: `get_active_sessions_for_token({ chain: "ethereum", pool_address: "0xPOOL" })`.
   - The session-api JOINs `session_pools` to find all active sessions watching `0xPOOL`.
   - Assuming a matching active session `S` is returned:
     - Min trigger: `amount_out >= session.min_buy_trigger_usd × 10^18` (simplified)
     - Sell amount: `sell_amount = amount_out × (pov_percent × 100) / 10000`
     - Remaining check: `remaining = total_amount - amount_sold > 0`
     - Price impact: `calculate_price_impact_v2("0xPOOL", sell_amount)` → returns bps using placeholder reserves
     - If impact ≤ max: builds tx calldata (placeholder), calls KMS to sign.

5. **KMS** (`sign_transaction`):
   - Fetches encrypted private key for `session.wallet_id`.
   - Decrypts with AES-256-GCM using `MASTER_ENCRYPTION_KEY`.
   - Signs the hash with the secp256k1 key.
   - Returns 65-byte signature (r, s, v).
   - Private key is dropped from memory.

6. **Execution Engine** continues:
   - (Placeholder: submits transaction on-chain)
   - Records trade in `trades` table with `status = 'confirmed'`.
   - Updates `sessions.amount_sold += sell_amount`.
   - If routing needed (intermediate ≠ target token): spawns `router::route_to_target_token()`.
   - Publishes JSON to NATS subject `trades.completed`.

7. **NATS** delivers to the `TRADES_COMPLETED` stream.

8. **WebSocket Service** (`nats_to_ws_bridge`):
   - Consumes the message via durable consumer `"ws-bridge"`.
   - Parses `session_id` from the JSON.
   - Looks up the broadcast channel for that session.
   - Sends the JSON payload to the channel.

9. **WebSocket clients** connected to `/ws/session/{session_id}` or `/ws/public/{slug}` receive the JSON message in real time.

---

## 11. Inter-Service Communication Map

```
┌─────────────────┐
│   EVM Node(s)   │  WebSocket (wss://)
│  (Alchemy, etc) │◄─────────────────────┐
└─────────────────┘                       │
                                          │
┌─────────────────┐    NATS JetStream     │
│    Indexer       │──── evm.dex.swaps ──►│
│  (1 per chain)  │                       │
└─────────────────┘                       │
                                          │
    ┌─────────────────────────────────────┘
    │  NATS subject: evm.dex.swaps
    │  Stream: DEX_SWAPS (WorkQueue)
    ▼
┌──────────────────────┐                 ┌──────────────┐
│  Execution Engine    │── gRPC ────────►│  Session API  │
│  (2 replicas)        │  GetActive...   │  :50052       │
│                      │                 └──────────────┘
│                      │── gRPC ────────►┌──────────────┐
│                      │  SignTx         │  KMS          │
│                      │                 │  :50051       │
│                      │                 └──────────────┘
│                      │
│                      │── NATS ────────► trades.completed
└──────────────────────┘
                                          │
    ┌─────────────────────────────────────┘
    │  NATS subject: trades.completed
    │  Stream: TRADES_COMPLETED
    ▼
┌──────────────────────┐
│  WebSocket Service   │
│  :8081 (WS)          │
│  :50053 (gRPC)       │◄──── gRPC push (PushSessionUpdate, PushTradeCompleted)
│                      │
│                      │──── WebSocket ──►  Browser clients
└──────────────────────┘

┌──────────────────────┐
│  API Gateway         │── gRPC ────────► Session API :50052
│  :8080 (HTTP)        │── gRPC ────────► KMS :50051
│                      │── HTTP  ◄──────  Frontend :3000
└──────────────────────┘

┌──────────────────────┐
│  Frontend (Next.js)  │
│  :3000               │── HTTP ────────► API Gateway :8080
│                      │── WebSocket ───► WebSocket Service :8081
└──────────────────────┘

┌──────────────────────┐
│  PostgreSQL :5432    │◄── All Rust services (direct connections)
├──────────────────────┤
│  Redis :6379         │◄── API Gateway, Execution Engine
├──────────────────────┤
│  NATS :4222          │◄── Indexer, Execution Engine, WebSocket Service
└──────────────────────┘
```
