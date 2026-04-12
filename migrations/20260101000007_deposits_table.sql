-- ============================================================
-- Migration 007: Deposits table for tracking user deposits
-- ============================================================
CREATE TABLE deposits (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    wallet_id UUID NOT NULL REFERENCES wallets(id) ON DELETE CASCADE,
    chain TEXT NOT NULL,
    token_address TEXT NOT NULL,
    from_address TEXT NOT NULL,
    amount TEXT NOT NULL,
    -- raw token amount (U256 string)
    tx_hash TEXT NOT NULL,
    block_number BIGINT NOT NULL,
    log_index INT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (chain, tx_hash, log_index)
);

CREATE INDEX idx_deposits_user ON deposits (user_id);

CREATE INDEX idx_deposits_wallet ON deposits (wallet_id);

CREATE INDEX idx_deposits_created ON deposits (user_id, created_at DESC);