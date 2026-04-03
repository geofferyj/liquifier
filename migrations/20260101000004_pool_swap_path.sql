-- Add per-pool swap path: the route from sell_token through this pool to target_token
ALTER TABLE
    session_pools
ADD
    COLUMN swap_path JSONB;