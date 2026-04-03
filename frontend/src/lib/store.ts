import { create } from "zustand";
import type { Session, Trade, WsMessage } from "./types";

// ─────────────────────────────────────────────────────────────
// Auth Store
// ─────────────────────────────────────────────────────────────

interface AuthState {
  userId: string | null;
  isAuthenticated: boolean;
  setAuth: (userId: string) => void;
  clearAuth: () => void;
}

export const useAuthStore = create<AuthState>((set) => ({
  userId:
    typeof window !== "undefined" ? localStorage.getItem("user_id") : null,
  isAuthenticated:
    typeof window !== "undefined"
      ? !!localStorage.getItem("access_token")
      : false,
  setAuth: (userId) => {
    localStorage.setItem("user_id", userId);
    set({ userId, isAuthenticated: true });
  },
  clearAuth: () => {
    localStorage.removeItem("user_id");
    localStorage.removeItem("access_token");
    localStorage.removeItem("refresh_token");
    set({ userId: null, isAuthenticated: false });
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
          sell_amount: msg.sell_amount,
          received_amount: msg.received_amount,
          tx_hash: msg.tx_hash,
          price_impact_bps: msg.price_impact_bps,
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
