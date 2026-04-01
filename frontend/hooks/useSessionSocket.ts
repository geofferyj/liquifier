"use client";

import { useEffect, useRef, useState, useCallback } from "react";
import type { SessionMetrics, TradeUpdate, WsEvent } from "@/types";

const WS_URL = process.env.NEXT_PUBLIC_WS_URL ?? "ws://localhost:8081";
const RECONNECT_DELAY_MS = 3_000;
const MAX_TRADE_HISTORY  = 50;

export type ConnectionStatus = "connecting" | "connected" | "disconnected" | "error";

export interface UseSessionSocketReturn {
  metrics:       SessionMetrics | null;
  tradeHistory:  TradeUpdate[];
  status:        ConnectionStatus;
  sendMessage:   (msg: Record<string, unknown>) => void;
}

/**
 * useSessionSocket — connects to the Liquifier Real-Time Metrics Service
 * and streams live session metrics and trade updates.
 *
 * @param sessionId  UUID of the session to watch (or null to disconnect)
 * @param isPublic   When true, connects to the public read-only endpoint
 */
export function useSessionSocket(
  sessionId: string | null,
  isPublic = false
): UseSessionSocketReturn {
  const [metrics,      setMetrics]      = useState<SessionMetrics | null>(null);
  const [tradeHistory, setTradeHistory] = useState<TradeUpdate[]>([]);
  const [status,       setStatus]       = useState<ConnectionStatus>("disconnected");

  const wsRef         = useRef<WebSocket | null>(null);
  const reconnectRef  = useRef<ReturnType<typeof setTimeout> | null>(null);
  const sessionIdRef  = useRef(sessionId);

  sessionIdRef.current = sessionId;

  const cleanup = useCallback(() => {
    if (reconnectRef.current) {
      clearTimeout(reconnectRef.current);
      reconnectRef.current = null;
    }
    if (wsRef.current) {
      wsRef.current.onclose   = null;
      wsRef.current.onerror   = null;
      wsRef.current.onmessage = null;
      wsRef.current.close();
      wsRef.current = null;
    }
  }, []);

  const connect = useCallback(() => {
    const id = sessionIdRef.current;
    if (!id) return;

    cleanup();
    setStatus("connecting");

    const path = isPublic ? `/ws/public/${id}` : `/ws/${id}`;
    const url  = `${WS_URL}${path}`;

    const ws = new WebSocket(url);
    wsRef.current = ws;

    ws.onopen = () => {
      setStatus("connected");
    };

    ws.onmessage = (event: MessageEvent<string>) => {
      try {
        const parsed: WsEvent = JSON.parse(event.data);

        switch (parsed.type) {
          case "SessionMetrics":
            setMetrics(parsed.data);
            break;

          case "TradeUpdate":
            setTradeHistory((prev) => {
              const next = [parsed.data, ...prev];
              return next.slice(0, MAX_TRADE_HISTORY);
            });
            break;

          case "Ping":
            // keepalive — no UI update needed
            break;

          default:
            // exhaustive check helper
            break;
        }
      } catch (err) {
        console.error("[useSessionSocket] Failed to parse WS message:", err);
      }
    };

    ws.onerror = () => {
      setStatus("error");
    };

    ws.onclose = () => {
      setStatus("disconnected");
      // Reconnect after delay if the session ID hasn't changed
      reconnectRef.current = setTimeout(() => {
        if (sessionIdRef.current === id) {
          connect();
        }
      }, RECONNECT_DELAY_MS);
    };
  }, [cleanup, isPublic]);

  useEffect(() => {
    if (sessionId) {
      connect();
    } else {
      cleanup();
      setStatus("disconnected");
    }

    return cleanup;
  }, [sessionId, connect, cleanup]);

  const sendMessage = useCallback((msg: Record<string, unknown>) => {
    if (wsRef.current?.readyState === WebSocket.OPEN) {
      wsRef.current.send(JSON.stringify(msg));
    }
  }, []);

  return { metrics, tradeHistory, status, sendMessage };
}
