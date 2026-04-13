-- ============================================================
-- Migration 008: Refund requests — USD amount, destination wallet, email verification
-- ============================================================
-- Amount in USD (user-facing value)
ALTER TABLE
    refund_requests
ADD
    COLUMN IF NOT EXISTS amount_usd NUMERIC(15, 2);

-- External wallet address where refund should be sent
ALTER TABLE
    refund_requests
ADD
    COLUMN IF NOT EXISTS destination_wallet TEXT;

-- Email verification token + verified flag
ALTER TABLE
    refund_requests
ADD
    COLUMN IF NOT EXISTS verification_token TEXT;

ALTER TABLE
    refund_requests
ADD
    COLUMN IF NOT EXISTS verification_token_expires_at TIMESTAMPTZ;

ALTER TABLE
    refund_requests
ADD
    COLUMN IF NOT EXISTS verified BOOLEAN NOT NULL DEFAULT FALSE;