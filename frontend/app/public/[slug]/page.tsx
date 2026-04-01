"use client";

import { useParams } from "next/navigation";
import { useQuery } from "@tanstack/react-query";
import apiClient from "@/lib/api";
import { useSessionSocket } from "@/hooks/useSessionSocket";
import { formatTokenAmount, sellProgress } from "@/lib/utils";
import { cn } from "@/lib/utils";

export default function PublicSessionPage() {
  const params = useParams<{ slug: string }>();
  const slug   = params.slug;

  const { data: session, isLoading } = useQuery({
    queryKey: ["public-session", slug],
    queryFn:  () => apiClient.get(`/api/public/${slug}`).then((r) => r.data),
    enabled:  !!slug,
  });

  // Use public WebSocket endpoint
  const { metrics, tradeHistory, status } = useSessionSocket(
    session?.id ?? null,
    true
  );

  const progress = metrics
    ? sellProgress(metrics.amount_sold, metrics.total_amount)
    : session
    ? sellProgress(session.amount_sold, session.total_amount)
    : 0;

  if (isLoading) {
    return (
      <div className="flex items-center justify-center min-h-screen text-muted-foreground">
        Loading…
      </div>
    );
  }

  if (!session) {
    return (
      <div className="flex items-center justify-center min-h-screen text-muted-foreground">
        Session not found.
      </div>
    );
  }

  return (
    <div className="min-h-screen bg-background">
      <div className="max-w-xl mx-auto py-12 px-4 space-y-6">
        <div className="text-center">
          <p className="text-4xl mb-2">💧</p>
          <h1 className="text-xl font-bold text-white">Liquifier — Live Session</h1>
          <p className="text-xs text-muted-foreground font-mono mt-1">{session.id}</p>
        </div>

        <div className="rounded-xl border border-border bg-card p-6 space-y-4">
          <div className="flex justify-between">
            <span className="text-sm text-muted-foreground">Status</span>
            <span className="font-semibold capitalize text-white">{metrics?.status ?? session.status}</span>
          </div>
          <div>
            <div className="flex justify-between text-sm mb-1">
              <span className="text-muted-foreground">Progress</span>
              <span className="text-white">{progress.toFixed(2)}%</span>
            </div>
            <div className="h-3 bg-secondary rounded-full overflow-hidden">
              <div
                className="h-full bg-primary transition-all duration-700"
                style={{ width: `${progress}%` }}
              />
            </div>
          </div>
          <div className="grid grid-cols-2 gap-3 text-sm">
            <div>
              <p className="text-muted-foreground">Total</p>
              <p className="font-medium text-white">{formatTokenAmount(session.total_amount)}</p>
            </div>
            <div>
              <p className="text-muted-foreground">Sold</p>
              <p className="font-medium text-white">{formatTokenAmount(metrics?.amount_sold ?? session.amount_sold)}</p>
            </div>
            <div>
              <p className="text-muted-foreground">Remaining</p>
              <p className="font-medium text-white">
                {formatTokenAmount(
                  metrics?.remaining ??
                  (BigInt(session.total_amount) - BigInt(session.amount_sold)).toString()
                )}
              </p>
            </div>
            <div>
              <p className="text-muted-foreground">Trades</p>
              <p className="font-medium text-white">{metrics?.trade_count ?? 0}</p>
            </div>
          </div>
        </div>

        {/* Live connection indicator */}
        <div className="flex items-center justify-center gap-2 text-xs text-muted-foreground">
          <span className={cn(
            "w-2 h-2 rounded-full",
            status === "connected" ? "bg-green-500" : "bg-gray-500 animate-pulse"
          )} />
          {status === "connected" ? "Live" : "Connecting…"}
        </div>

        <p className="text-center text-xs text-muted-foreground">
          Powered by{" "}
          <a href="/" className="text-primary hover:underline">Liquifier</a>
        </p>
      </div>
    </div>
  );
}
