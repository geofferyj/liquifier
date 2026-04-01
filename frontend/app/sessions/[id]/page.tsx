"use client";

import { useParams } from "next/navigation";
import { useQuery } from "@tanstack/react-query";
import {
  ResponsiveContainer,
  LineChart,
  Line,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  Legend,
  AreaChart,
  Area,
} from "recharts";
import { useSessionSocket } from "@/hooks/useSessionSocket";
import { formatTokenAmount, sellProgress, shortenAddress, bpsToPercent } from "@/lib/utils";
import apiClient from "@/lib/api";
import { cn } from "@/lib/utils";
import type { Session, TradeDataPoint } from "@/types";
import { useMemo } from "react";

// ── Page ───────────────────────────────────────────────────────────────────

export default function SessionDashboard() {
  const params    = useParams<{ id: string }>();
  const sessionId = params.id;

  // Fetch static session data once
  const { data: session, isLoading } = useQuery<Session>({
    queryKey: ["session", sessionId],
    queryFn:  () => apiClient.get(`/api/sessions/${sessionId}`).then((r) => r.data),
    enabled:  !!sessionId,
  });

  // Live metrics + trade history via WebSocket
  const { metrics, tradeHistory, status, sendMessage } = useSessionSocket(sessionId);

  // Derive chart data from trade history
  const chartData: TradeDataPoint[] = useMemo(
    () =>
      [...tradeHistory].reverse().map((t, i) => ({
        time:          t.tx_hash.slice(0, 8) + "…",
        amountSold:    Number(t.amount_in) / 1e18,
        priceImpactBps: t.price_impact_bps,
      })),
    [tradeHistory]
  );

  const progress = metrics
    ? sellProgress(metrics.amount_sold, metrics.total_amount)
    : session
    ? sellProgress(session.amount_sold, session.total_amount)
    : 0;

  const currentStatus = metrics?.status ?? session?.status ?? "pending";

  if (isLoading) {
    return <PageSkeleton />;
  }

  if (!session) {
    return (
      <div className="flex items-center justify-center h-64 text-muted-foreground">
        Session not found.
      </div>
    );
  }

  return (
    <div className="max-w-5xl mx-auto py-8 px-4 space-y-6">
      {/* ── Header ── */}
      <div className="flex items-start justify-between">
        <div>
          <h1 className="text-2xl font-bold text-white">Session Dashboard</h1>
          <p className="text-muted-foreground font-mono text-sm">{sessionId}</p>
        </div>

        <div className="flex items-center gap-3">
          {/* Connection badge */}
          <ConnectionBadge status={status} />

          {/* Play / Pause */}
          {currentStatus === "active" && (
            <button
              className="btn-secondary flex items-center gap-2 text-sm"
              onClick={() => sendMessage({ action: "pause" })}
            >
              ⏸ Pause
            </button>
          )}
          {(currentStatus === "paused" || currentStatus === "pending") && (
            <button
              className="btn-primary flex items-center gap-2 text-sm"
              onClick={async () => {
                await apiClient.post(`/api/sessions/${sessionId}/start`);
                sendMessage({ action: "resume" });
              }}
            >
              ▶ Start
            </button>
          )}
        </div>
      </div>

      {/* ── Status banner ── */}
      <StatusBanner status={currentStatus as Session["status"]} />

      {/* ── KPI cards ── */}
      <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
        <KpiCard
          title="Total to Sell"
          value={formatTokenAmount(session.total_amount)}
        />
        <KpiCard
          title="Amount Sold"
          value={formatTokenAmount(metrics?.amount_sold ?? session.amount_sold)}
          highlight
        />
        <KpiCard
          title="Remaining"
          value={formatTokenAmount(metrics?.remaining ?? (
            (BigInt(session.total_amount) - BigInt(session.amount_sold)).toString()
          ))}
        />
        <KpiCard
          title="Trades"
          value={String(metrics?.trade_count ?? 0)}
        />
      </div>

      {/* ── Progress bar ── */}
      <div>
        <div className="flex justify-between text-sm text-muted-foreground mb-1">
          <span>Sell Progress</span>
          <span>{progress.toFixed(2)}%</span>
        </div>
        <div className="h-3 bg-secondary rounded-full overflow-hidden">
          <div
            className="h-full bg-primary transition-all duration-700"
            style={{ width: `${progress}%` }}
          />
        </div>
      </div>

      {/* ── Session config ── */}
      <div className="grid grid-cols-2 md:grid-cols-3 gap-3 text-sm">
        <ConfigBadge label="Strategy"       value={session.strategy.toUpperCase()} />
        <ConfigBadge label="POV %"          value={session.pov_percentage ? `${session.pov_percentage}%` : "—"} />
        <ConfigBadge label="Max Impact"     value={session.max_price_impact_bps ? bpsToPercent(session.max_price_impact_bps) : "—"} />
        <ConfigBadge label="Min Buy Trigger" value={`$${session.min_buy_trigger_usd}`} />
        <ConfigBadge label="Chain"          value={`Chain ${session.chain_id}`} />
        <ConfigBadge label="Token"          value={shortenAddress(session.token_address)} mono />
      </div>

      {/* ── Charts ── */}
      {chartData.length > 0 && (
        <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
          {/* Cumulative sells */}
          <ChartCard title="Tokens Sold per Trade (ETH)">
            <ResponsiveContainer width="100%" height={220}>
              <AreaChart data={chartData}>
                <defs>
                  <linearGradient id="sellGrad" x1="0" y1="0" x2="0" y2="1">
                    <stop offset="5%"  stopColor="#6366f1" stopOpacity={0.4} />
                    <stop offset="95%" stopColor="#6366f1" stopOpacity={0}   />
                  </linearGradient>
                </defs>
                <CartesianGrid strokeDasharray="3 3" stroke="#1e293b" />
                <XAxis dataKey="time" tick={{ fill: "#64748b", fontSize: 11 }} />
                <YAxis tick={{ fill: "#64748b", fontSize: 11 }} />
                <Tooltip
                  contentStyle={{ backgroundColor: "#0f172a", border: "1px solid #1e293b" }}
                  labelStyle={{ color: "#94a3b8" }}
                />
                <Area
                  type="monotone"
                  dataKey="amountSold"
                  stroke="#6366f1"
                  fill="url(#sellGrad)"
                  strokeWidth={2}
                  dot={false}
                  name="Amount Sold"
                />
              </AreaChart>
            </ResponsiveContainer>
          </ChartCard>

          {/* Price impact */}
          <ChartCard title="Price Impact per Trade (bps)">
            <ResponsiveContainer width="100%" height={220}>
              <LineChart data={chartData}>
                <CartesianGrid strokeDasharray="3 3" stroke="#1e293b" />
                <XAxis dataKey="time" tick={{ fill: "#64748b", fontSize: 11 }} />
                <YAxis tick={{ fill: "#64748b", fontSize: 11 }} />
                <Tooltip
                  contentStyle={{ backgroundColor: "#0f172a", border: "1px solid #1e293b" }}
                  labelStyle={{ color: "#94a3b8" }}
                />
                <Line
                  type="monotone"
                  dataKey="priceImpactBps"
                  stroke="#f59e0b"
                  strokeWidth={2}
                  dot={false}
                  name="Impact (bps)"
                />
              </LineChart>
            </ResponsiveContainer>
          </ChartCard>
        </div>
      )}

      {/* ── Recent trades ── */}
      {tradeHistory.length > 0 && (
        <div>
          <h2 className="text-lg font-semibold mb-3 text-white">Recent Trades</h2>
          <div className="rounded-lg border border-border overflow-hidden">
            <table className="w-full text-sm">
              <thead>
                <tr className="bg-secondary text-muted-foreground">
                  <th className="p-3 text-left">Tx Hash</th>
                  <th className="p-3 text-right">Amount In</th>
                  <th className="p-3 text-right">Price Impact</th>
                  <th className="p-3 text-center">Status</th>
                </tr>
              </thead>
              <tbody>
                {tradeHistory.slice(0, 10).map((t) => (
                  <tr key={t.tx_hash} className="border-t border-border hover:bg-muted/30">
                    <td className="p-3 font-mono text-xs">
                      <a
                        href={`https://etherscan.io/tx/${t.tx_hash}`}
                        target="_blank"
                        rel="noopener noreferrer"
                        className="text-primary hover:underline"
                      >
                        {t.tx_hash.slice(0, 10)}…
                      </a>
                    </td>
                    <td className="p-3 text-right">
                      {formatTokenAmount(t.amount_in, 18, 6)}
                    </td>
                    <td className="p-3 text-right text-amber-400">
                      {bpsToPercent(t.price_impact_bps)}
                    </td>
                    <td className="p-3 text-center">
                      <StatusPill status={t.status} />
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}

      {/* ── Empty state ── */}
      {tradeHistory.length === 0 && (
        <div className="flex flex-col items-center justify-center py-16 text-muted-foreground">
          <p className="text-4xl mb-3">⏳</p>
          <p className="text-lg">Waiting for buy events…</p>
          <p className="text-sm mt-1">
            Trades will appear here once the engine detects qualifying buys.
          </p>
        </div>
      )}
    </div>
  );
}

// ── Sub-components ─────────────────────────────────────────────────────────

function KpiCard({ title, value, highlight }: { title: string; value: string; highlight?: boolean }) {
  return (
    <div className={cn("rounded-lg border p-4", highlight ? "border-primary/50 bg-primary/5" : "border-border bg-card")}>
      <p className="text-xs text-muted-foreground mb-1">{title}</p>
      <p className={cn("text-xl font-bold", highlight ? "text-primary" : "text-white")}>{value}</p>
    </div>
  );
}

function ConfigBadge({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="rounded-md bg-secondary px-3 py-2">
      <p className="text-xs text-muted-foreground">{label}</p>
      <p className={cn("font-medium text-sm text-white", mono && "font-mono")}>{value}</p>
    </div>
  );
}

function ChartCard({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="rounded-lg border border-border bg-card p-4">
      <h3 className="text-sm font-medium text-muted-foreground mb-3">{title}</h3>
      {children}
    </div>
  );
}

function ConnectionBadge({ status }: { status: string }) {
  const colors: Record<string, string> = {
    connected:    "bg-green-500",
    connecting:   "bg-yellow-500 animate-pulse",
    disconnected: "bg-gray-500",
    error:        "bg-red-500",
  };
  return (
    <div className="flex items-center gap-2 text-xs text-muted-foreground">
      <span className={cn("w-2 h-2 rounded-full", colors[status] ?? "bg-gray-500")} />
      {status}
    </div>
  );
}

function StatusBanner({ status }: { status: Session["status"] }) {
  const map: Record<Session["status"], { label: string; cls: string }> = {
    pending:   { label: "Pending — waiting to start",  cls: "bg-yellow-500/10 text-yellow-400 border-yellow-500/20" },
    active:    { label: "Active — engine is running",  cls: "bg-green-500/10  text-green-400  border-green-500/20"  },
    paused:    { label: "Paused",                      cls: "bg-blue-500/10   text-blue-400   border-blue-500/20"   },
    completed: { label: "Completed ✓",                 cls: "bg-green-500/10  text-green-400  border-green-500/20"  },
    failed:    { label: "Failed — check logs",         cls: "bg-red-500/10    text-red-400    border-red-500/20"    },
  };
  const { label, cls } = map[status] ?? map.pending;
  return (
    <div className={cn("rounded-md border px-4 py-2 text-sm font-medium", cls)}>
      {label}
    </div>
  );
}

function StatusPill({ status }: { status: string }) {
  const cls =
    status === "confirmed" ? "bg-green-500/20 text-green-400" :
    status === "submitted" ? "bg-blue-500/20  text-blue-400"  :
    status === "failed"    ? "bg-red-500/20   text-red-400"   :
                             "bg-muted text-muted-foreground";
  return (
    <span className={cn("px-2 py-0.5 rounded-full text-xs font-medium", cls)}>
      {status}
    </span>
  );
}

function PageSkeleton() {
  return (
    <div className="max-w-5xl mx-auto py-8 px-4 animate-pulse space-y-4">
      <div className="h-8 bg-secondary rounded w-64" />
      <div className="grid grid-cols-4 gap-4">
        {[...Array(4)].map((_, i) => (
          <div key={i} className="h-20 bg-secondary rounded" />
        ))}
      </div>
      <div className="h-4 bg-secondary rounded" />
      <div className="h-64 bg-secondary rounded" />
    </div>
  );
}
