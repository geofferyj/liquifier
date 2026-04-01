"use client";

import { useQuery } from "@tanstack/react-query";
import Link from "next/link";
import apiClient from "@/lib/api";
import { formatTokenAmount, sellProgress } from "@/lib/utils";
import { cn } from "@/lib/utils";
import type { Session } from "@/types";

export default function DashboardPage() {
  const { data: sessions = [], isLoading } = useQuery<Session[]>({
    queryKey: ["sessions"],
    queryFn:  () => apiClient.get("/api/sessions").then((r) => r.data),
  });

  const active    = sessions.filter((s) => s.status === "active").length;
  const completed = sessions.filter((s) => s.status === "completed").length;

  return (
    <div className="max-w-5xl mx-auto py-8 px-4 space-y-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-bold text-white">Dashboard</h1>
        <Link href="/sessions/new" className="btn-primary text-sm">
          + New Session
        </Link>
      </div>

      {/* Summary KPIs */}
      <div className="grid grid-cols-3 gap-4">
        <KpiCard title="Total Sessions" value={sessions.length} />
        <KpiCard title="Active"         value={active} highlight />
        <KpiCard title="Completed"      value={completed} />
      </div>

      {/* Sessions list */}
      {isLoading ? (
        <div className="space-y-3">
          {[...Array(3)].map((_, i) => (
            <div key={i} className="h-24 bg-secondary rounded-lg animate-pulse" />
          ))}
        </div>
      ) : sessions.length === 0 ? (
        <div className="flex flex-col items-center justify-center py-20 text-muted-foreground">
          <p className="text-5xl mb-4">🪣</p>
          <p className="text-lg font-medium">No sessions yet</p>
          <p className="text-sm mt-1 mb-6">Create your first Liquifier session to start offloading.</p>
          <Link href="/sessions/new" className="btn-primary">
            Create Session
          </Link>
        </div>
      ) : (
        <div className="space-y-3">
          {sessions.map((session) => (
            <SessionRow key={session.id} session={session} />
          ))}
        </div>
      )}
    </div>
  );
}

function SessionRow({ session }: { session: Session }) {
  const progress = sellProgress(session.amount_sold, session.total_amount);

  const statusColor: Record<Session["status"], string> = {
    active:    "text-green-400",
    pending:   "text-yellow-400",
    paused:    "text-blue-400",
    completed: "text-gray-400",
    failed:    "text-red-400",
  };

  return (
    <Link
      href={`/sessions/${session.id}`}
      className="block rounded-lg border border-border bg-card p-4 hover:border-primary/50 transition-colors"
    >
      <div className="flex items-start justify-between mb-3">
        <div>
          <p className="font-mono text-xs text-muted-foreground">{session.id.slice(0, 16)}…</p>
          <p className="text-sm font-medium text-white mt-0.5">
            Chain {session.chain_id} — {session.strategy.toUpperCase()}
          </p>
        </div>
        <span className={cn("text-sm font-semibold capitalize", statusColor[session.status])}>
          {session.status}
        </span>
      </div>

      {/* Progress bar */}
      <div className="h-2 bg-secondary rounded-full overflow-hidden">
        <div
          className={cn(
            "h-full rounded-full transition-all",
            session.status === "completed" ? "bg-green-500" : "bg-primary"
          )}
          style={{ width: `${progress}%` }}
        />
      </div>

      <div className="flex justify-between text-xs text-muted-foreground mt-1.5">
        <span>Sold: {formatTokenAmount(session.amount_sold)}</span>
        <span>{progress.toFixed(1)}% of {formatTokenAmount(session.total_amount)}</span>
      </div>
    </Link>
  );
}

function KpiCard({ title, value, highlight }: { title: string; value: number; highlight?: boolean }) {
  return (
    <div className={cn("rounded-lg border p-4", highlight ? "border-primary/50 bg-primary/5" : "border-border bg-card")}>
      <p className="text-xs text-muted-foreground mb-1">{title}</p>
      <p className={cn("text-3xl font-bold", highlight ? "text-primary" : "text-white")}>{value}</p>
    </div>
  );
}
