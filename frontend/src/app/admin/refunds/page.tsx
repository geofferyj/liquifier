"use client";

import { useState } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { api } from "@/lib/api";
import { useAuthStore } from "@/lib/store";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { cn, shortenAddress, formatTokenAmount } from "@/lib/utils";
import { useRouter } from "next/navigation";
import Link from "next/link";
import type { AdminRefundRequest } from "@/lib/types";

const WKC_DECIMALS = 18;

type FilterStatus = "all" | "pending" | "approved" | "rejected" | "completed";

export default function AdminRefundsPage() {
  const router = useRouter();
  const role = useAuthStore((s) => s.role);

  if (role !== "admin") {
    router.replace("/my-dashboard");
    return null;
  }

  return <AdminRefundsContent />;
}

function AdminRefundsContent() {
  const queryClient = useQueryClient();
  const [filter, setFilter] = useState<FilterStatus>("all");
  const [noteTarget, setNoteTarget] = useState<string | null>(null);
  const [adminNote, setAdminNote] = useState("");

  const refundsQuery = useQuery({
    queryKey: ["admin-refunds"],
    queryFn: () => api.adminListRefundRequests(),
    refetchInterval: 30_000,
  });

  const updateMutation = useMutation({
    mutationFn: ({
      refundId,
      status,
      note,
    }: {
      refundId: string;
      status: string;
      note?: string;
    }) => api.adminUpdateRefundStatus(refundId, status, note),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["admin-refunds"] });
      setNoteTarget(null);
      setAdminNote("");
    },
  });

  const refunds: AdminRefundRequest[] = refundsQuery.data?.refunds ?? [];

  const filtered =
    filter === "all" ? refunds : refunds.filter((r) => r.status === filter);

  const counts = {
    all: refunds.length,
    pending: refunds.filter((r) => r.status === "pending").length,
    approved: refunds.filter((r) => r.status === "approved").length,
    rejected: refunds.filter((r) => r.status === "rejected").length,
    completed: refunds.filter((r) => r.status === "completed").length,
  };

  return (
    <main className="min-h-screen p-8 max-w-5xl mx-auto">
      <div className="flex items-center justify-between mb-8">
        <h1 className="text-3xl font-bold">Refund Requests</h1>
        <Link href="/dashboard">
          <Button variant="secondary">Back to Dashboard</Button>
        </Link>
      </div>

      {/* Filter tabs */}
      <div className="flex gap-2 mb-6 flex-wrap">
        {(
          ["all", "pending", "approved", "rejected", "completed"] as const
        ).map((status) => (
          <Button
            key={status}
            size="sm"
            variant={filter === status ? "default" : "outline"}
            onClick={() => setFilter(status)}
          >
            {status.charAt(0).toUpperCase() + status.slice(1)}
            <span className="ml-1.5 text-xs opacity-70">({counts[status]})</span>
          </Button>
        ))}
      </div>

      {refundsQuery.isLoading && (
        <p className="text-muted-foreground">Loading refund requests...</p>
      )}

      {filtered.length === 0 && !refundsQuery.isLoading && (
        <Card>
          <CardContent className="py-12 text-center">
            <p className="text-muted-foreground">
              No {filter === "all" ? "" : filter + " "}refund requests.
            </p>
          </CardContent>
        </Card>
      )}

      <div className="space-y-3">
        {filtered.map((r) => (
          <Card key={r.refund_id}>
            <CardContent className="py-4">
              <div className="flex items-start justify-between gap-4">
                <div className="flex-1 space-y-1">
                  <div className="flex items-center gap-3">
                    <p className="font-semibold">
                      {formatTokenAmount(r.amount, WKC_DECIMALS)} {r.token_symbol}
                    </p>
                    <span
                      className={cn(
                        "px-2 py-0.5 rounded-full text-xs font-medium",
                        r.status === "pending" &&
                          "bg-yellow-500/10 text-yellow-500",
                        r.status === "approved" &&
                          "bg-green-500/10 text-green-500",
                        r.status === "rejected" &&
                          "bg-red-500/10 text-red-500",
                        r.status === "completed" &&
                          "bg-blue-500/10 text-blue-500",
                      )}
                    >
                      {r.status.toUpperCase()}
                    </span>
                  </div>

                  <p className="text-sm text-muted-foreground">
                    <span className="font-medium text-foreground">
                      {r.username || r.email}
                    </span>{" "}
                    · {r.email}
                  </p>
                  <p className="text-xs text-muted-foreground">
                    Wallet: {shortenAddress(r.wallet_id)} · Requested{" "}
                    {new Date(r.created_at).toLocaleDateString()}{" "}
                    {new Date(r.created_at).toLocaleTimeString()}
                  </p>

                  {r.admin_note && (
                    <p className="text-xs text-muted-foreground mt-1">
                      Admin note: {r.admin_note}
                    </p>
                  )}
                </div>

                {/* Actions for pending refunds */}
                {r.status === "pending" && (
                  <div className="flex flex-col gap-2 items-end shrink-0">
                    <div className="flex gap-2">
                      <Button
                        size="sm"
                        onClick={() =>
                          updateMutation.mutate({
                            refundId: r.refund_id,
                            status: "approved",
                          })
                        }
                        disabled={updateMutation.isPending}
                      >
                        Approve
                      </Button>
                      <Button
                        size="sm"
                        variant="destructive"
                        onClick={() =>
                          noteTarget === r.refund_id
                            ? updateMutation.mutate({
                                refundId: r.refund_id,
                                status: "rejected",
                                note: adminNote || undefined,
                              })
                            : setNoteTarget(r.refund_id)
                        }
                        disabled={updateMutation.isPending}
                      >
                        Reject
                      </Button>
                    </div>
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() =>
                        updateMutation.mutate({
                          refundId: r.refund_id,
                          status: "completed",
                        })
                      }
                      disabled={updateMutation.isPending}
                    >
                      Mark Completed
                    </Button>
                  </div>
                )}

                {r.status === "approved" && (
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() =>
                      updateMutation.mutate({
                        refundId: r.refund_id,
                        status: "completed",
                      })
                    }
                    disabled={updateMutation.isPending}
                  >
                    Mark Completed
                  </Button>
                )}
              </div>

              {/* Note input when rejecting */}
              {noteTarget === r.refund_id && (
                <div className="mt-3 flex gap-2 items-center">
                  <Input
                    placeholder="Reason for rejection (optional)"
                    value={adminNote}
                    onChange={(e) => setAdminNote(e.target.value)}
                    className="flex-1"
                    autoFocus
                  />
                  <Button
                    size="sm"
                    variant="destructive"
                    onClick={() =>
                      updateMutation.mutate({
                        refundId: r.refund_id,
                        status: "rejected",
                        note: adminNote || undefined,
                      })
                    }
                    disabled={updateMutation.isPending}
                  >
                    Confirm Reject
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={() => {
                      setNoteTarget(null);
                      setAdminNote("");
                    }}
                  >
                    Cancel
                  </Button>
                </div>
              )}
            </CardContent>
          </Card>
        ))}
      </div>
    </main>
  );
}
