// ─────────────────────────────────────────────────────────────
// Core types matching the backend API
// ─────────────────────────────────────────────────────────────

export type Chain =
  | "ethereum"
  | "base"
  | "arbitrum"
  | "bsc"
  | "polygon"
  | "optimism";

export type SessionStatus =
  | "pending"
  | "active"
  | "paused"
  | "completed"
  | "cancelled"
  | "error";

export interface AuthResponse {
  access_token: string;
  refresh_token: string;
  user_id: string;
}

export type UserRole = "admin" | "common";

export type AuthStatus =
  | "authenticated"
  | "email_verification_required"
  | "totp_setup_required"
  | "totp_required";

export interface LoginResponse {
  status: AuthStatus;
  user_id: string;
  message?: string;
  access_token?: string;
  refresh_token?: string;
  role?: UserRole;
}

export interface SignupResponse {
  status: AuthStatus;
  user_id: string;
  message?: string;
  access_token?: string;
  role?: UserRole;
}

export interface Wallet {
  wallet_id: string;
  address: string;
  chain: string;
  created_at: string;
}

export interface AdminWallet extends Wallet {
  owner_id: string;
  owner_name: string;
}

export interface SwapPath {
  rank: number;
  hops: string[];
  hop_tokens: string[];
  estimated_output: string;
  estimated_price_impact: number;
  fee_percent: number;
}

export interface PoolInfo {
  pool_address: string;
  pool_type: string;
  dex_name: string;
  token0: string;
  token1: string;
  fee_tier: number;
  reserve0: string;
  reserve1: string;
  liquidity: string;
  balance0: string;
  balance1: string;
  token0_price_usd: number;
  token1_price_usd: number;
  swap_path_json: string;
}

export interface Session {
  session_id: string;
  user_id: string;
  wallet_id: string;
  chain: Chain;
  status: SessionStatus;
  sell_token: string;
  sell_token_symbol: string;
  sell_token_decimals: number;
  target_token: string;
  target_token_symbol: string;
  target_token_decimals: number;
  strategy: string;
  total_amount: string;
  amount_sold: string;
  pov_percent: number;
  max_price_impact: number;
  min_buy_trigger_usd: number;
  swap_path_json: string;
  public_slug: string;
  public_sharing_enabled: boolean;
  created_at: string;
  updated_at: string;
  pools: PoolInfo[];
}

export interface Trade {
  trade_id: string;
  session_id: string;
  chain: string;
  status?: string;
  sell_amount: string;
  received_amount: string;
  tx_hash: string;
  price_impact_bps: number;
  failure_reason?: string | null;
  executed_at: string;
}

// WebSocket message types
export type WsMessageType = "session_update" | "trade_completed";

export interface WsSessionUpdate {
  type: "session_update";
  session_id: string;
  status: SessionStatus;
  amount_sold: string;
  remaining: string;
  converted_value_usd: string;
}

export interface WsTradeCompleted {
  type: "trade_completed";
  trade_id: string;
  session_id: string;
  chain: string;
  status?: string;
  sell_amount: string;
  received_amount: string;
  tx_hash: string;
  price_impact_bps: number;
  failure_reason?: string | null;
  executed_at: string;
}

export type WsMessage = WsSessionUpdate | WsTradeCompleted;

export interface UserProfile {
  user_id: string;
  email: string;
  username?: string;
  email_verified: boolean;
  totp_enabled: boolean;
  role: UserRole;
}

export interface TotpSetupResponse {
  secret: string;
  otpauth_url: string;
  qr_code_base64: string;
}

export interface RefundRequest {
  refund_id: string;
  wallet_id: string;
  amount: string;
  token_address: string;
  token_symbol: string;
  status: string;
  admin_note?: string;
  created_at: string;
  updated_at: string;
}

export interface AdminRefundRequest extends RefundRequest {
  user_id: string;
  email: string;
  username?: string;
}

export interface AdminUser {
  user_id: string;
  email: string;
  username?: string;
  role: UserRole;
  email_verified: boolean;
  totp_enabled: boolean;
  wallet_count: number;
  session_count: number;
  created_at: string;
}

export interface AdminUserSession {
  session_id: string;
  status: string;
  sell_token_symbol: string;
  target_token_symbol: string;
  sell_token_decimals: number;
  target_token_decimals: number;
  chain: string;
  total_amount: string;
  amount_sold: string;
  pov_percent: number;
  strategy: string;
  wallet_address: string;
  public_slug?: string;
  created_at: string;
  updated_at: string;
}

export interface WalletSession {
  session_id: string;
  status: string;
  sell_token_symbol: string;
  target_token_symbol: string;
  chain: string;
  total_amount: string;
  amount_sold: string;
  pov_percent: number;
  wallet_address: string;
  public_slug?: string;
  created_at: string;
}
