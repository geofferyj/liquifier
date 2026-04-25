-- Add market-cap controls and snapshots
ALTER TABLE
    sessions
ADD
    COLUMN IF NOT EXISTS min_market_cap_usd NUMERIC(30, 2) NOT NULL DEFAULT 50000000.00;

ALTER TABLE
    trades
ADD
    COLUMN IF NOT EXISTS market_cap_usd NUMERIC(30, 2);
