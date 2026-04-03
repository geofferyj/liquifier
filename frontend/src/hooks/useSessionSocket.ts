import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "@/lib/api";
import { useLiveDataStore } from "@/lib/store";
import type { WsMessage } from "@/lib/types";

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
        const msg: WsMessage = JSON.parse(event.data);
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
