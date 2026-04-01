// ── API response types ─────────────────────────────────────────────────────

export interface User {
  id: string;
  email: string;
  totp_enabled: boolean;
}

export interface AuthResponse {
  token: string;
  user_id: string;
  totp_enabled: boolean;
}

export interface Wallet {
  id: string;
  address: string;
  label?: string;
  created_at: string;
}

export interface TokenBalance {
  token_address: string;
  balance_raw: string;
  decimals: string;
  symbol: string;
}

export type SessionStatus = "pending" | "active" | "paused" | "completed" | "failed";
export type ExecutionStrategy = "pov" | "price_impact";

export interface SwapPath {
  tokens: string[];
  pools: string[];
  fee_bps: string;
  liquidity: string;
}

export interface Session {
  id: string;
  wallet_id: string;
  chain_id: number;
  token_address: string;
  target_token_address: string;
  total_amount: string;
  amount_sold: string;
  strategy: ExecutionStrategy;
  pov_percentage?: number;
  max_price_impact_bps?: number;
  min_buy_trigger_usd: string;
  swap_paths: SwapPath[];
  status: SessionStatus;
  public_slug: string;
  created_at: string;
}

export interface CreateSessionPayload {
  wallet_id: string;
  chain_id: number;
  token_address: string;
  target_token_address: string;
  total_amount: string;
  strategy: ExecutionStrategy;
  pov_percentage?: number;
  max_price_impact_bps?: number;
  min_buy_trigger_usd?: number;
}

// ── WebSocket event types ──────────────────────────────────────────────────

export interface TradeUpdate {
  session_id: string;
  tx_hash: string;
  pool_address: string;
  amount_in: string;
  amount_out: string;
  price_impact_bps: number;
  status: string;
}

export interface SessionMetrics {
  session_id: string;
  total_amount: string;
  amount_sold: string;
  remaining: string;
  trade_count: number;
  last_trade_at?: string;
  status: SessionStatus;
}

export type WsEvent =
  | { type: "TradeUpdate"; data: TradeUpdate }
  | { type: "SessionMetrics"; data: SessionMetrics }
  | { type: "Ping" };

// ── Chart types ────────────────────────────────────────────────────────────

export interface TradeDataPoint {
  time: string;
  amountSold: number;
  priceImpactBps: number;
}
