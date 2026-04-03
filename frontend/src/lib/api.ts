import type {
  AuthResponse,
  PoolInfo,
  Session,
  SwapPath,
  TotpSetupResponse,
  UserProfile,
  Wallet,
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
      throw new Error(`API error: ${res.status}`);
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

  async signup(email: string, password: string): Promise<AuthResponse> {
    const data = await this.request<AuthResponse>("/api/v1/auth/signup", {
      method: "POST",
      body: JSON.stringify({ email, password }),
    });
    this.setTokens(data.access_token, data.refresh_token);
    return data;
  }

  async login(
    email: string,
    password: string,
    totpCode?: string,
  ): Promise<AuthResponse> {
    const data = await this.request<AuthResponse>("/api/v1/auth/login", {
      method: "POST",
      body: JSON.stringify({ email, password, totp_code: totpCode }),
    });
    this.setTokens(data.access_token, data.refresh_token);
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
  ): Promise<{ private_key: string; address: string }> {
    return this.request(`/api/v1/wallets/${walletId}/export`, {
      method: "POST",
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

  // ── Session Config / Sharing ───────────────────────────────

  async updateSessionConfig(
    sessionId: string,
    config: {
      pov_percent?: number;
      max_price_impact?: number;
      min_buy_trigger_usd?: number;
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

  async getWalletBalance(
    walletId: string,
    tokenAddress?: string,
  ): Promise<{ balance: string; decimals: number }> {
    const params = tokenAddress ? `?token_address=${tokenAddress}` : "";
    return this.request(`/api/v1/wallets/${walletId}/balance${params}`);
  }
}

export const api = new ApiClient();
