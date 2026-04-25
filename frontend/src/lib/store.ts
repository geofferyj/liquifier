import { create } from "zustand";
import type { Session, Trade, WsMessage, UserRole } from "./types";

// ─────────────────────────────────────────────────────────────
// Auth Store
// ─────────────────────────────────────────────────────────────

interface AuthState {
  userId: string | null;
  role: UserRole | null;
  isAuthenticated: boolean;
  hydrated: boolean;
  setAuth: (userId: string, role?: UserRole) => void;
  clearAuth: () => void;
  hydrate: () => void;
}

export const useAuthStore = create<AuthState>((set) => ({
  userId: null,
  role: null,
  isAuthenticated: false,
  hydrated: false,
  setAuth: (userId, role) => {
    localStorage.setItem("user_id", userId);
    if (role) localStorage.setItem("user_role", role);
    set({
      userId,
      role: role ?? (localStorage.getItem("user_role") as UserRole | null),
      isAuthenticated: true,
    });
  },
  clearAuth: () => {
    localStorage.removeItem("user_id");
    localStorage.removeItem("access_token");
    localStorage.removeItem("refresh_token");
    localStorage.removeItem("user_role");
    set({ userId: null, role: null, isAuthenticated: false });
  },
  hydrate: () => {
    const userId = localStorage.getItem("user_id");
    const role = localStorage.getItem("user_role") as UserRole | null;
    const isAuthenticated = !!localStorage.getItem("access_token");
    set({ userId, role, isAuthenticated, hydrated: true });
  },
}));

// ─────────────────────────────────────────────────────────────
// Session Live Data Store (fed by WebSocket)
// ─────────────────────────────────────────────────────────────

interface SessionLiveData {
  amountSold: string;
  remaining: string;
  convertedValueUsd: string;
  status: string;
  recentTrades: Trade[];
}

interface LiveDataState {
  sessions: Record<string, SessionLiveData>;
  updateSession: (msg: WsMessage) => void;
  seedSession: (sessionId: string, data: Partial<SessionLiveData>) => void;
  reset: () => void;
}

export const useLiveDataStore = create<LiveDataState>((set) => ({
  sessions: {},
  updateSession: (msg) =>
    set((state) => {
      const sessions = { ...state.sessions };

      if (msg.type === "session_update") {
        sessions[msg.session_id] = {
          ...sessions[msg.session_id],
          amountSold: msg.amount_sold,
          remaining: msg.remaining,
          convertedValueUsd: msg.converted_value_usd,
          status: msg.status,
          recentTrades: sessions[msg.session_id]?.recentTrades ?? [],
        };
      }

      if (msg.type === "trade_completed") {
        const existing = sessions[msg.session_id] ?? {
          amountSold: "0",
          remaining: "0",
          convertedValueUsd: "0",
          status: "active",
          recentTrades: [],
        };

        const trade: Trade = {
          trade_id: msg.trade_id,
          session_id: msg.session_id,
          chain: msg.chain,
          status: msg.status,
          sell_amount: msg.sell_amount,
          received_amount: msg.received_amount,
          tx_hash: msg.tx_hash,
          price_impact_bps: msg.price_impact_bps,
          market_cap_usd: msg.market_cap_usd,
          failure_reason: msg.failure_reason,
          executed_at: msg.executed_at,
        };

        sessions[msg.session_id] = {
          ...existing,
          recentTrades: [trade, ...existing.recentTrades].slice(0, 50),
        };
      }

      return { sessions };
    }),
  seedSession: (sessionId, data) =>
    set((state) => {
      const existing = state.sessions[sessionId] ?? {
        amountSold: "0",
        remaining: "0",
        convertedValueUsd: "0.00",
        status: "pending",
        recentTrades: [],
      };

      return {
        sessions: {
          ...state.sessions,
          [sessionId]: {
            amountSold: data.amountSold ?? existing.amountSold,
            remaining: data.remaining ?? existing.remaining,
            convertedValueUsd:
              data.convertedValueUsd ?? existing.convertedValueUsd,
            status: data.status ?? existing.status,
            recentTrades: data.recentTrades ?? existing.recentTrades,
          },
        },
      };
    }),
  reset: () => set({ sessions: {} }),
}));
