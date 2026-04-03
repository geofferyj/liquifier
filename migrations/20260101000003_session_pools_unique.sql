ALTER TABLE
    session_pools
ADD
    CONSTRAINT uq_session_pools_session_pool UNIQUE (session_id, pool_address);