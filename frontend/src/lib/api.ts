import type {
  AuthResponse,
  LoginResponse,
  SignupResponse,
  PoolInfo,
  Session,
  Trade,
  SessionTradesResponse,
  SwapPath,
  TotpSetupResponse,
  UserProfile,
  Wallet,
  RefundRequest,
  AdminRefundRequest,
  AdminUser,
  WalletSession,
  TokenUsdPrice,
  Deposit,
  PlatformConfig,
} from "./types";

const API_URL = process.env.NEXT_PUBLIC_API_URL ?? "http://localhost:8080";

class ApiClient {
  private accessToken: string | null = null;
  private refreshToken: string | null = null;

  constructor() {
    if (typeof window !== "undefined") {
      this.accessToken = localStorage.getItem("access_token");
      this.refreshToken = localStorage.getItem("refresh_token");
    }
  }

  private setTokens(access: string, refresh: string) {
    this.accessToken = access;
    this.refreshToken = refresh;
    localStorage.setItem("access_token", access);
    localStorage.setItem("refresh_token", refresh);
  }

  clearTokens() {
    this.accessToken = null;
    this.refreshToken = null;
    localStorage.removeItem("access_token");
    localStorage.removeItem("refresh_token");
  }

  getAccessToken(): string | null {
    return this.accessToken;
  }

  private async request<T>(
    path: string,
    options: RequestInit = {},
  ): Promise<T> {
    const headers: Record<string, string> = {
      "Content-Type": "application/json",
      ...(options.headers as Record<string, string>),
    };

    if (this.accessToken) {
      headers["Authorization"] = `Bearer ${this.accessToken}`;
    }

    const res = await fetch(`${API_URL}${path}`, {
      ...options,
      headers,
    });

    if (res.status === 401 && this.refreshToken) {
      // Try refresh
      const refreshed = await this.refreshAccessToken();
      if (refreshed) {
        headers["Authorization"] = `Bearer ${this.accessToken}`;
        const retry = await fetch(`${API_URL}${path}`, {
          ...options,
          headers,
        });
        if (!retry.ok) throw new Error(`API error: ${retry.status}`);
        return retry.json();
      }
    }

    if (!res.ok) {
      let message = `API error: ${res.status}`;
      try {
        const body = await res.clone().json();
        if (body?.message) message = body.message;
        else if (body?.error) message = body.error;
      } catch {
        // ignore parse failures
      }
      const err = new Error(message) as Error & { status: number };
      err.status = res.status;
      throw err;
    }

    return res.json();
  }

  private async refreshAccessToken(): Promise<boolean> {
    try {
      const res = await fetch(`${API_URL}/api/v1/auth/refresh`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ refresh_token: this.refreshToken }),
      });
      if (!res.ok) {
        this.clearTokens();
        return false;
      }
      const data: AuthResponse = await res.json();
      this.setTokens(data.access_token, data.refresh_token);
      return true;
    } catch {
      this.clearTokens();
      return false;
    }
  }

  // ── Auth ───────────────────────────────────────────────────

  async signup(
    email: string,
    password: string,
    role: string = "common",
    username?: string,
  ): Promise<SignupResponse> {
    const data = await this.request<SignupResponse>("/api/v1/auth/signup", {
      method: "POST",
      body: JSON.stringify({ email, password, role, username }),
    });
    if (data.access_token) {
      this.accessToken = data.access_token;
      localStorage.setItem("access_token", data.access_token);
    }
    if (data.role) {
      localStorage.setItem("user_role", data.role);
    }
    return data;
  }

  async login(
    email: string,
    password: string,
    totpCode?: string,
  ): Promise<LoginResponse> {
    const data = await this.request<LoginResponse>("/api/v1/auth/login", {
      method: "POST",
      body: JSON.stringify({ email, password, totp_code: totpCode }),
    });
    if (data.access_token) {
      this.accessToken = data.access_token;
      localStorage.setItem("access_token", data.access_token);
    }
    if (data.refresh_token) {
      this.refreshToken = data.refresh_token;
      localStorage.setItem("refresh_token", data.refresh_token);
    }
    if (data.role) {
      localStorage.setItem("user_role", data.role);
    }
    return data;
  }

  async setup2fa(): Promise<TotpSetupResponse> {
    return this.request("/api/v1/auth/2fa/setup", { method: "POST" });
  }

  async verify2fa(code: string): Promise<{ verified: boolean }> {
    return this.request("/api/v1/auth/2fa/verify", {
      method: "POST",
      body: JSON.stringify({ code }),
    });
  }

  async resendVerification(): Promise<{ message: string }> {
    return this.request("/api/v1/auth/resend-verification", { method: "POST" });
  }

  async verifyEmail(token: string): Promise<{ message: string }> {
    return this.request(
      `/api/v1/auth/verify-email?token=${encodeURIComponent(token)}`,
    );
  }

  async getProfile(): Promise<UserProfile> {
    return this.request("/api/v1/profile");
  }

  // ── Chains ─────────────────────────────────────────────────

  async listChains(): Promise<{
    chains: { name: string; chain_id: number }[];
  }> {
    return this.request("/api/v1/chains");
  }

  // ── Wallets ────────────────────────────────────────────────

  async createWallet(): Promise<{ wallet_id: string; address: string }> {
    return this.request("/api/v1/wallets", {
      method: "POST",
      body: JSON.stringify({}),
    });
  }

  async listWallets(): Promise<{ wallets: Wallet[] }> {
    return this.request("/api/v1/wallets");
  }

  async exportWallet(
    walletId: string,
    totpCode: string,
  ): Promise<{ private_key: string; address: string }> {
    return this.request(`/api/v1/wallets/${walletId}/export`, {
      method: "POST",
      body: JSON.stringify({ totp_code: totpCode }),
    });
  }

  // ── Sessions ───────────────────────────────────────────────

  async createSession(params: {
    wallet_id: string;
    chain: string;
    sell_token: string;
    sell_token_symbol: string;
    sell_token_decimals: number;
    target_token: string;
    target_token_symbol: string;
    target_token_decimals: number;
    total_amount: string;
    pov_percent: number;
    max_price_impact: number;
    min_buy_trigger_usd: number;
    min_market_cap_usd: number;
    swap_path_json?: string;
    pools?: PoolInfo[];
  }): Promise<Session> {
    return this.request("/api/v1/sessions", {
      method: "POST",
      body: JSON.stringify(params),
    });
  }

  async listSessions(): Promise<{ sessions: Session[] }> {
    return this.request("/api/v1/sessions");
  }

  async getSession(sessionId: string): Promise<Session> {
    return this.request(`/api/v1/sessions/${sessionId}`);
  }

  async getSessionTrades(
    sessionId: string,
    limit = 50,
  ): Promise<SessionTradesResponse> {
    return this.request(
      `/api/v1/sessions/${sessionId}/trades?limit=${encodeURIComponent(limit)}`,
    );
  }

  async updateSessionStatus(
    sessionId: string,
    status: string,
  ): Promise<Session> {
    return this.request(`/api/v1/sessions/${sessionId}/status`, {
      method: "PUT",
      body: JSON.stringify({ status }),
    });
  }

  async getSwapPaths(params: {
    chain: string;
    sell_token: string;
    target_token: string;
    amount: string;
  }): Promise<{ paths: SwapPath[] }> {
    return this.request("/api/v1/sessions/paths", {
      method: "POST",
      body: JSON.stringify(params),
    });
  }

  async discoverPools(params: {
    chain: string;
    token_address: string;
  }): Promise<{ pools: PoolInfo[] }> {
    return this.request("/api/v1/sessions/pools/discover", {
      method: "POST",
      body: JSON.stringify(params),
    });
  }

  async computePoolPath(params: {
    chain: string;
    sell_token: string;
    target_token: string;
    pool_address: string;
    pool_type: string;
    token0: string;
    token1: string;
    fee_tier: number;
  }): Promise<{ path: SwapPath | null }> {
    return this.request("/api/v1/sessions/pools/path", {
      method: "POST",
      body: JSON.stringify(params),
    });
  }

  // ── Session Config / Sharing ───────────────────────────────

  async updateSessionConfig(
    sessionId: string,
    config: {
      pov_percent?: number;
      max_price_impact?: number;
      min_buy_trigger_usd?: number;
      min_market_cap_usd?: number;
    },
  ): Promise<Session> {
    return this.request(`/api/v1/sessions/${sessionId}/config`, {
      method: "PUT",
      body: JSON.stringify(config),
    });
  }

  async togglePublicSharing(
    sessionId: string,
    enabled: boolean,
  ): Promise<Session> {
    return this.request(`/api/v1/sessions/${sessionId}/sharing`, {
      method: "PUT",
      body: JSON.stringify({ enabled }),
    });
  }

  async getSessionBySlug(slug: string): Promise<Session> {
    return this.request(`/api/v1/public/${slug}`);
  }

  async getSessionTradesBySlug(
    slug: string,
    limit = 50,
  ): Promise<SessionTradesResponse> {
    return this.request(
      `/api/v1/public/${slug}/trades?limit=${encodeURIComponent(limit)}`,
    );
  }

  async getTokenMetadata(
    chain: string,
    address: string,
  ): Promise<{
    address: string;
    name: string;
    symbol: string;
    decimals: number;
  }> {
    return this.request(
      `/api/v1/tokens/metadata?chain=${encodeURIComponent(chain)}&address=${encodeURIComponent(address)}`,
    );
  }

  async getTokenUsdPrice(
    chain: string,
    tokenAddress: string,
  ): Promise<TokenUsdPrice> {
    return this.request(
      `/api/v1/tokens/usd-price?chain=${encodeURIComponent(chain)}&token_address=${encodeURIComponent(tokenAddress)}`,
    );
  }

  async getWalletBalance(
    walletId: string,
    tokenAddress?: string,
  ): Promise<{ balance: string; decimals: number }> {
    const params = tokenAddress
      ? `?token_address=${encodeURIComponent(tokenAddress)}`
      : "";
    return this.request(`/api/v1/wallets/${walletId}/balance${params}`);
  }

  // ── Refund Requests ────────────────────────────────────────

  async createRefundRequest(params: {
    amount_usd: number;
    destination_wallet: string;
  }): Promise<{ refund_id: string; status: string; message: string }> {
    return this.request("/api/v1/refunds", {
      method: "POST",
      body: JSON.stringify(params),
    });
  }

  async verifyRefund(token: string): Promise<{ message: string }> {
    return this.request(
      `/api/v1/refunds/verify?token=${encodeURIComponent(token)}`,
    );
  }

  async listMyRefundRequests(): Promise<{ refunds: RefundRequest[] }> {
    return this.request("/api/v1/refunds");
  }

  // ── Common User: My Wallet Sessions ────────────────────────

  async listMyWalletSessions(): Promise<{ sessions: WalletSession[] }> {
    return this.request("/api/v1/my/sessions");
  }

  // ── Common User: Deposit History ───────────────────────────

  async listMyDeposits(): Promise<{ deposits: Deposit[] }> {
    return this.request("/api/v1/my/deposits");
  }

  // ── Common User: Start Selling ─────────────────────────────

  async startSelling(): Promise<Session> {
    return this.request("/api/v1/my/start-selling", { method: "POST" });
  }

  // ── Platform Config ────────────────────────────────────────

  async getPlatformConfig(): Promise<PlatformConfig> {
    return this.request("/api/v1/config");
  }

  // ── Admin Routes ───────────────────────────────────────────

  async adminListUsers(): Promise<{ users: AdminUser[] }> {
    return this.request("/api/v1/admin/users");
  }

  async adminGetUserWallets(userId: string): Promise<{ wallets: Wallet[] }> {
    return this.request(`/api/v1/admin/users/${userId}/wallets`);
  }

  async adminGetUserSessions(
    userId: string,
  ): Promise<{ sessions: import("./types").AdminUserSession[] }> {
    return this.request(`/api/v1/admin/users/${userId}/sessions`);
  }

  async adminExportUserWallet(
    userId: string,
    walletId: string,
    totpCode: string,
  ): Promise<{ private_key: string; address: string }> {
    return this.request(
      `/api/v1/admin/users/${userId}/wallets/${walletId}/export`,
      {
        method: "POST",
        body: JSON.stringify({ totp_code: totpCode }),
      },
    );
  }

  async adminListRefundRequests(): Promise<{
    refunds: AdminRefundRequest[];
  }> {
    return this.request("/api/v1/admin/refunds");
  }

  async adminUpdateRefundStatus(
    refundId: string,
    status: string,
    adminNote?: string,
  ): Promise<{ refund_id: string; status: string }> {
    return this.request(`/api/v1/admin/refunds/${refundId}`, {
      method: "PUT",
      body: JSON.stringify({ status, admin_note: adminNote }),
    });
  }

  async adminUpdateUserRole(
    userId: string,
    role: "admin" | "common",
  ): Promise<{ user_id: string; role: string }> {
    return this.request(`/api/v1/admin/users/${userId}/role`, {
      method: "PUT",
      body: JSON.stringify({ role }),
    });
  }

  async adminListAllWallets(): Promise<{
    wallets: import("./types").AdminWallet[];
  }> {
    return this.request("/api/v1/admin/wallets");
  }

  async adminWithdrawSession(
    sessionId: string,
    params: {
      destination_wallet: string;
      amount?: string; // raw wei amount; omit to withdraw all
      totp_code: string;
    },
  ): Promise<{
    tx_hash: string;
    amount: string;
    destination_wallet: string;
    token_address: string;
  }> {
    return this.request(`/api/v1/admin/sessions/${sessionId}/withdraw`, {
      method: "POST",
      body: JSON.stringify(params),
    });
  }
}

export const api = new ApiClient();
