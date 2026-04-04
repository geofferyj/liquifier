-- Add trigger-level idempotency key and failure diagnostics for trade lifecycle
ALTER TABLE
    trades
ADD
    COLUMN IF NOT EXISTS trigger_log_index INT NOT NULL DEFAULT 0;

ALTER TABLE
    trades
ADD
    COLUMN IF NOT EXISTS failure_reason TEXT;

-- Legacy rows can contain duplicate trigger keys from pre-idempotency retries.
-- Keep the best candidate per key, preferring confirmed rows and most recent execution.
WITH ranked AS (
    SELECT
        id,
        session_id,
        trigger_tx_hash,
        trigger_pool,
        trigger_log_index,
        ROW_NUMBER() OVER (
            PARTITION BY session_id,
            trigger_tx_hash,
            trigger_pool,
            trigger_log_index
            ORDER BY
                CASE
                    status :: text
                    WHEN 'confirmed' THEN 0
                    WHEN 'submitted' THEN 1
                    WHEN 'routing' THEN 2
                    WHEN 'failed' THEN 3
                    ELSE 4
                END,
                COALESCE(executed_at, created_at) DESC,
                created_at DESC,
                id DESC
        ) AS rn
    FROM
        trades
)
DELETE FROM
    trades t USING ranked r
WHERE
    t.id = r.id
    AND r.rn > 1;

-- Ensure session aggregates remain consistent after de-duplication.
UPDATE
    sessions s
SET
    amount_sold = COALESCE(agg.total_sold, 0)
FROM
    (
        SELECT
            session_id,
            COALESCE(SUM(sell_amount), 0) AS total_sold
        FROM
            trades
        GROUP BY
            session_id
    ) agg
WHERE
    s.id = agg.session_id;

UPDATE
    sessions
SET
    amount_sold = 0
WHERE
    id NOT IN (
        SELECT
            DISTINCT session_id
        FROM
            trades
    );

CREATE UNIQUE INDEX IF NOT EXISTS uq_trades_session_trigger ON trades (
    session_id,
    trigger_tx_hash,
    trigger_pool,
    trigger_log_index
);