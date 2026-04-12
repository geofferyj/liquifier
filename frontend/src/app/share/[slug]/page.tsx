"use client";

import { useParams } from "next/navigation";
import { useEffect } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/api";
import { useSessionSocket } from "@/hooks/useSessionSocket";
import { useLiveDataStore } from "@/lib/store";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import {
  AreaChart,
  Area,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  ResponsiveContainer,
} from "recharts";
import { cn, formatTokenAmount, formatTokenAmountCompact, shortenTxHash, tokenAmountToUsd, formatUsd } from "@/lib/utils";
import type { SessionStatus } from "@/lib/types";

export default function PublicSessionPage() {
  const params = useParams<{ slug: string }>();
  const slug = params.slug;

  // Fetch initial session data via public REST endpoint
  const sessionQuery = useQuery({
    queryKey: ["public-session", slug],
    queryFn: () => api.getSessionBySlug(slug),
    enabled: !!slug,
  });

  // Connect via public slug (no auth needed)
  const { isConnected } = useSessionSocket({
    sessionId: slug,
    publicSlug: slug,
    enabled: !!sessionQuery.data,
  });

  const session = sessionQuery.data;
  const sessionStoreKey = session?.session_id ?? slug;
  const liveData = useLiveDataStore((s) => s.sessions[sessionStoreKey]);
  const seedSession = useLiveDataStore((s) => s.seedSession);

  const sellTokenUsdPriceQuery = useQuery({
    queryKey: ["token-usd-price", session?.chain, session?.sell_token],
    queryFn: () => api.getTokenUsdPrice(session!.chain, session!.sell_token),
    enabled: !!session?.chain && !!session?.sell_token,
    staleTime: 5 * 60_000,
    refetchInterval: 60_000,
  });

  const sellTokenUsdPrice = sellTokenUsdPriceQuery.data?.usd_price;

  const sessionTradesQuery = useQuery({
    queryKey: ["public-session-trades", slug],
    queryFn: async () => {
      const resp = await api.getSessionTradesBySlug(slug, 50);
      return resp.trades;
    },
    enabled: !!session,
    refetchInterval: 15_000,
  });

  useEffect(() => {
    if (!session || !sessionTradesQuery.data) {
      return;
    }

    if ((liveData?.recentTrades.length ?? 0) > 0) {
      return;
    }

    seedSession(session.session_id, {
      amountSold: session.amount_sold,
      remaining: (BigInt(session.total_amount) - BigInt(session.amount_sold)).toString(),
      convertedValueUsd: liveData?.convertedValueUsd ?? "0.00",
      status: session.status,
      recentTrades: sessionTradesQuery.data,
    });
  }, [
    liveData?.convertedValueUsd,
    liveData?.recentTrades.length,
    seedSession,
    session,
    sessionTradesQuery.data,
  ]);

  if (sessionQuery.isLoading) {
    return (
      <main className="min-h-screen flex items-center justify-center">
        <p className="text-muted-foreground">Loading session...</p>
      </main>
    );
  }

  if (sessionQuery.isError || !session) {
    return (
      <main className="min-h-screen flex items-center justify-center">
        <div className="text-center space-y-2">
          <p className="text-destructive text-lg font-medium">Session not available</p>
          <p className="text-sm text-muted-foreground">
            This link may have been disabled or the session does not exist.
          </p>
        </div>
      </main>
    );
  }

  const status = (liveData?.status ?? session.status) as SessionStatus;
  const amountSold = liveData?.amountSold ?? session.amount_sold;
  const remaining =
    liveData?.remaining ??
    (BigInt(session.total_amount) - BigInt(session.amount_sold)).toString();
  const convertedUsd = liveData?.convertedValueUsd ?? "0.00";
  const recentTrades =
    (liveData?.recentTrades.length ?? 0) > 0
      ? liveData?.recentTrades ?? []
      : sessionTradesQuery.data ?? [];
  const chartData = recentTrades
    .slice(0, 20)
    .reverse()
    .map((t, i) => ({
      index: i + 1,
      amount: parseFloat(t.sell_amount) / Math.pow(10, session.sell_token_decimals),
      impact: t.price_impact_bps / 100,
      time: new Date(t.executed_at).toLocaleTimeString(),
    }));

  return (
    <main className="min-h-screen p-8 max-w-4xl mx-auto space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold">
            {session
              ? `${session.sell_token_symbol} → ${session.target_token_symbol}`
              : "Liquifier Session"}
          </h1>
          <p className="text-sm text-muted-foreground">
            Public view · Read-only
            {session && ` · ${session.chain}`}
          </p>
        </div>
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <div
            className={cn(
              "w-2 h-2 rounded-full",
              isConnected ? "bg-green-500" : "bg-red-500"
            )}
          />
          {isConnected ? "Live" : "Connecting..."}
        </div>
      </div>

      {/* Status badge */}
      <div className="flex items-center gap-2">
        <span
          className={cn(
            "px-2.5 py-0.5 rounded-full text-xs font-medium",
            status === "active" && "bg-green-500/10 text-green-500",
            status === "paused" && "bg-yellow-500/10 text-yellow-500",
            status === "completed" && "bg-blue-500/10 text-blue-500",
            status === "cancelled" && "bg-red-500/10 text-red-500",
            status === "pending" && "bg-muted text-muted-foreground",
            status === "error" && "bg-red-500/10 text-red-500"
          )}
        >
          {status.toUpperCase()}
        </span>
      </div>

      {/* Metric cards */}
      <div className="grid grid-cols-1 md:grid-cols-4 gap-4">
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm text-muted-foreground">
              Total to Sell
            </CardTitle>
          </CardHeader>
          <CardContent>
            <p
              className="text-xl md:text-2xl font-bold font-mono leading-tight break-all"
              title={formatTokenAmount(session.total_amount, session.sell_token_decimals)}
            >
              {formatTokenAmountCompact(session.total_amount, session.sell_token_decimals)}
            </p>
            <p className="text-xs text-muted-foreground">{session.sell_token_symbol}</p>
            <p className="text-xs text-muted-foreground">{formatUsd(sellTokenUsdPrice ? tokenAmountToUsd(session.total_amount, session.sell_token_decimals, sellTokenUsdPrice) : null)}</p>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm text-muted-foreground">
              Amount Sold
            </CardTitle>
          </CardHeader>
          <CardContent>
            <p
              className="text-xl md:text-2xl font-bold font-mono text-primary leading-tight break-all"
              title={formatTokenAmount(amountSold, session.sell_token_decimals)}
            >
              {formatTokenAmountCompact(amountSold, session.sell_token_decimals)}
            </p>
            <p className="text-xs text-muted-foreground">{session.sell_token_symbol}</p>
            <p className="text-xs text-muted-foreground">{formatUsd(sellTokenUsdPrice ? tokenAmountToUsd(amountSold, session.sell_token_decimals, sellTokenUsdPrice) : null)}</p>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm text-muted-foreground">
              Remaining
            </CardTitle>
          </CardHeader>
          <CardContent>
            <p
              className="text-xl md:text-2xl font-bold font-mono leading-tight break-all"
              title={formatTokenAmount(remaining, session.sell_token_decimals)}
            >
              {formatTokenAmountCompact(remaining, session.sell_token_decimals)}
            </p>
            <p className="text-xs text-muted-foreground">{session.sell_token_symbol}</p>
            <p className="text-xs text-muted-foreground">{formatUsd(sellTokenUsdPrice ? tokenAmountToUsd(remaining, session.sell_token_decimals, sellTokenUsdPrice) : null)}</p>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm text-muted-foreground">
              Converted (USD)
            </CardTitle>
          </CardHeader>
          <CardContent>
            <p
              className="text-xl md:text-2xl font-bold font-mono text-primary leading-tight break-all"
              title={`$${convertedUsd}`}
            >
              ${convertedUsd}
            </p>
            <p className="text-xs text-muted-foreground">{session.target_token_symbol}</p>
          </CardContent>
        </Card>
      </div>

      {/* Live chart */}
      <Card>
        <CardHeader>
          <CardTitle>Live Trade Volume</CardTitle>
        </CardHeader>
        <CardContent>
          {chartData.length === 0 ? (
            <div className="h-48 flex items-center justify-center text-muted-foreground text-sm">
              Waiting for live data...
            </div>
          ) : (
            <ResponsiveContainer width="100%" height={250}>
              <AreaChart data={chartData}>
                <CartesianGrid
                  strokeDasharray="3 3"
                  stroke="hsl(var(--border))"
                />
                <XAxis
                  dataKey="time"
                  tick={{ fontSize: 10 }}
                  stroke="hsl(var(--muted-foreground))"
                />
                <YAxis
                  tick={{ fontSize: 10 }}
                  stroke="hsl(var(--muted-foreground))"
                />
                <Tooltip
                  contentStyle={{
                    backgroundColor: "hsl(var(--card))",
                    border: "1px solid hsl(var(--border))",
                    borderRadius: "8px",
                  }}
                />
                <Area
                  type="monotone"
                  dataKey="amount"
                  stroke="hsl(var(--primary))"
                  fill="hsl(var(--primary))"
                  fillOpacity={0.1}
                  name="Trade Amount"
                />
              </AreaChart>
            </ResponsiveContainer>
          )}
        </CardContent>
      </Card>

      {/* Recent trades */}
      <Card>
        <CardHeader>
          <CardTitle>Recent Trades</CardTitle>
        </CardHeader>
        <CardContent>
          {recentTrades.length === 0 ? (
            <p className="text-sm text-muted-foreground py-4 text-center">
              No trades yet.
            </p>
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full text-sm">
                <thead>
                  <tr className="border-b border-border text-muted-foreground">
                    <th className="text-left py-2 font-medium">Time</th>
                    <th className="text-right py-2 font-medium">Amount</th>
                    <th className="text-right py-2 font-medium">Impact</th>
                    <th className="text-right py-2 font-medium">Tx</th>
                  </tr>
                </thead>
                <tbody>
                  {recentTrades.slice(0, 10).map((trade) => (
                    <tr
                      key={trade.trade_id}
                      className="border-b border-border/50"
                    >
                      <td className="py-2">
                        {new Date(trade.executed_at).toLocaleTimeString()}
                      </td>
                      <td className="text-right font-mono">
                        <span>{formatTokenAmount(trade.sell_amount, session.sell_token_decimals)}</span>
                        {sellTokenUsdPrice && (
                          <p className="text-xs text-muted-foreground">{formatUsd(tokenAmountToUsd(trade.sell_amount, session.sell_token_decimals, sellTokenUsdPrice))}</p>
                        )}
                      </td>
                      <td className="text-right font-mono">
                        {(trade.price_impact_bps / 100).toFixed(2)}%
                      </td>
                      <td className="text-right">
                        <code className="text-xs text-muted-foreground">
                          {shortenTxHash(trade.tx_hash)}
                        </code>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </CardContent>
      </Card>
    </main>
  );
}
