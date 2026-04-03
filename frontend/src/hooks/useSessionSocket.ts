import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "@/lib/api";
import { useLiveDataStore } from "@/lib/store";
import type { SessionStatus, WsMessage } from "@/lib/types";

const WS_URL = process.env.NEXT_PUBLIC_WS_URL ?? "ws://localhost:8081";

interface UseSessionSocketOptions {
  sessionId: string;
  /** Use public slug instead of authenticated session ID */
  publicSlug?: string;
  enabled?: boolean;
}

interface UseSessionSocketReturn {
  isConnected: boolean;
  error: string | null;
  reconnect: () => void;
}

const SESSION_STATUSES: SessionStatus[] = [
  "pending",
  "active",
  "paused",
  "completed",
  "cancelled",
  "error",
];

function coerceSessionStatus(value: unknown): SessionStatus {
  if (
    typeof value === "string" &&
    SESSION_STATUSES.includes(value as SessionStatus)
  ) {
    return value as SessionStatus;
  }
  return "active";
}

function normalizeWsMessage(raw: unknown): WsMessage | null {
  if (!raw || typeof raw !== "object") {
    return null;
  }

  const value = raw as Record<string, unknown>;

  // Native websocket-session-update payload.
  if (
    value.type === "session_update" &&
    typeof value.session_id === "string" &&
    typeof value.amount_sold === "string" &&
    typeof value.remaining === "string"
  ) {
    return {
      type: "session_update",
      session_id: value.session_id,
      status: coerceSessionStatus(value.status),
      amount_sold: value.amount_sold,
      remaining: value.remaining,
      converted_value_usd:
        typeof value.converted_value_usd === "string"
          ? value.converted_value_usd
          : "0.00",
    };
  }

  // Native websocket-trade-completed payload.
  if (
    value.type === "trade_completed" &&
    typeof value.session_id === "string" &&
    typeof value.sell_amount === "string"
  ) {
    const parsedImpact =
      typeof value.price_impact_bps === "number"
        ? value.price_impact_bps
        : Number.parseInt(String(value.price_impact_bps ?? "0"), 10);

    return {
      type: "trade_completed",
      trade_id:
        typeof value.trade_id === "string"
          ? value.trade_id
          : `${value.session_id}-${Date.now()}`,
      session_id: value.session_id,
      chain: typeof value.chain === "string" ? value.chain : "unknown",
      status: typeof value.status === "string" ? value.status : undefined,
      sell_amount: value.sell_amount,
      received_amount:
        typeof value.received_amount === "string" ? value.received_amount : "0",
      tx_hash: typeof value.tx_hash === "string" ? value.tx_hash : "",
      price_impact_bps: Number.isFinite(parsedImpact) ? parsedImpact : 0,
      failure_reason:
        typeof value.failure_reason === "string"
          ? value.failure_reason
          : value.failure_reason === null
            ? null
            : undefined,
      executed_at:
        typeof value.executed_at === "string"
          ? value.executed_at
          : new Date().toISOString(),
    };
  }

  // Legacy NATS trade payload emitted by execution-engine.
  if (
    typeof value.session_id === "string" &&
    typeof value.sell_amount === "string" &&
    (typeof value.price_impact_bps === "number" ||
      typeof value.price_impact_bps === "string")
  ) {
    const parsedImpact =
      typeof value.price_impact_bps === "number"
        ? value.price_impact_bps
        : Number.parseInt(value.price_impact_bps, 10);

    return {
      type: "trade_completed",
      trade_id:
        typeof value.trade_id === "string"
          ? value.trade_id
          : `${value.session_id}-${Date.now()}`,
      session_id: value.session_id,
      chain: typeof value.chain === "string" ? value.chain : "unknown",
      status: typeof value.status === "string" ? value.status : undefined,
      sell_amount: value.sell_amount,
      received_amount:
        typeof value.received_amount === "string" ? value.received_amount : "0",
      tx_hash:
        typeof value.tx_hash === "string"
          ? value.tx_hash
          : typeof value.trigger_tx === "string"
            ? value.trigger_tx
            : "",
      price_impact_bps: Number.isFinite(parsedImpact) ? parsedImpact : 0,
      failure_reason:
        typeof value.failure_reason === "string"
          ? value.failure_reason
          : value.failure_reason === null
            ? null
            : undefined,
      executed_at:
        typeof value.executed_at === "string"
          ? value.executed_at
          : new Date().toISOString(),
    };
  }

  // Legacy session update payload without explicit type.
  if (
    typeof value.session_id === "string" &&
    typeof value.amount_sold === "string" &&
    typeof value.remaining === "string" &&
    typeof value.status === "string"
  ) {
    return {
      type: "session_update",
      session_id: value.session_id,
      status: coerceSessionStatus(value.status),
      amount_sold: value.amount_sold,
      remaining: value.remaining,
      converted_value_usd:
        typeof value.converted_value_usd === "string"
          ? value.converted_value_usd
          : "0.00",
    };
  }

  return null;
}

/**
 * WebSocket hook for real-time session metrics.
 *
 * Connects to the WebSocket/Metrics Service and pushes incoming
 * messages into the Zustand live data store.
 */
export function useSessionSocket({
  sessionId,
  publicSlug,
  enabled = true,
}: UseSessionSocketOptions): UseSessionSocketReturn {
  const [isConnected, setIsConnected] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const wsRef = useRef<WebSocket | null>(null);
  const reconnectTimeoutRef = useRef<ReturnType<typeof setTimeout>>(undefined);
  const reconnectAttempts = useRef(0);
  const updateSession = useLiveDataStore((s) => s.updateSession);

  const connect = useCallback(() => {
    // Close existing connection
    if (wsRef.current) {
      wsRef.current.close();
      wsRef.current = null;
    }

    let url: string;
    if (publicSlug) {
      url = `${WS_URL}/ws/public/${publicSlug}`;
    } else {
      const token = api.getAccessToken();
      if (!token) {
        setError("Not authenticated");
        return;
      }
      url = `${WS_URL}/ws/session/${sessionId}?token=${encodeURIComponent(token)}`;
    }

    const ws = new WebSocket(url);
    wsRef.current = ws;

    ws.onopen = () => {
      setIsConnected(true);
      setError(null);
      reconnectAttempts.current = 0;
    };

    ws.onmessage = (event) => {
      try {
        const msg = normalizeWsMessage(JSON.parse(event.data));
        if (!msg) {
          return;
        }
        updateSession(msg);
      } catch {
        // Ignore malformed messages
      }
    };

    ws.onerror = () => {
      setError("WebSocket connection error");
    };

    ws.onclose = (event) => {
      setIsConnected(false);
      wsRef.current = null;

      // Reconnect with exponential backoff (max 30s)
      if (enabled && !event.wasClean) {
        const delay = Math.min(
          1000 * Math.pow(2, reconnectAttempts.current),
          30_000,
        );
        reconnectAttempts.current++;
        reconnectTimeoutRef.current = setTimeout(connect, delay);
      }
    };
  }, [sessionId, publicSlug, enabled, updateSession]);

  useEffect(() => {
    if (!enabled) return;
    connect();

    return () => {
      if (reconnectTimeoutRef.current) {
        clearTimeout(reconnectTimeoutRef.current);
      }
      if (wsRef.current) {
        wsRef.current.close(1000, "Component unmounted");
        wsRef.current = null;
      }
    };
  }, [connect, enabled]);

  const reconnect = useCallback(() => {
    reconnectAttempts.current = 0;
    connect();
  }, [connect]);

  return { isConnected, error, reconnect };
}
