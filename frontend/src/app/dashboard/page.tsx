"use client";

import Link from "next/link";
import { useState, useEffect } from "react";
import { useRouter } from "next/navigation";
import { useMutation, useQuery } from "@tanstack/react-query";
import { QRCodeSVG } from "qrcode.react";
import { api } from "@/lib/api";
import { useAuthStore } from "@/lib/store";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { cn, shortenAddress } from "@/lib/utils";
import type { SessionStatus } from "@/lib/types";

export default function DashboardPage() {
  const router = useRouter();
  const role = useAuthStore((s) => s.role);
  const hydrated = useAuthStore((s) => s.hydrated);

  useEffect(() => {
    if (hydrated && role && role !== "admin") router.replace("/my-dashboard");
  }, [hydrated, role, router]);

  const sessionsQuery = useQuery({
    queryKey: ["sessions"],
    queryFn: () => api.listSessions(),
    refetchInterval: 30_000,
    enabled: hydrated,
  });

  const walletsQuery = useQuery({
    queryKey: ["wallets"],
    queryFn: () => api.listWallets(),
    enabled: hydrated,
  });

  const createWalletMutation = useMutation({
    mutationFn: () => api.createWallet(),
    onSuccess: () => walletsQuery.refetch(),
  });

  const [exportedKey, setExportedKey] = useState<{
    walletId: string;
    privateKey: string;
    address: string;
  } | null>(null);
  const [exportingId, setExportingId] = useState<string | null>(null);
  const [totpPrompt, setTotpPrompt] = useState<string | null>(null); // walletId awaiting TOTP
  const [totpCode, setTotpCode] = useState("");
  const [totpError, setTotpError] = useState("");

  const handleExportRequest = (walletId: string) => {
    setTotpPrompt(walletId);
    setTotpCode("");
    setTotpError("");
    setExportedKey(null);
  };

  const handleExportConfirm = async () => {
    if (!totpPrompt || !totpCode) return;
    setExportingId(totpPrompt);
    setTotpError("");
    try {
      const res = await api.exportWallet(totpPrompt, totpCode);
      setExportedKey({ walletId: totpPrompt, privateKey: res.private_key, address: res.address });
      setTotpPrompt(null);
    } catch (err: unknown) {
      setTotpError(err instanceof Error ? err.message : "Invalid 2FA code");
    } finally {
      setExportingId(null);
    }
  };

  const sessions = sessionsQuery.data?.sessions ?? [];
  const wallets = walletsQuery.data?.wallets ?? [];

  if (!hydrated) return null;

  return (
    <main className="min-h-screen p-8 max-w-5xl mx-auto">
      <div className="flex items-center justify-between mb-8">
        <h1 className="text-3xl font-bold">Dashboard</h1>
        <div className="flex gap-2">
          <Link href="/admin/users">
            <Button variant="secondary">Manage Users</Button>
          </Link>
          <Link href="/admin/refunds">
            <Button variant="secondary">Refund Requests</Button>
          </Link>
          <Link href="/dashboard/settings">
            <Button variant="secondary">Settings</Button>
          </Link>
          <Link href="/sessions/new">
            <Button>New Session</Button>
          </Link>
        </div>
      </div>

      {sessionsQuery.isLoading && (
        <p className="text-muted-foreground">Loading sessions...</p>
      )}

      {/* ── Wallets ──────────────────────────────────────── */}
      <Card className="mb-8">
        <CardHeader className="flex flex-row items-center justify-between">
          <CardTitle>Wallets</CardTitle>
          {wallets.length === 0 && (
            <Button
              size="sm"
              onClick={() => createWalletMutation.mutate()}
              disabled={createWalletMutation.isPending}
            >
              {createWalletMutation.isPending ? "Creating..." : "Create Wallet"}
            </Button>
          )}
        </CardHeader>
        <CardContent>
          {wallets.length === 0 ? (
            <p className="text-sm text-muted-foreground">No wallets yet. Create one to get started.</p>
          ) : (
            <div className="space-y-4">
              {wallets.map((w) => (
                <div
                  key={w.wallet_id}
                  className="flex flex-col sm:flex-row gap-4 p-4 rounded-lg bg-muted/50"
                >
                  {/* QR Code */}
                  <div className="flex-shrink-0 flex justify-center">
                    <div className="bg-white p-2 rounded-lg">
                      <QRCodeSVG value={w.address} size={120} />
                    </div>
                  </div>

                  {/* Details */}
                  <div className="flex-1 flex items-center justify-between">
                    <div>
                      <code className="text-sm break-all select-all">{w.address}</code>
                      <p className="text-xs text-muted-foreground mt-0.5">
                        {w.chain?.toUpperCase()} · Created {new Date(w.created_at).toLocaleDateString()}
                      </p>
                    </div>
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => handleExportRequest(w.wallet_id)}
                      disabled={exportingId === w.wallet_id}
                    >
                      {exportingId === w.wallet_id ? "Exporting..." : "Backup"}
                    </Button>
                  </div>
                </div>
              ))}
            </div>
          )}

          {/* TOTP prompt for export */}
          {totpPrompt && !exportedKey && (
            <div className="mt-4 p-4 rounded-lg border border-primary/50 bg-primary/5 space-y-3">
              <p className="text-sm font-medium">
                Enter your 2FA code to export the private key
              </p>
              <div className="flex items-center gap-2">
                <Input
                  type="text"
                  inputMode="numeric"
                  maxLength={6}
                  value={totpCode}
                  onChange={(e) => setTotpCode(e.target.value)}
                  onKeyDown={(e) => e.key === "Enter" && handleExportConfirm()}
                  placeholder="000000"
                  className="w-32 text-center tracking-widest"
                  autoFocus
                />
                <Button
                  size="sm"
                  onClick={handleExportConfirm}
                  disabled={totpCode.length < 6 || exportingId !== null}
                >
                  {exportingId ? "Verifying..." : "Confirm"}
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => setTotpPrompt(null)}
                >
                  Cancel
                </Button>
              </div>
              {totpError && (
                <p className="text-sm text-destructive">{totpError}</p>
              )}
            </div>
          )}

          {/* Export modal */}
          {exportedKey && (
            <div className="mt-4 p-4 rounded-lg border border-amber-500/50 bg-amber-500/5 space-y-2">
              <p className="text-sm font-medium text-amber-500">
                ⚠ Private Key — store this securely and never share it
              </p>
              <div className="flex items-center gap-2">
                <code className="text-xs break-all flex-1 select-all bg-muted p-2 rounded">
                  {exportedKey.privateKey}
                </code>
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => {
                    navigator.clipboard.writeText(exportedKey.privateKey);
                  }}
                >
                  Copy
                </Button>
              </div>
              <p className="text-xs text-muted-foreground">
                Address: {exportedKey.address}
              </p>
              <Button
                size="sm"
                variant="ghost"
                onClick={() => setExportedKey(null)}
              >
                Dismiss
              </Button>
            </div>
          )}
        </CardContent>
      </Card>

      {/* ── Sessions ─────────────────────────────────────── */}

      {sessions.length === 0 && !sessionsQuery.isLoading && (
        <Card>
          <CardContent className="py-12 text-center">
            <p className="text-muted-foreground mb-4">No sessions yet.</p>
            <Link href="/sessions/new">
              <Button>Create Your First Session</Button>
            </Link>
          </CardContent>
        </Card>
      )}

      <div className="grid gap-4">
        {sessions.map((session) => {
          const status = session.status as SessionStatus;
          const progress =
            session.total_amount !== "0"
              ? Number(
                  (BigInt(session.amount_sold) * 10000n) /
                    BigInt(session.total_amount)
                ) / 100
              : 0;

          return (
            <Link
              key={session.session_id}
              href={`/sessions/${session.session_id}`}
            >
              <Card className="hover:border-primary/50 transition-colors cursor-pointer">
                <CardContent className="py-4">
                  <div className="flex items-center justify-between">
                    <div className="flex items-center gap-4">
                      <div>
                        <p className="font-semibold">
                          {session.sell_token_symbol} →{" "}
                          {session.target_token_symbol}
                        </p>
                        <p className="text-xs text-muted-foreground">
                          {session.chain} ·{" "}
                          {shortenAddress(session.session_id)}
                        </p>
                      </div>
                    </div>

                    <div className="flex items-center gap-6">
                      <div className="text-right">
                        <p className="text-sm font-mono">
                          {progress.toFixed(1)}%
                        </p>
                        <p className="text-xs text-muted-foreground">
                          POV {session.pov_percent}%
                        </p>
                      </div>

                      <span
                        className={cn(
                          "px-2.5 py-0.5 rounded-full text-xs font-medium",
                          status === "active" &&
                            "bg-green-500/10 text-green-500",
                          status === "paused" &&
                            "bg-yellow-500/10 text-yellow-500",
                          status === "completed" &&
                            "bg-blue-500/10 text-blue-500",
                          status === "cancelled" &&
                            "bg-red-500/10 text-red-500",
                          status === "pending" &&
                            "bg-muted text-muted-foreground",
                          status === "error" && "bg-red-500/10 text-red-500"
                        )}
                      >
                        {status.toUpperCase()}
                      </span>
                    </div>
                  </div>

                  {/* Progress bar */}
                  <div className="mt-3 w-full h-1 bg-secondary rounded-full overflow-hidden">
                    <div
                      className="h-full bg-primary rounded-full transition-all"
                      style={{ width: `${Math.min(progress, 100)}%` }}
                    />
                  </div>
                </CardContent>
              </Card>
            </Link>
          );
        })}
      </div>
    </main>
  );
}
