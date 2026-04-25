-- Set default minimum market cap to 50M USD for newly created sessions
ALTER TABLE sessions
    ALTER COLUMN min_market_cap_usd SET DEFAULT 50000000.00;
