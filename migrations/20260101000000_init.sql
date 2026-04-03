-- ============================================================
-- Liquifier – PostgreSQL Schema
-- ============================================================
-- Extensions
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ────────────────────────────────────────────────────────────
-- ENUM Types
-- ────────────────────────────────────────────────────────────
CREATE TYPE chain_id AS ENUM (
    'ethereum',
    -- 1
    'base',
    -- 8453
    'arbitrum',
    -- 42161
    'bsc',
    -- 56
    'polygon',
    -- 137
    'optimism' -- 10
);

CREATE TYPE session_status AS ENUM (
    'pending',
    'active',
    'paused',
    'completed',
    'cancelled',
    'error'
);

CREATE TYPE trade_status AS ENUM (
    'submitted',
    'confirmed',
    'failed',
    'routing' -- intermediate token acquired, routing to final
);

CREATE TYPE strategy_type AS ENUM ('pov' -- Percentage of Volume
);

-- ────────────────────────────────────────────────────────────
-- Users
-- ────────────────────────────────────────────────────────────
CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    email TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    -- argon2id
    totp_secret BYTEA,
    -- encrypted TOTP seed, NULL = 2FA not set
    totp_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_users_email ON users (email);

-- ────────────────────────────────────────────────────────────
-- Wallets (owned by the KMS, keys never leave that service)
-- ────────────────────────────────────────────────────────────
CREATE TABLE wallets (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    chain chain_id NOT NULL,
    address TEXT NOT NULL,
    -- checksummed 0x…
    encrypted_key BYTEA NOT NULL,
    -- AES-256-GCM ciphertext
    nonce BYTEA NOT NULL,
    -- 12-byte GCM nonce
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (chain, address)
);

CREATE INDEX idx_wallets_user ON wallets (user_id);

CREATE INDEX idx_wallets_address ON wallets (chain, address);

-- ────────────────────────────────────────────────────────────
-- Sessions (a.k.a. Liquifier campaigns)
-- ────────────────────────────────────────────────────────────
CREATE TABLE sessions (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    wallet_id UUID NOT NULL REFERENCES wallets(id),
    chain chain_id NOT NULL,
    status session_status NOT NULL DEFAULT 'pending',
    -- Token config
    sell_token TEXT NOT NULL,
    -- ERC-20 contract address
    sell_token_symbol TEXT NOT NULL,
    sell_token_decimals INT NOT NULL DEFAULT 18,
    target_token TEXT NOT NULL,
    -- desired output token address
    target_token_symbol TEXT NOT NULL,
    target_token_decimals INT NOT NULL DEFAULT 18,
    -- Strategy config
    strategy strategy_type NOT NULL DEFAULT 'pov',
    total_amount NUMERIC(78, 0) NOT NULL,
    -- raw amount (U256 range)
    amount_sold NUMERIC(78, 0) NOT NULL DEFAULT 0,
    pov_percent NUMERIC(5, 2) NOT NULL DEFAULT 10.00,
    -- e.g. 10.00 %
    max_price_impact NUMERIC(5, 2) NOT NULL DEFAULT 1.00,
    -- e.g. 1.00 %
    min_buy_trigger_usd NUMERIC(18, 2) NOT NULL DEFAULT 100.00,
    -- Chosen path (JSON array of pool hops)
    swap_path JSONB,
    -- Public sharing
    public_slug TEXT UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_sessions_user ON sessions (user_id);

CREATE INDEX idx_sessions_status ON sessions (status)
WHERE
    status = 'active';

CREATE INDEX idx_sessions_chain_sell ON sessions (chain, sell_token)
WHERE
    status = 'active';

CREATE INDEX idx_sessions_slug ON sessions (public_slug)
WHERE
    public_slug IS NOT NULL;

-- ────────────────────────────────────────────────────────────
-- Trades (individual executions within a session)
-- ────────────────────────────────────────────────────────────
CREATE TABLE trades (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    session_id UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    chain chain_id NOT NULL,
    status trade_status NOT NULL DEFAULT 'submitted',
    -- Trigger
    trigger_tx_hash TEXT NOT NULL,
    -- the buy tx that triggered this trade
    trigger_pool TEXT NOT NULL,
    -- pool address where buy occurred
    trigger_buy_amount NUMERIC(78, 0) NOT NULL,
    -- Execution
    sell_amount NUMERIC(78, 0) NOT NULL,
    sell_tx_hash TEXT,
    sell_pool TEXT NOT NULL,
    -- same pool as trigger
    price_impact_bps INT,
    -- basis points
    -- Routing (if intermediate swap needed)
    route_tx_hash TEXT,
    final_received NUMERIC(78, 0),
    final_token TEXT,
    -- Gas
    gas_used BIGINT,
    gas_price_gwei NUMERIC(18, 9),
    executed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_trades_session ON trades (session_id);

CREATE INDEX idx_trades_status ON trades (status);

-- ────────────────────────────────────────────────────────────
-- Audit log (append-only, security events)
-- ────────────────────────────────────────────────────────────
CREATE TABLE audit_log (
    id BIGSERIAL PRIMARY KEY,
    user_id UUID REFERENCES users(id),
    action TEXT NOT NULL,
    metadata JSONB,
    ip_address INET,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_audit_user ON audit_log (user_id, created_at DESC);

-- ────────────────────────────────────────────────────────────
-- Trigger: auto-update updated_at columns
-- ────────────────────────────────────────────────────────────
CREATE
OR REPLACE FUNCTION trigger_set_updated_at() RETURNS TRIGGER AS $$ BEGIN NEW.updated_at = NOW();

RETURN NEW;

END;

$$ LANGUAGE plpgsql;

CREATE TRIGGER set_users_updated_at BEFORE
UPDATE
    ON users FOR EACH ROW EXECUTE FUNCTION trigger_set_updated_at();

CREATE TRIGGER set_sessions_updated_at BEFORE
UPDATE
    ON sessions FOR EACH ROW EXECUTE FUNCTION trigger_set_updated_at();