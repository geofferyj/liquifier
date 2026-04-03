-- ────────────────────────────────────────────────────────────
-- Session Pools: stores discovered DEX pools per session
-- ────────────────────────────────────────────────────────────
CREATE TYPE pool_type AS ENUM ('v2', 'v3');

CREATE TABLE session_pools (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    session_id UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    pool_address TEXT NOT NULL,
    -- checksummed 0x…
    pool_type pool_type NOT NULL,
    -- v2 or v3
    dex_name TEXT NOT NULL,
    -- e.g. "uniswap", "sushiswap", "pancakeswap"
    token0 TEXT NOT NULL,
    -- first token in pool pair
    token1 TEXT NOT NULL,
    -- second token in pool pair
    fee_tier INT,
    -- v3 fee tier in bps (e.g. 500, 3000, 10000), NULL for v2
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_session_pools_session ON session_pools (session_id);

CREATE INDEX idx_session_pools_address ON session_pools (pool_address);