-- ============================================================
-- Migration 006: User roles (admin/common) + refund requests
-- ============================================================
-- User role enum
CREATE TYPE user_role AS ENUM ('admin', 'common');

-- Add role column (existing users become admin)
ALTER TABLE
    users
ADD
    COLUMN IF NOT EXISTS role user_role NOT NULL DEFAULT 'admin';

-- Add username column for common users
ALTER TABLE
    users
ADD
    COLUMN IF NOT EXISTS username TEXT UNIQUE;

-- Refund requests table
CREATE TABLE refund_requests (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    wallet_id UUID NOT NULL REFERENCES wallets(id) ON DELETE CASCADE,
    amount TEXT NOT NULL,
    -- raw token amount requested
    token_address TEXT NOT NULL,
    -- ERC-20 contract address
    token_symbol TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    -- pending, approved, rejected, completed
    admin_note TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_refund_requests_user ON refund_requests (user_id);

CREATE INDEX idx_refund_requests_status ON refund_requests (status);