-- Add trigger-level idempotency key and failure diagnostics for trade lifecycle
ALTER TABLE
    trades
ADD
    COLUMN IF NOT EXISTS trigger_log_index INT NOT NULL DEFAULT 0;

ALTER TABLE
    trades
ADD
    COLUMN IF NOT EXISTS failure_reason TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS uq_trades_session_trigger ON trades (
    session_id,
    trigger_tx_hash,
    trigger_pool,
    trigger_log_index
);