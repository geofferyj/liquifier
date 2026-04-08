"use client";

import { useState } from "react";
import { useQuery, useMutation } from "@tanstack/react-query";
import { QRCodeSVG } from "qrcode.react";
import { api } from "@/lib/api";
import { useAuthStore } from "@/lib/store";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { cn, formatTokenAmount, shortenAddress } from "@/lib/utils";
import { useRouter } from "next/navigation";

const WKC_TOKEN_ADDRESS = "0x6Ec90334d89dBdc89E08A133271be3d104128Edb";
const WKC_DECIMALS = 18;

export default function MyDashboardPage() {
  const router = useRouter();
  const role = useAuthStore((s) => s.role);
  const clearAuth = useAuthStore((s) => s.clearAuth);
  const hydrated = useAuthStore((s) => s.hydrated);

  // Wait for hydration
  if (!hydrated) return null;

  // Redirect admin users
  if (role === "admin") {
    router.replace("/dashboard");
    return null;
  }

  return <MyDashboardContent />;
}

function MyDashboardContent() {
  const router = useRouter();
  const clearAuth = useAuthStore((s) => s.clearAuth);

  const profileQuery = useQuery({
    queryKey: ["profile"],
    queryFn: () => api.getProfile(),
  });

  const walletsQuery = useQuery({
    queryKey: ["wallets"],
    queryFn: () => api.listWallets(),
  });

  const sessionsQuery = useQuery({
    queryKey: ["my-wallet-sessions"],
    queryFn: () => api.listMyWalletSessions(),
    refetchInterval: 30_000,
  });

  const refundsQuery = useQuery({
    queryKey: ["my-refunds"],
    queryFn: () => api.listMyRefundRequests(),
  });

  const wallet = walletsQuery.data?.wallets?.[0];

  const balanceQuery = useQuery({
    queryKey: ["wallet-balance", wallet?.wallet_id, WKC_TOKEN_ADDRESS],
    queryFn: () =>
      api.getWalletBalance(wallet!.wallet_id, WKC_TOKEN_ADDRESS),
    enabled: !!wallet,
    refetchInterval: 30_000,
  });

  const [desiredUsd, setDesiredUsd] = useState("10000");
  const [walletExpanded, setWalletExpanded] = useState(false);

  const [refundAmount, setRefundAmount] = useState("");
  const [refundError, setRefundError] = useState("");
  const [refundSuccess, setRefundSuccess] = useState("");
  const [showRefundForm, setShowRefundForm] = useState(false);

  const refundMutation = useMutation({
    mutationFn: () =>
      api.createRefundRequest({
        wallet_id: wallet!.wallet_id,
        amount: refundAmount,
        token_address: WKC_TOKEN_ADDRESS,
        token_symbol: "WKC",
      }),
    onSuccess: (data) => {
      setRefundSuccess(data.message);
      setRefundAmount("");
      setRefundError("");
      refundsQuery.refetch();
    },
    onError: (err) => {
      setRefundError(
        err instanceof Error ? err.message : "Failed to submit refund request",
      );
    },
  });

  const handleRefund = (e: React.FormEvent) => {
    e.preventDefault();
    setRefundError("");
    setRefundSuccess("");
    if (!refundAmount || !wallet) return;
    refundMutation.mutate();
  };

  const handleLogout = () => {
    api.clearTokens();
    clearAuth();
    router.push("/login");
  };

  const profile = profileQuery.data;
  const sessions = sessionsQuery.data?.sessions ?? [];
  const refunds = refundsQuery.data?.refunds ?? [];
  const balance = balanceQuery.data;

  const hasActiveSession = sessions.length > 0;

  return (
    <main className="min-h-screen p-8 max-w-3xl mx-auto">
      <div className="flex items-center justify-between mb-8">
        <div>
          <h1 className="text-3xl font-bold">My Dashboard</h1>
          {profile && (
            <p className="text-sm text-muted-foreground mt-1">
              Welcome, {profile.username || profile.email}
            </p>
          )}
        </div>
        <Button variant="outline" onClick={handleLogout}>
          Sign Out
        </Button>
      </div>

      {/* ── Wallet Card with QR ─────────────────────────── */}
      <Card className="mb-8">
        <CardHeader
          className={cn(
            hasActiveSession && "cursor-pointer select-none",
          )}
          onClick={() => hasActiveSession && setWalletExpanded((v) => !v)}
        >
          <div className="flex items-center justify-between">
            <CardTitle className="flex items-center gap-2">
              My Wallet
              {hasActiveSession && wallet && (
                <span className="text-sm font-normal text-muted-foreground">
                  · {shortenAddress(wallet.address)}
                  {balance
                    ? ` · ${formatTokenAmount(balance.balance, balance.decimals)} WKC`
                    : ""}
                </span>
              )}
            </CardTitle>
            {hasActiveSession && (
              <svg
                xmlns="http://www.w3.org/2000/svg"
                width="16"
                height="16"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
                className={cn(
                  "transition-transform text-muted-foreground",
                  walletExpanded && "rotate-180",
                )}
              >
                <polyline points="6 9 12 15 18 9" />
              </svg>
            )}
          </div>
        </CardHeader>
        {(!hasActiveSession || walletExpanded) && <CardContent>
          {!wallet ? (
            <p className="text-sm text-muted-foreground">
              Your wallet is being set up...
            </p>
          ) : (
            <div className="flex flex-col sm:flex-row gap-6">
              {/* QR Code */}
              <div className="flex-shrink-0 flex justify-center">
                <div className="bg-white p-3 rounded-lg">
                  <QRCodeSVG value={wallet.address} size={160} />
                </div>
              </div>

              {/* Wallet Details */}
              <div className="flex-1 space-y-3">
                <div>
                  <p className="text-xs text-muted-foreground">Address</p>
                  <code className="text-sm break-all select-all">
                    {wallet.address}
                  </code>
                </div>

                <div>
                  <p className="text-xs text-muted-foreground">Chain</p>
                  <p className="text-sm font-medium uppercase">
                    {wallet.chain}
                  </p>
                </div>

                <div>
                  <p className="text-xs text-muted-foreground">WKC Balance</p>
                  <p className="text-2xl font-bold">
                    {balance
                      ? formatTokenAmount(balance.balance, balance.decimals)
                      : "---"}
                    <span className="text-sm font-normal text-muted-foreground ml-1">
                      WKC
                    </span>
                  </p>
                </div>

                <p className="text-xs text-muted-foreground">
                  Send WKC tokens to this address to deposit into your wallet.
                </p>
              </div>
            </div>
          )}

          {/* Deposit Calculator */}
          {wallet && (
            <div className="mt-6 p-4 rounded-lg border border-primary/30 bg-primary/5 space-y-3">
              <p className="text-sm font-semibold">Deposit Calculator</p>
              <p className="text-xs text-muted-foreground">
                WKC has a 4% token tax on transfers. Use this calculator to see
                how much you need to send to achieve your desired USD value.
              </p>
              <div className="flex items-end gap-3">
                <div className="flex-1">
                  <label className="text-xs text-muted-foreground mb-1 block">
                    Desired amount (USD)
                  </label>
                  <Input
                    type="number"
                    min="0"
                    step="any"
                    value={desiredUsd}
                    onChange={(e) => setDesiredUsd(e.target.value)}
                    placeholder="10000"
                    className="w-full"
                  />
                </div>
                <div className="flex-1">
                  <p className="text-xs text-muted-foreground mb-1">You need to send</p>
                  <p className="text-lg font-bold">
                    ${
                      desiredUsd && Number(desiredUsd) > 0
                        ? (Number(desiredUsd) / 0.96).toLocaleString(undefined, {
                            minimumFractionDigits: 2,
                            maximumFractionDigits: 2,
                          })
                        : "---"
                    }
                    <span className="text-sm font-normal text-muted-foreground ml-1">
                      USD worth of WKC
                    </span>
                  </p>
                </div>
              </div>
              {desiredUsd && Number(desiredUsd) > 0 && (
                <p className="text-xs text-muted-foreground">
                  4% tax ={" "}
                  <span className="font-medium text-foreground">
                    ${
                      ((Number(desiredUsd) / 0.96) - Number(desiredUsd)).toLocaleString(
                        undefined,
                        { minimumFractionDigits: 2, maximumFractionDigits: 2 }
                      )
                    }
                  </span>
                  {" "}will be deducted on transfer.
                </p>
              )}
            </div>
          )}

          {/* Deposit T&C / Disclaimer */}
          {wallet && (
            <div className="mt-4 p-4 rounded-lg border border-yellow-500/30 bg-yellow-500/5">
              <p className="text-sm font-semibold text-yellow-600 dark:text-yellow-400 mb-2">
                ⚠ Important Disclaimer
              </p>
              <p className="text-sm text-muted-foreground leading-relaxed">
                Please be reminded that the minimum deposit amount is{" "}
                <span className="font-semibold text-foreground">$10,000</span>.
                If, after liquidation, the total amount falls below this minimum,
                you may be required to add more funds to your wallet. For example,
                if you deposit $10,000 and the bot is only able to liquidate
                $9,700, you will need to add $300 to bring the total back to the
                $10,000 minimum.
              </p>
            </div>
          )}
        </CardContent>}
      </Card>
      <Card className="mb-8">
        <CardHeader>
          <CardTitle>Sessions Using My Wallet</CardTitle>
        </CardHeader>
        <CardContent>
          {sessions.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              No sessions have been created with your wallet yet.
            </p>
          ) : (
            <div className="space-y-3">
              {sessions.map((session) => {
                const progress =
                  session.total_amount !== "0"
                    ? Number(
                        (BigInt(session.amount_sold) * 10000n) /
                          BigInt(session.total_amount),
                      ) / 100
                    : 0;
                const remaining =
                  BigInt(session.total_amount) - BigInt(session.amount_sold);

                return (
                  <div
                    key={session.session_id}
                    className="p-3 rounded-lg bg-muted/50 space-y-2 cursor-pointer hover:bg-muted/80 transition-colors"
                    onClick={() => router.push(`/sessions/${session.session_id}`)}
                  >
                    <div className="flex items-center justify-between">
                      <div>
                        <p className="font-semibold text-sm">
                          {session.sell_token_symbol} →{" "}
                          {session.target_token_symbol}
                        </p>
                        <p className="text-xs text-muted-foreground">
                          {session.chain} · Wallet:{" "}
                          {shortenAddress(session.wallet_address)}
                        </p>
                      </div>
                      <span
                        className={cn(
                          "px-2 py-0.5 rounded-full text-xs font-medium",
                          session.status === "active" &&
                            "bg-green-500/10 text-green-500",
                          session.status === "paused" &&
                            "bg-yellow-500/10 text-yellow-500",
                          session.status === "completed" &&
                            "bg-blue-500/10 text-blue-500",
                          session.status === "pending" &&
                            "bg-muted text-muted-foreground",
                        )}
                      >
                        {session.status.toUpperCase()}
                      </span>
                    </div>
                    <div className="grid grid-cols-3 gap-2 text-xs">
                      <div>
                        <p className="text-muted-foreground">Total</p>
                        <p className="font-medium">{formatTokenAmount(session.total_amount, session.sell_token_decimals)} {session.sell_token_symbol}</p>
                      </div>
                      <div>
                        <p className="text-muted-foreground">Sold</p>
                        <p className="font-medium">{formatTokenAmount(session.amount_sold, session.sell_token_decimals)} {session.sell_token_symbol}</p>
                      </div>
                      <div>
                        <p className="text-muted-foreground">Remaining</p>
                        <p className="font-medium">{formatTokenAmount(remaining.toString(), session.sell_token_decimals)} {session.sell_token_symbol}</p>
                      </div>
                    </div>
                    <div className="flex items-center justify-between text-xs text-muted-foreground">
                      <span>Progress: {progress.toFixed(1)}%</span>
                      <span>POV: {session.pov_percent}%</span>
                    </div>
                    <div className="w-full h-1 bg-secondary rounded-full overflow-hidden">
                      <div
                        className="h-full bg-primary rounded-full transition-all"
                        style={{
                          width: `${Math.min(progress, 100)}%`,
                        }}
                      />
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </CardContent>
      </Card>

      {/* ── Request Refund ────────────────────────────────── */}
      <Card className="mb-8">
        <CardHeader className="flex flex-row items-center justify-between">
          <CardTitle>Refunds</CardTitle>
          {!showRefundForm && (
            <Button
              size="sm"
              variant="default"
              disabled={!wallet}
              onClick={() => setShowRefundForm(true)}
            >
              Request Refund
            </Button>
          )}
        </CardHeader>
        <CardContent>
          {showRefundForm && (
            <div className="mb-4 p-4 rounded-lg border border-primary/30 bg-primary/5 space-y-3">
              <p className="text-sm font-medium">How much WKC would you like refunded?</p>
              <form onSubmit={handleRefund} className="space-y-3">
                <div>
                  <Input
                    type="number"
                    step="any"
                    min="0"
                    placeholder="Amount in WKC"
                    value={refundAmount}
                    onChange={(e) => setRefundAmount(e.target.value)}
                  />
                  <p className="text-xs text-muted-foreground mt-1">
                    Current balance:{" "}
                    {balance
                      ? formatTokenAmount(balance.balance, balance.decimals)
                      : "---"}{" "}
                    WKC
                  </p>
                </div>
                <div className="flex gap-2">
                  <Button
                    type="submit"
                    size="sm"
                    disabled={!wallet || refundMutation.isPending || !refundAmount}
                  >
                    {refundMutation.isPending ? "Submitting..." : "Submit Request"}
                  </Button>
                  <Button
                    type="button"
                    size="sm"
                    variant="ghost"
                    onClick={() => {
                      setShowRefundForm(false);
                      setRefundAmount("");
                      setRefundError("");
                      setRefundSuccess("");
                    }}
                  >
                    Cancel
                  </Button>
                </div>
                {refundError && (
                  <p className="text-sm text-destructive">{refundError}</p>
                )}
                {refundSuccess && (
                  <p className="text-sm text-primary">{refundSuccess}</p>
                )}
              </form>
            </div>
          )}

          {/* Refund History */}
          {refunds.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              No refund requests yet.
            </p>
          ) : (
            <div className="space-y-2">
              {refunds.map((r) => (
                <div
                  key={r.refund_id}
                  className="flex items-center justify-between p-3 rounded-lg bg-muted/50"
                >
                  <div>
                    <p className="text-sm font-medium">
                      {formatTokenAmount(r.amount, WKC_DECIMALS)} WKC
                    </p>
                    <p className="text-xs text-muted-foreground">
                      {new Date(r.created_at).toLocaleDateString()}
                    </p>
                    {r.admin_note && (
                      <p className="text-xs text-muted-foreground mt-1">
                        Note: {r.admin_note}
                      </p>
                    )}
                  </div>
                  <span
                    className={cn(
                      "px-2 py-0.5 rounded-full text-xs font-medium",
                      r.status === "pending" &&
                        "bg-yellow-500/10 text-yellow-500",
                      r.status === "approved" &&
                        "bg-green-500/10 text-green-500",
                      r.status === "rejected" && "bg-red-500/10 text-red-500",
                      r.status === "completed" &&
                        "bg-blue-500/10 text-blue-500",
                    )}
                  >
                    {r.status.toUpperCase()}
                  </span>
                </div>
              ))}
            </div>
          )}
        </CardContent>
      </Card>

    </main>
  );
}
