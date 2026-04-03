"use client";

import { useParams } from "next/navigation";
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
import { cn, formatTokenAmount, shortenTxHash } from "@/lib/utils";

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
    enabled: !!slug,
  });

  const liveData = useLiveDataStore((s) => {
    const entries = Object.entries(s.sessions);
    return entries.length > 0 ? entries[0][1] : null;
  });

  const session = sessionQuery.data;

  const recentTrades = liveData?.recentTrades ?? [];
  const chartData = recentTrades
    .slice(0, 20)
    .reverse()
    .map((t, i) => ({
      index: i + 1,
      amount: parseFloat(t.sell_amount) / 1e18,
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

      {/* Metric cards */}
      <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm text-muted-foreground">
              Amount Sold
            </CardTitle>
          </CardHeader>
          <CardContent>
            <p className="text-2xl font-bold font-mono text-primary">
              {liveData?.amountSold ?? "—"}
            </p>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm text-muted-foreground">
              Remaining
            </CardTitle>
          </CardHeader>
          <CardContent>
            <p className="text-2xl font-bold font-mono">
              {liveData?.remaining ?? "—"}
            </p>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm text-muted-foreground">
              Converted (USD)
            </CardTitle>
          </CardHeader>
          <CardContent>
            <p className="text-2xl font-bold font-mono text-primary">
              ${liveData?.convertedValueUsd ?? "0.00"}
            </p>
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
                        {trade.sell_amount}
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
