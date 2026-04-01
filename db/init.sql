-- ============================================================
-- Liquifier PostgreSQL Schema
-- ============================================================

CREATE EXTENSION IF NOT EXISTS "pgcrypto";
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

-- ── Users ────────────────────────────────────────────────────

CREATE TABLE users (
    id            UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    email         TEXT        UNIQUE NOT NULL,
    password_hash TEXT        NOT NULL,   -- argon2id hash
    totp_secret   TEXT,                   -- NULL until 2FA is enrolled
    totp_enabled  BOOLEAN     NOT NULL DEFAULT FALSE,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_users_email ON users (email);

-- ── Wallets ──────────────────────────────────────────────────

-- Each user may have multiple KMS-managed EVM wallets.
-- Private key material is stored ONLY in the KMS service
-- (encrypted with AES-256-GCM + master env key).
CREATE TABLE wallets (
    id                  UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id             UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    address             TEXT        NOT NULL UNIQUE,     -- 0x… checksummed
    encrypted_key       BYTEA       NOT NULL,            -- AES-256-GCM ciphertext
    key_nonce           BYTEA       NOT NULL,            -- 12-byte GCM nonce
    label               TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_wallets_user_id  ON wallets (user_id);
CREATE INDEX idx_wallets_address  ON wallets (address);

-- ── Sessions ─────────────────────────────────────────────────

CREATE TYPE session_status AS ENUM ('pending', 'active', 'paused', 'completed', 'failed');
CREATE TYPE execution_strategy AS ENUM ('pov', 'price_impact');

CREATE TABLE sessions (
    id                     UUID              PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id                UUID              NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    wallet_id              UUID              NOT NULL REFERENCES wallets(id),
    public_slug            TEXT              UNIQUE NOT NULL DEFAULT encode(gen_random_bytes(12), 'hex'),

    -- Token configuration
    chain_id               BIGINT            NOT NULL,
    token_address          TEXT              NOT NULL,   -- token to sell
    target_token_address   TEXT              NOT NULL,   -- desired output token
    total_amount           NUMERIC(78, 0)    NOT NULL,   -- raw U256 as numeric
    amount_sold            NUMERIC(78, 0)    NOT NULL DEFAULT 0,

    -- Execution strategy
    strategy               execution_strategy NOT NULL DEFAULT 'pov',
    pov_percentage         NUMERIC(5, 2),               -- 0.01 – 100.00 %
    max_price_impact_bps   INT,                         -- basis points, e.g. 50 = 0.5 %
    min_buy_trigger_usd    NUMERIC(18, 6)    NOT NULL DEFAULT 100.0,

    -- Top-5 swap paths stored as JSONB array
    swap_paths             JSONB             NOT NULL DEFAULT '[]',

    status                 session_status    NOT NULL DEFAULT 'pending',
    created_at             TIMESTAMPTZ       NOT NULL DEFAULT NOW(),
    updated_at             TIMESTAMPTZ       NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_sessions_user_id     ON sessions (user_id);
CREATE INDEX idx_sessions_status      ON sessions (status);
CREATE INDEX idx_sessions_public_slug ON sessions (public_slug);
CREATE INDEX idx_sessions_chain_token ON sessions (chain_id, token_address);

-- ── Trades ───────────────────────────────────────────────────

CREATE TYPE trade_status AS ENUM ('pending', 'submitted', 'confirmed', 'failed');

CREATE TABLE trades (
    id                   UUID         PRIMARY KEY DEFAULT uuid_generate_v4(),
    session_id           UUID         NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,

    -- Trigger information (the buy event that caused this sell)
    trigger_tx_hash      TEXT,
    trigger_pool_address TEXT,
    trigger_buy_amount   NUMERIC(78, 0),

    -- Our execution
    tx_hash              TEXT,
    pool_address         TEXT         NOT NULL,
    amount_in            NUMERIC(78, 0) NOT NULL,
    amount_out           NUMERIC(78, 0),
    intermediate_token   TEXT,        -- non-NULL when multi-hop routing is in progress
    final_token          TEXT         NOT NULL,

    price_impact_bps     INT,
    gas_used             BIGINT,
    gas_price_wei        NUMERIC(78, 0),

    status               trade_status NOT NULL DEFAULT 'pending',
    error_message        TEXT,

    submitted_at         TIMESTAMPTZ,
    confirmed_at         TIMESTAMPTZ,
    created_at           TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_trades_session_id  ON trades (session_id);
CREATE INDEX idx_trades_tx_hash     ON trades (tx_hash);
CREATE INDEX idx_trades_status      ON trades (status);

-- ── Trigger: auto-update updated_at ─────────────────────────

CREATE OR REPLACE FUNCTION set_updated_at()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$;

CREATE TRIGGER users_updated_at
    BEFORE UPDATE ON users
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

CREATE TRIGGER sessions_updated_at
    BEFORE UPDATE ON sessions
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
