-- ============================================================
-- Migration 003: Email verification + public sharing toggle
-- ============================================================
-- Email verification columns
ALTER TABLE
    users
ADD
    COLUMN IF NOT EXISTS email_verified BOOLEAN NOT NULL DEFAULT FALSE;

ALTER TABLE
    users
ADD
    COLUMN IF NOT EXISTS verification_token TEXT UNIQUE;

ALTER TABLE
    users
ADD
    COLUMN IF NOT EXISTS verification_token_expires_at TIMESTAMPTZ;

-- Public sharing toggle
ALTER TABLE
    sessions
ADD
    COLUMN IF NOT EXISTS public_sharing_enabled BOOLEAN NOT NULL DEFAULT TRUE;