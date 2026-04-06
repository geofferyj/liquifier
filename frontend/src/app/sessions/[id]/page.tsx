"use client";

import { useParams, useRouter } from "next/navigation";
import { useEffect, useState } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { useAuthStore } from "@/lib/store";
import {
  AreaChart,
  Area,
  BarChart,
  Bar,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  ResponsiveContainer,
} from "recharts";
import { api } from "@/lib/api";
import { useSessionSocket } from "@/hooks/useSessionSocket";
import { useLiveDataStore } from "@/lib/store";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import {
  formatTokenAmount,
  shortenAddress,
  shortenTxHash,
  cn,
} from "@/lib/utils";
import type { Session, SessionStatus } from "@/lib/types";

function formatTokenAmountCompact(raw: string, decimals: number): string {
  if (!raw || raw === "0") return "0";

  try {
    const amountRaw = BigInt(raw);
    const divisor = 10n ** BigInt(decimals);
    const units = [
      { threshold: 1_000_000_000_000n, suffix: "T" },
      { threshold: 1_000_000_000n, suffix: "B" },
      { threshold: 1_000_000n, suffix: "M" },
      { threshold: 1_000n, suffix: "K" },
    ];

    for (const unit of units) {
      const unitDivisor = divisor * unit.threshold;
      if (amountRaw >= unitDivisor) {
        const scaledX100 = (amountRaw * 100n) / unitDivisor;
        const intPart = scaledX100 / 100n;
        const fracPart = scaledX100 % 100n;
        return `${intPart.toString()}.${fracPart.toString().padStart(2, "0")}${unit.suffix}`;
      }
    }
  } catch {
    // Fallback to full formatting below.
  }

  return formatTokenAmount(raw, decimals);
}

function rawTokenAmountToNumber(raw: string, decimals: number): number {
  const parsed = Number.parseFloat(formatTokenAmount(raw, decimals));
  return Number.isFinite(parsed) ? parsed : 0;
}

function calculateUsdValue(
  rawAmount: string,
  decimals: number,
  usdPrice?: number,
): number | null {
  if (usdPrice === undefined || !Number.isFinite(usdPrice)) {
    return null;
  }
  return rawTokenAmountToNumber(rawAmount, decimals) * usdPrice;
}

function formatUsd(value: number | null): string {
  if (value === null || !Number.isFinite(value)) {
    return "USD —";
  }
  return `$${value.toLocaleString(undefined, {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  })}`;
}

export default function SessionDashboardPage() {
  const router = useRouter();
  const role = useAuthStore((s) => s.role);
  const params = useParams<{ id: string }>();
  const sessionId = params.id;
  const queryClient = useQueryClient();

  useEffect(() => {
    if (role && role !== "admin") router.replace("/my-dashboard");
  }, [role, router]);

  // Fetch session data
  const sessionQuery = useQuery({
    queryKey: ["session", sessionId],
    queryFn: () => api.getSession(sessionId),
    refetchInterval: 15_000,
  });

  // WebSocket for live data
  const { isConnected, error: wsError, reconnect } = useSessionSocket({
    sessionId,
    enabled: !!sessionId,
  });

  // Live data from Zustand store
  const liveData = useLiveDataStore((s) => s.sessions[sessionId]);
  const seedSession = useLiveDataStore((s) => s.seedSession);

  // Fetch recent trades for first-load hydration.
  const sessionTradesQuery = useQuery({
    queryKey: ["session-trades", sessionId],
    queryFn: () => api.getSessionTrades(sessionId, 50),
    enabled: !!sessionId,
    refetchInterval: 15_000,
  });

  // Status mutation
  const statusMutation = useMutation({
    mutationFn: (status: string) =>
      api.updateSessionStatus(sessionId, status),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["session", sessionId] });
    },
  });

  // Config editing
  const [editingConfig, setEditingConfig] = useState(false);
  const [editPov, setEditPov] = useState(0);
  const [editMaxImpact, setEditMaxImpact] = useState(0);
  const [editMinTrigger, setEditMinTrigger] = useState(0);

  const configMutation = useMutation({
    mutationFn: (config: { pov_percent: number; max_price_impact: number; min_buy_trigger_usd: number }) =>
      api.updateSessionConfig(sessionId, config),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["session", sessionId] });
      setEditingConfig(false);
    },
  });

  const sharingMutation = useMutation({
    mutationFn: (enabled: boolean) => api.togglePublicSharing(sessionId, enabled),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["session", sessionId] });
    },
  });

  const session = sessionQuery.data;

  const sellTokenUsdPriceQuery = useQuery({
    queryKey: ["token-usd-price", session?.chain, session?.sell_token],
    queryFn: () => api.getTokenUsdPrice(session!.chain, session!.sell_token),
    enabled: !!session?.chain && !!session?.sell_token,
    staleTime: 5 * 60_000,
    refetchInterval: 60_000,
  });

  const targetTokenUsdPriceQuery = useQuery({
    queryKey: ["token-usd-price", session?.chain, session?.target_token],
    queryFn: () => api.getTokenUsdPrice(session!.chain, session!.target_token),
    enabled: !!session?.chain && !!session?.target_token,
    staleTime: 5 * 60_000,
    refetchInterval: 60_000,
  });

  // Fetch the wallet's current target-token balance.
  const targetBalanceQuery = useQuery({
    queryKey: ["wallet-target-balance", session?.wallet_id, session?.target_token],
    queryFn: () => api.getWalletBalance(session!.wallet_id, session!.target_token),
    enabled: !!session?.wallet_id && !!session?.target_token,
    refetchInterval: 30_000,
  });

  useEffect(() => {
    if (!session || !sessionId || !sessionTradesQuery.data) {
      return;
    }

    if ((liveData?.recentTrades.length ?? 0) > 0) {
      return;
    }

    seedSession(sessionId, {
      amountSold: session.amount_sold,
      remaining: (BigInt(session.total_amount) - BigInt(session.amount_sold)).toString(),
      convertedValueUsd: liveData?.convertedValueUsd ?? "0.00",
      status: session.status,
      recentTrades: sessionTradesQuery.data.trades,
    });
  }, [
    liveData?.convertedValueUsd,
    liveData?.recentTrades.length,
    seedSession,
    session,
    sessionId,
    sessionTradesQuery.data,
  ]);

  if (sessionQuery.isLoading) {
    return (
      <main className="min-h-screen flex items-center justify-center">
        <p className="text-muted-foreground">Loading session...</p>
      </main>
    );
  }
  if (!session) {
    return (
      <main className="min-h-screen flex items-center justify-center">
        <p className="text-destructive">Session not found</p>
      </main>
    );
  }

  const status = (liveData?.status ?? session.status) as SessionStatus;
  const amountSold = liveData?.amountSold ?? session.amount_sold;
  const remaining =
    liveData?.remaining ??
    (
      BigInt(session.total_amount) - BigInt(session.amount_sold)
    ).toString();
  const convertedUsd = liveData?.convertedValueUsd ?? "0.00";
  const recentTrades =
    (liveData?.recentTrades.length ?? 0) > 0
      ? liveData?.recentTrades ?? []
      : sessionTradesQuery.data?.trades ?? [];

  const walletTargetBalance = targetBalanceQuery.data?.balance ?? "0";

  const sellTokenUsdPrice = sellTokenUsdPriceQuery.data?.usd_price;
  const targetTokenUsdPrice = targetTokenUsdPriceQuery.data?.usd_price;

  const totalToSellUsd = calculateUsdValue(
    session.total_amount,
    session.sell_token_decimals,
    sellTokenUsdPrice,
  );
  const amountSoldUsd = calculateUsdValue(
    amountSold,
    session.sell_token_decimals,
    sellTokenUsdPrice,
  );
  const remainingUsd = calculateUsdValue(
    remaining,
    session.sell_token_decimals,
    sellTokenUsdPrice,
  );
  const walletBalanceUsd = calculateUsdValue(
    walletTargetBalance,
    session.target_token_decimals,
    targetTokenUsdPrice,
  );

  // Build chart data from recent trades
  const chartData = recentTrades
    .slice(0, 20)
    .reverse()
    .map((t, i) => ({
      index: i + 1,
      amount: rawTokenAmountToNumber(t.sell_amount, session.sell_token_decimals),
      impact_bps: t.price_impact_bps,
      time: new Date(t.executed_at).toLocaleTimeString(),
    }));

  const progress =
    session.total_amount !== "0"
      ? (Number(BigInt(amountSold) * 10000n / BigInt(session.total_amount)) / 100)
      : 0;

  return (
    <main className="min-h-screen p-8 max-w-6xl mx-auto space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold">
            {session.sell_token_symbol} → {session.target_token_symbol}
          </h1>
          <p className="text-sm text-muted-foreground">
            {session.chain} · Session {shortenAddress(session.session_id)}
          </p>
        </div>

        <div className="flex items-center gap-3">
          {/* Connection status indicator */}
          <div className="flex items-center gap-2 text-xs text-muted-foreground">
            <div
              className={cn(
                "w-2 h-2 rounded-full",
                isConnected ? "bg-green-500" : "bg-red-500"
              )}
            />
            {isConnected ? "Live" : "Disconnected"}
            {wsError && (
              <button onClick={reconnect} className="text-primary underline">
                Retry
              </button>
            )}
          </div>

          {/* Play / Pause / Stop controls */}
          {status === "pending" && (
            <Button
              onClick={() => statusMutation.mutate("active")}
              disabled={statusMutation.isPending}
            >
              Start
            </Button>
          )}
          {status === "active" && (
            <Button
              variant="secondary"
              onClick={() => statusMutation.mutate("paused")}
              disabled={statusMutation.isPending}
            >
              Pause
            </Button>
          )}
          {status === "paused" && (
            <Button
              onClick={() => statusMutation.mutate("active")}
              disabled={statusMutation.isPending}
            >
              Resume
            </Button>
          )}
          {(status === "active" || status === "paused") && (
            <Button
              variant="destructive"
              onClick={() => statusMutation.mutate("cancelled")}
              disabled={statusMutation.isPending}
            >
              Cancel
            </Button>
          )}
        </div>
      </div>

      {/* Status badge + progress */}
      <div className="space-y-2">
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
          <span className="text-sm text-muted-foreground">
            {progress.toFixed(2)}% complete
          </span>
        </div>
        <div className="w-full h-2 bg-secondary rounded-full overflow-hidden">
          <div
            className="h-full bg-primary rounded-full transition-all duration-500"
            style={{ width: `${Math.min(progress, 100)}%` }}
          />
        </div>
      </div>

      {/* Metric Cards */}
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
            <p className="text-xs text-muted-foreground">
              {session.sell_token_symbol}
            </p>
            <p className="text-xs text-muted-foreground">{formatUsd(totalToSellUsd)}</p>
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
            <p className="text-xs text-muted-foreground">
              {session.sell_token_symbol}
            </p>
            <p className="text-xs text-muted-foreground">{formatUsd(amountSoldUsd)}</p>
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
            <p className="text-xs text-muted-foreground">
              {session.sell_token_symbol}
            </p>
            <p className="text-xs text-muted-foreground">{formatUsd(remainingUsd)}</p>
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm text-muted-foreground">
              Wallet Balance
            </CardTitle>
          </CardHeader>
          <CardContent>
            <p
              className="text-xl md:text-2xl font-bold font-mono text-primary leading-tight break-all"
              title={formatTokenAmount(walletTargetBalance, session.target_token_decimals)}
            >
              {formatTokenAmountCompact(walletTargetBalance, session.target_token_decimals)}
            </p>
            <p className="text-xs text-muted-foreground">
              {session.target_token_symbol}
            </p>
            <p className="text-xs text-muted-foreground">{formatUsd(walletBalanceUsd)}</p>
          </CardContent>
        </Card>
      </div>

      {/* Charts */}
      <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
        {/* Trade Volume Chart */}
        <Card>
          <CardHeader>
            <CardTitle>Trade Volume</CardTitle>
          </CardHeader>
          <CardContent>
            {chartData.length === 0 ? (
              <div className="h-48 flex items-center justify-center text-muted-foreground text-sm">
                Waiting for trades...
              </div>
            ) : (
              <ResponsiveContainer width="100%" height={200}>
                <AreaChart data={chartData}>
                  <CartesianGrid strokeDasharray="3 3" stroke="hsl(var(--border))" />
                  <XAxis dataKey="time" tick={{ fontSize: 10 }} stroke="hsl(var(--muted-foreground))" />
                  <YAxis tick={{ fontSize: 10 }} stroke="hsl(var(--muted-foreground))" />
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
                    name="Amount Sold"
                  />
                </AreaChart>
              </ResponsiveContainer>
            )}
          </CardContent>
        </Card>

        {/* Price Impact Chart */}
        <Card>
          <CardHeader>
            <CardTitle>Price Impact (bps)</CardTitle>
          </CardHeader>
          <CardContent>
            {chartData.length === 0 ? (
              <div className="h-48 flex items-center justify-center text-muted-foreground text-sm">
                Waiting for trades...
              </div>
            ) : (
              <ResponsiveContainer width="100%" height={200}>
                <BarChart data={chartData}>
                  <CartesianGrid strokeDasharray="3 3" stroke="hsl(var(--border))" />
                  <XAxis dataKey="time" tick={{ fontSize: 10 }} stroke="hsl(var(--muted-foreground))" />
                  <YAxis tick={{ fontSize: 10 }} stroke="hsl(var(--muted-foreground))" />
                  <Tooltip
                    contentStyle={{
                      backgroundColor: "hsl(var(--card))",
                      border: "1px solid hsl(var(--border))",
                      borderRadius: "8px",
                    }}
                  />
                  <Bar
                    dataKey="impact_bps"
                    fill="hsl(var(--accent))"
                    radius={[4, 4, 0, 0]}
                    name="Impact (bps)"
                  />
                </BarChart>
              </ResponsiveContainer>
            )}
          </CardContent>
        </Card>
      </div>

      {/* Session Config */}
      <Card>
        <CardHeader className="flex flex-row items-center justify-between">
          <CardTitle>Configuration</CardTitle>
          {!editingConfig && (status === "pending" || status === "paused") && (
            <Button
              variant="secondary"
              size="sm"
              onClick={() => {
                setEditPov(session.pov_percent);
                setEditMaxImpact(session.max_price_impact);
                setEditMinTrigger(session.min_buy_trigger_usd);
                setEditingConfig(true);
              }}
            >
              Edit
            </Button>
          )}
        </CardHeader>
        <CardContent>
          {editingConfig ? (
            <div className="space-y-4">
              <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
                <div>
                  <label className="text-sm text-muted-foreground">POV %</label>
                  <Input
                    type="number"
                    step="0.01"
                    value={editPov}
                    onChange={(e) => setEditPov(parseFloat(e.target.value) || 0)}
                  />
                </div>
                <div>
                  <label className="text-sm text-muted-foreground">Max Price Impact %</label>
                  <Input
                    type="number"
                    step="0.01"
                    value={editMaxImpact}
                    onChange={(e) => setEditMaxImpact(parseFloat(e.target.value) || 0)}
                  />
                </div>
                <div>
                  <label className="text-sm text-muted-foreground">Min Buy Trigger (USD)</label>
                  <Input
                    type="number"
                    step="0.01"
                    value={editMinTrigger}
                    onChange={(e) => setEditMinTrigger(parseFloat(e.target.value) || 0)}
                  />
                </div>
              </div>
              <div className="flex gap-2">
                <Button
                  size="sm"
                  onClick={() =>
                    configMutation.mutate({
                      pov_percent: editPov,
                      max_price_impact: editMaxImpact,
                      min_buy_trigger_usd: editMinTrigger,
                    })
                  }
                  disabled={configMutation.isPending}
                >
                  {configMutation.isPending ? "Saving..." : "Save"}
                </Button>
                <Button size="sm" variant="secondary" onClick={() => setEditingConfig(false)}>
                  Cancel
                </Button>
              </div>
            </div>
          ) : (
            <div className="grid grid-cols-2 md:grid-cols-4 gap-4 text-sm">
              <div>
                <span className="text-muted-foreground">Strategy</span>
                <p className="font-mono">POV ({session.pov_percent}%)</p>
              </div>
              <div>
                <span className="text-muted-foreground">Max Impact</span>
                <p className="font-mono">{session.max_price_impact}%</p>
              </div>
              <div>
                <span className="text-muted-foreground">Min Trigger</span>
                <p className="font-mono">${session.min_buy_trigger_usd}</p>
              </div>
              <div>
                <span className="text-muted-foreground">Public Link</span>
                <p className="font-mono text-primary">
                  {session.public_slug ? `/share/${session.public_slug}` : "—"}
                </p>
              </div>
            </div>
          )}
        </CardContent>
      </Card>

      {/* Public Sharing Toggle */}
      <Card>
        <CardHeader>
          <CardTitle>Public Sharing</CardTitle>
        </CardHeader>
        <CardContent className="flex items-center justify-between">
          <div>
            <p className="text-sm">
              {session.public_sharing_enabled
                ? "Anyone with the link can view this session's live data."
                : "Sharing is disabled for this session."}
            </p>
            {session.public_sharing_enabled && session.public_slug && (
              <p className="text-xs font-mono text-muted-foreground mt-1">
                {typeof window !== "undefined" ? window.location.origin : ""}/share/{session.public_slug}
              </p>
            )}
          </div>
          <div className="flex items-center gap-2">
            {session.public_sharing_enabled && session.public_slug && (
              <Button
                variant="secondary"
                size="sm"
                onClick={() => {
                  navigator.clipboard.writeText(
                    `${window.location.origin}/share/${session.public_slug}`,
                  );
                }}
              >
                Copy Link
              </Button>
            )}
            <Button
              variant={session.public_sharing_enabled ? "destructive" : "default"}
              size="sm"
              onClick={() => sharingMutation.mutate(!session.public_sharing_enabled)}
              disabled={sharingMutation.isPending}
            >
              {sharingMutation.isPending
                ? "..."
                : session.public_sharing_enabled
                  ? "Disable"
                  : "Enable"}
            </Button>
          </div>
        </CardContent>
      </Card>

      {/* Recent Trades Table */}
      <Card>
        <CardHeader>
          <CardTitle>Recent Trades ({recentTrades.length})</CardTitle>
        </CardHeader>
        <CardContent>
          {recentTrades.length === 0 ? (
            <p className="text-sm text-muted-foreground py-4 text-center">
              No trades executed yet.
            </p>
          ) : (
            <div className="overflow-x-auto">
              <table className="w-full text-sm">
                <thead>
                  <tr className="border-b border-border text-muted-foreground">
                    <th className="text-left py-2 font-medium">Time</th>
                    <th className="text-right py-2 font-medium">Sold</th>
                    <th className="text-right py-2 font-medium">Received</th>
                    <th className="text-right py-2 font-medium">Impact</th>
                    <th className="text-right py-2 font-medium">Tx</th>
                  </tr>
                </thead>
                <tbody>
                  {recentTrades.slice(0, 15).map((trade) => (
                    <tr
                      key={trade.trade_id}
                      className="border-b border-border/50 hover:bg-secondary/30 transition-colors"
                    >
                      <td className="py-2">
                        {new Date(trade.executed_at).toLocaleTimeString()}
                      </td>
                      <td className="text-right font-mono">
                        {formatTokenAmount(
                          trade.sell_amount,
                          session.sell_token_decimals
                        )}
                      </td>
                      <td className="text-right font-mono text-primary">
                        {formatTokenAmount(
                          trade.received_amount,
                          session.target_token_decimals
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
