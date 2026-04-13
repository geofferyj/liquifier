"use client";

import { useState } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { api } from "@/lib/api";
import { useAuthStore } from "@/lib/store";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  cn,
  shortenAddress,
  formatTokenAmount,
  tokenAmountToUsd,
  formatUsd,
} from "@/lib/utils";
import { CopyableAddress } from "@/components/ui/copyable-address";
import { useRouter } from "next/navigation";
import type { AdminUser, AdminUserSession, Wallet } from "@/lib/types";

const WKC_TOKEN_ADDRESS = "0x6Ec90334d89dBdc89E08A133271be3d104128Edb";
const WKC_DECIMALS = 18;

export default function AdminUsersPage() {
  const router = useRouter();
  const role = useAuthStore((s) => s.role);

  if (role !== "admin") {
    router.replace("/my-dashboard");
    return null;
  }

  return <AdminUsersContent />;
}

function AdminUsersContent() {
  const queryClient = useQueryClient();

  const usersQuery = useQuery({
    queryKey: ["admin-users"],
    queryFn: () => api.adminListUsers(),
  });

  const refundsQuery = useQuery({
    queryKey: ["admin-refunds"],
    queryFn: () => api.adminListRefundRequests(),
  });

  const wkcPriceQuery = useQuery({
    queryKey: ["wkc-usd-price"],
    queryFn: () => api.getTokenUsdPrice("bsc", WKC_TOKEN_ADDRESS),
    staleTime: 5 * 60_000,
    refetchInterval: 60_000,
  });

  const wkcPrice = wkcPriceQuery.data?.usd_price ?? 0;

  const [expandedUser, setExpandedUser] = useState<string | null>(null);
  const [userWallets, setUserWallets] = useState<Record<string, Wallet[]>>({});
  const [userSessions, setUserSessions] = useState<
    Record<string, AdminUserSession[]>
  >({});
  const [walletBalances, setWalletBalances] = useState<
    Record<string, { balance: string; decimals: number } | null>
  >({});
  const [loadingWallets, setLoadingWallets] = useState<string | null>(null);

  // Export wallet state
  const [exportTarget, setExportTarget] = useState<{
    userId: string;
    walletId: string;
  } | null>(null);
  const [totpCode, setTotpCode] = useState("");
  const [exportResult, setExportResult] = useState<{
    privateKey: string;
    address: string;
  } | null>(null);
  const [exportError, setExportError] = useState("");
  const [exporting, setExporting] = useState(false);

  const handleExpandUser = async (userId: string) => {
    if (expandedUser === userId) {
      setExpandedUser(null);
      return;
    }
    setExpandedUser(userId);

    // Load wallets if not cached
    if (!userWallets[userId]) {
      setLoadingWallets(userId);
      try {
        const res = await api.adminGetUserWallets(userId);
        setUserWallets((prev) => ({ ...prev, [userId]: res.wallets }));
        // Fetch WKC balances for each wallet in parallel
        for (const w of res.wallets) {
          api
            .getWalletBalance(w.wallet_id, WKC_TOKEN_ADDRESS)
            .then((bal) =>
              setWalletBalances((prev) => ({
                ...prev,
                [w.wallet_id]: bal,
              })),
            )
            .catch(() =>
              setWalletBalances((prev) => ({
                ...prev,
                [w.wallet_id]: null,
              })),
            );
        }
      } catch {
        // ignore
      } finally {
        setLoadingWallets(null);
      }
    }

    // Load sessions if not cached
    if (!userSessions[userId]) {
      try {
        const res = await api.adminGetUserSessions(userId);
        setUserSessions((prev) => ({ ...prev, [userId]: res.sessions }));
      } catch {
        // ignore
      }
    }
  };

  const handleExportRequest = (userId: string, walletId: string) => {
    setExportTarget({ userId, walletId });
    setTotpCode("");
    setExportResult(null);
    setExportError("");
  };

  const handleExportConfirm = async () => {
    if (!exportTarget || !totpCode) return;
    setExporting(true);
    setExportError("");
    try {
      const res = await api.adminExportUserWallet(
        exportTarget.userId,
        exportTarget.walletId,
        totpCode,
      );
      setExportResult({ privateKey: res.private_key, address: res.address });
    } catch (err) {
      setExportError(
        err instanceof Error ? err.message : "Export failed",
      );
    } finally {
      setExporting(false);
    }
  };

  const refundMutation = useMutation({
    mutationFn: ({
      refundId,
      status,
    }: {
      refundId: string;
      status: string;
    }) => api.adminUpdateRefundStatus(refundId, status),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["admin-refunds"] });
    },
  });

  const roleMutation = useMutation({
    mutationFn: ({
      userId,
      role,
    }: {
      userId: string;
      role: "admin" | "common";
    }) => api.adminUpdateUserRole(userId, role),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["admin-users"] });
    },
  });

  const users = usersQuery.data?.users ?? [];
  const refunds = refundsQuery.data?.refunds ?? [];
  const pendingRefunds = refunds.filter((r) => r.status === "pending");

  return (
    <main className="min-h-screen p-8 max-w-5xl mx-auto">
      <div className="flex items-center justify-between mb-8">
        <h1 className="text-3xl font-bold">User Management</h1>
      </div>

      {/* ── Pending Refund Requests ──────────────────────── */}
      {pendingRefunds.length > 0 && (
        <Card className="mb-8">
          <CardHeader>
            <CardTitle>
              Pending Refund Requests ({pendingRefunds.length})
            </CardTitle>
          </CardHeader>
          <CardContent className="space-y-3">
            {pendingRefunds.map((r) => (
              <div
                key={r.refund_id}
                className="flex items-center justify-between p-3 rounded-lg bg-muted/50"
              >
                <div>
                  <p className="text-sm font-medium">
                    {r.username || r.email} — {r.amount} {r.token_symbol}
                    {wkcPrice > 0 && (
                      <span className="text-xs font-normal text-muted-foreground ml-1">({formatUsd(tokenAmountToUsd(r.amount, WKC_DECIMALS, wkcPrice))})</span>
                    )}
                  </p>
                  <p className="text-xs text-muted-foreground">
                    Wallet: <CopyableAddress address={r.wallet_id} className="text-xs" /> ·{" "}
                    {new Date(r.created_at).toLocaleDateString()}
                  </p>
                </div>
                <div className="flex gap-2">
                  <Button
                    size="sm"
                    onClick={() =>
                      refundMutation.mutate({
                        refundId: r.refund_id,
                        status: "approved",
                      })
                    }
                    disabled={refundMutation.isPending}
                  >
                    Approve
                  </Button>
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() =>
                      refundMutation.mutate({
                        refundId: r.refund_id,
                        status: "rejected",
                      })
                    }
                    disabled={refundMutation.isPending}
                  >
                    Reject
                  </Button>
                </div>
              </div>
            ))}
          </CardContent>
        </Card>
      )}

      {/* ── Users List ───────────────────────────────────── */}
      <Card>
        <CardHeader>
          <CardTitle>All Users ({users.length})</CardTitle>
        </CardHeader>
        <CardContent className="space-y-2">
          {users.map((u: AdminUser) => (
            <div key={u.user_id}>
              <div
                className="flex items-center justify-between p-3 rounded-lg bg-muted/50 cursor-pointer hover:bg-muted/80 transition"
                onClick={() => handleExpandUser(u.user_id)}
              >
                <div>
                  <p className="text-sm font-medium">
                    {u.username || u.email}
                  </p>
                  <p className="text-xs text-muted-foreground">
                    {u.email} · {u.wallet_count} wallet
                    {u.wallet_count !== 1 ? "s" : ""} · {u.session_count}{" "}
                    session{u.session_count !== 1 ? "s" : ""}
                  </p>
                  <p className="text-xs text-muted-foreground">
                    Joined {new Date(u.created_at).toLocaleDateString()} ·{" "}
                    {u.email_verified ? "Verified" : "Unverified"}
                  </p>
                </div>
                <div className="flex items-center gap-2">
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={(e) => {
                      e.stopPropagation();
                      roleMutation.mutate({
                        userId: u.user_id,
                        role: u.role === "admin" ? "common" : "admin",
                      });
                    }}
                    disabled={roleMutation.isPending}
                    title={
                      u.role === "admin"
                        ? "Downgrade to common user"
                        : "Upgrade to admin (will require 2FA setup on next login)"
                    }
                  >
                    {u.role === "admin" ? "Demote" : "Promote"}
                  </Button>
                  <span
                    className={cn(
                      "px-2 py-0.5 rounded-full text-xs font-medium",
                      u.role === "admin"
                        ? "bg-purple-500/10 text-purple-500"
                        : "bg-blue-500/10 text-blue-500",
                    )}
                  >
                    {u.role.toUpperCase()}
                  </span>
                  <span className="text-xs text-muted-foreground">
                    {expandedUser === u.user_id ? "▲" : "▼"}
                  </span>
                </div>
              </div>

              {/* Expanded: show wallets + sessions */}
              {expandedUser === u.user_id && (
                <div className="ml-4 mt-2 space-y-4 mb-2">
                  {loadingWallets === u.user_id && (
                    <p className="text-xs text-muted-foreground">
                      Loading wallets...
                    </p>
                  )}

                  {/* Wallets */}
                  {(userWallets[u.user_id] ?? []).length > 0 && (
                    <div>
                      <p className="text-xs font-semibold text-muted-foreground mb-1.5 uppercase tracking-wider">
                        Wallets
                      </p>
                      <div className="space-y-2">
                        {(userWallets[u.user_id] ?? []).map((w) => {
                          const bal = walletBalances[w.wallet_id];
                          return (
                            <div
                              key={w.wallet_id}
                              className="flex items-center justify-between p-3 rounded bg-background border"
                            >
                              <div className="space-y-0.5">
                                <CopyableAddress address={w.address} shorten={false} className="text-xs" />
                                <div className="flex items-center gap-3 text-xs text-muted-foreground">
                                  <span>{w.chain}</span>
                                  <span>
                                    Created{" "}
                                    {new Date(
                                      w.created_at,
                                    ).toLocaleDateString()}
                                  </span>
                                </div>
                                <p className="text-sm font-semibold">
                                  {bal
                                    ? `${formatTokenAmount(bal.balance, bal.decimals)} WKC`
                                    : bal === null
                                      ? "Balance unavailable"
                                      : "Loading balance..."}
                                </p>
                                {bal && wkcPrice > 0 && (
                                  <p className="text-xs text-muted-foreground">{formatUsd(tokenAmountToUsd(bal.balance, bal.decimals, wkcPrice))}</p>
                                )}
                              </div>
                              <Button
                                size="sm"
                                variant="outline"
                                onClick={(e) => {
                                  e.stopPropagation();
                                  handleExportRequest(u.user_id, w.wallet_id);
                                }}
                              >
                                Backup
                              </Button>
                            </div>
                          );
                        })}
                      </div>
                    </div>
                  )}

                  {(userWallets[u.user_id] ?? []).length === 0 &&
                    loadingWallets !== u.user_id && (
                      <p className="text-xs text-muted-foreground">
                        No wallets
                      </p>
                    )}

                  {/* Sessions */}
                  {(userSessions[u.user_id] ?? []).length > 0 && (
                    <div>
                      <p className="text-xs font-semibold text-muted-foreground mb-1.5 uppercase tracking-wider">
                        Sessions
                      </p>
                      <div className="space-y-2">
                        {(userSessions[u.user_id] ?? []).map((s) => {
                          const progress =
                            s.total_amount !== "0"
                              ? Number(
                                  (BigInt(s.amount_sold) * 10000n) /
                                    BigInt(s.total_amount),
                                ) / 100
                              : 0;
                          return (
                            <div
                              key={s.session_id}
                              className="p-3 rounded bg-background border space-y-2"
                            >
                              <div className="flex items-center justify-between">
                                <div>
                                  <p className="text-sm font-semibold">
                                    {s.sell_token_symbol} →{" "}
                                    {s.target_token_symbol}
                                  </p>
                                  <p className="text-xs text-muted-foreground">
                                    {s.chain} · Wallet:{" "}
                                    <CopyableAddress address={s.wallet_address} className="text-xs" /> ·{" "}
                                    {s.strategy.toUpperCase()} · POV{" "}
                                    {s.pov_percent}%
                                  </p>
                                </div>
                                <span
                                  className={cn(
                                    "px-2 py-0.5 rounded-full text-xs font-medium",
                                    s.status === "active" &&
                                      "bg-green-500/10 text-green-500",
                                    s.status === "paused" &&
                                      "bg-yellow-500/10 text-yellow-500",
                                    s.status === "completed" &&
                                      "bg-blue-500/10 text-blue-500",
                                    s.status === "cancelled" &&
                                      "bg-red-500/10 text-red-500",
                                    s.status === "pending" &&
                                      "bg-muted text-muted-foreground",
                                    s.status === "error" &&
                                      "bg-red-500/10 text-red-500",
                                  )}
                                >
                                  {s.status.toUpperCase()}
                                </span>
                              </div>
                              <div className="flex items-center justify-between text-xs text-muted-foreground">
                                <span>
                                  Sold:{" "}
                                  {formatTokenAmount(
                                    s.amount_sold,
                                    s.sell_token_decimals,
                                  )}{" "}
                                  /{" "}
                                  {formatTokenAmount(
                                    s.total_amount,
                                    s.sell_token_decimals,
                                  )}{" "}
                                  {s.sell_token_symbol}
                                  {wkcPrice > 0 && (
                                    <span className="ml-1">({formatUsd(tokenAmountToUsd(s.amount_sold, s.sell_token_decimals, wkcPrice))} / {formatUsd(tokenAmountToUsd(s.total_amount, s.sell_token_decimals, wkcPrice))})</span>
                                  )}
                                </span>
                                <span>{progress.toFixed(1)}% complete</span>
                              </div>
                              <div className="w-full h-1 bg-secondary rounded-full overflow-hidden">
                                <div
                                  className="h-full bg-primary rounded-full transition-all"
                                  style={{
                                    width: `${Math.min(progress, 100)}%`,
                                  }}
                                />
                              </div>
                              <p className="text-xs text-muted-foreground">
                                Created{" "}
                                {new Date(s.created_at).toLocaleDateString()}
                                {" · Updated "}
                                {new Date(s.updated_at).toLocaleDateString()}
                              </p>
                            </div>
                          );
                        })}
                      </div>
                    </div>
                  )}
                </div>
              )}
            </div>
          ))}
        </CardContent>
      </Card>

      {/* TOTP prompt for admin export */}
      {exportTarget && !exportResult && (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
          <Card className="w-full max-w-sm">
            <CardHeader>
              <CardTitle className="text-lg">
                Backup Wallet — Enter Your 2FA
              </CardTitle>
            </CardHeader>
            <CardContent className="space-y-3">
              <div className="flex items-center gap-2">
                <Input
                  type="text"
                  inputMode="numeric"
                  maxLength={6}
                  value={totpCode}
                  onChange={(e) => setTotpCode(e.target.value)}
                  onKeyDown={(e) =>
                    e.key === "Enter" && handleExportConfirm()
                  }
                  placeholder="000000"
                  className="w-32 text-center tracking-widest"
                  autoFocus
                />
                <Button
                  size="sm"
                  onClick={handleExportConfirm}
                  disabled={totpCode.length < 6 || exporting}
                >
                  {exporting ? "..." : "Confirm"}
                </Button>
              </div>
              {exportError && (
                <p className="text-sm text-destructive">{exportError}</p>
              )}
              <Button
                size="sm"
                variant="ghost"
                onClick={() => setExportTarget(null)}
              >
                Cancel
              </Button>
            </CardContent>
          </Card>
        </div>
      )}

      {/* Export result modal */}
      {exportResult && (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
          <Card className="w-full max-w-md">
            <CardHeader>
              <CardTitle className="text-lg text-amber-500">
                Private Key
              </CardTitle>
            </CardHeader>
            <CardContent className="space-y-3">
              <p className="text-xs text-muted-foreground">
                Store this securely. Never share it.
              </p>
              <code className="text-xs break-all select-all bg-muted p-2 rounded block">
                {exportResult.privateKey}
              </code>
              <p className="text-xs text-muted-foreground">
                Address: <CopyableAddress address={exportResult.address} shorten={false} className="text-xs" />
              </p>
              <div className="flex gap-2">
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() =>
                    navigator.clipboard.writeText(exportResult.privateKey)
                  }
                >
                  Copy
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => {
                    setExportResult(null);
                    setExportTarget(null);
                  }}
                >
                  Dismiss
                </Button>
              </div>
            </CardContent>
          </Card>
        </div>
      )}
    </main>
  );
}
