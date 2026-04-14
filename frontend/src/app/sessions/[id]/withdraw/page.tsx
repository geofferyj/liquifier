"use client";

import { useParams, useRouter } from "next/navigation";
import { useEffect, useState } from "react";
import { useQuery, useMutation } from "@tanstack/react-query";
import { useAuthStore } from "@/lib/store";
import { api } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { CopyableAddress } from "@/components/ui/copyable-address";
import {
  formatTokenAmount,
  formatTokenAmountCompact,
  shortenAddress,
  formatUsd,
  tokenAmountToUsd,
  tokenAmountToNumber,
  cn,
} from "@/lib/utils";



export default function WithdrawSessionPage() {
  const router = useRouter();
  const params = useParams<{ id: string }>();
  const sessionId = params.id;
  const role = useAuthStore((s) => s.role);

  // Gate: admin only
  useEffect(() => {
    if (role && role !== "admin") {
      router.replace(`/sessions/${sessionId}`);
    }
  }, [role, router, sessionId]);

  // ── Session data ──────────────────────────────────────────
  const sessionQuery = useQuery({
    queryKey: ["session", sessionId],
    queryFn: () => api.getSession(sessionId),
    enabled: !!sessionId,
  });

  const session = sessionQuery.data;

  // ── Trades (for recovered USDT) ───────────────────────────
  const tradesQuery = useQuery({
    queryKey: ["session-trades", sessionId],
    queryFn: () => api.getSessionTrades(sessionId, 500),
    enabled: !!sessionId,
  });

  // Total received across all trades (raw bigint string in target token decimals)
  const totalReceivedRaw: string =
    tradesQuery.data?.total_received ??
    (tradesQuery.data?.trades ?? [])
      .reduce((sum, t) => sum + BigInt(t.received_amount), 0n)
      .toString() ??
    "0";

  // ── On-chain balances ─────────────────────────────────────
  // Fetch the session owner's wallets (admin view) to get the on-chain address
  const walletQuery = useQuery({
    queryKey: ["admin-user-wallets", session?.user_id],
    queryFn: () => api.adminGetUserWallets(session!.user_id),
    enabled: !!session?.user_id,
    staleTime: Infinity,
  });
  const walletAddress = walletQuery.data?.wallets.find(
    (w) => w.wallet_id === session?.wallet_id,
  )?.address;

  const usdtBalanceQuery = useQuery({
    queryKey: ["wallet-balance", session?.wallet_id, session?.target_token],
    queryFn: () =>
      api.getWalletBalance(session!.wallet_id, session!.target_token),
    enabled: !!session?.wallet_id && !!session?.target_token,
    refetchInterval: 15_000,
  });

  const bnbBalanceQuery = useQuery({
    queryKey: ["wallet-balance", session?.wallet_id, "native"],
    // No token address → backend fetches native (BNB) balance
    queryFn: () => api.getWalletBalance(session!.wallet_id),
    enabled: !!session?.wallet_id,
    refetchInterval: 15_000,
  });

  // ── USDT USD price ────────────────────────────────────────
  const usdtPriceQuery = useQuery({
    queryKey: ["token-usd-price", session?.chain, session?.target_token],
    queryFn: () => api.getTokenUsdPrice(session!.chain, session!.target_token),
    enabled: !!session?.chain && !!session?.target_token,
    staleTime: 60_000,
  });

  // ── Form state ────────────────────────────────────────────
  const [destinationWallet, setDestinationWallet] = useState("");
  const [withdrawAll, setWithdrawAll] = useState(false);
  const [customAmount, setCustomAmount] = useState("");
  const [totpCode, setTotpCode] = useState("");
  const [successTx, setSuccessTx] = useState<string | null>(null);

  // Pre-fill custom amount with recovered USDT when data arrives
  useEffect(() => {
    if (totalReceivedRaw && totalReceivedRaw !== "0" && !customAmount) {
      const decimals = session?.target_token_decimals ?? 18;
      const human = (Number(BigInt(totalReceivedRaw)) / 10 ** decimals).toFixed(
        6,
      );
      setCustomAmount(human);
    }
  }, [totalReceivedRaw, session?.target_token_decimals, customAmount]);

  const withdrawMutation = useMutation({
    mutationFn: async () => {
      const decimals = session?.target_token_decimals ?? 18;
      let amountWei: string | undefined;

      if (!withdrawAll) {
        // Convert human-readable amount to raw wei
        const parsed = parseFloat(customAmount);
        if (isNaN(parsed) || parsed <= 0) throw new Error("Enter a valid amount");
        const raw = BigInt(Math.floor(parsed * 10 ** decimals));
        amountWei = raw.toString();
      }
      // withdrawAll → omit amount so backend uses full balance

      return api.adminWithdrawSession(sessionId, {
        destination_wallet: destinationWallet.trim(),
        amount: amountWei,
        totp_code: totpCode.trim(),
      });
    },
    onSuccess: (data) => {
      setSuccessTx(data.tx_hash);
      setTotpCode("");
    },
  });

  // ── Render ────────────────────────────────────────────────
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
        <p className="text-destructive">Session not found.</p>
      </main>
    );
  }

  if (session.status !== "completed") {
    return (
      <main className="min-h-screen flex items-center justify-center">
        <p className="text-destructive">Withdrawals are only available for completed sessions.</p>
      </main>
    );
  }

  const usdtBalance = usdtBalanceQuery.data?.balance ?? "0";
  const usdtDecimals = usdtBalanceQuery.data?.decimals ?? session.target_token_decimals;
  const bnbBalance = bnbBalanceQuery.data?.balance ?? "0";
  const usdtPrice = usdtPriceQuery.data?.usd_price;

  const usdtBalanceHuman = (Number(BigInt(usdtBalance)) / 10 ** usdtDecimals).toFixed(4);
  const bnbBalanceHuman = (Number(BigInt(bnbBalance)) / 10 ** 18).toFixed(6);

  const recoveredHuman = (
    Number(BigInt(totalReceivedRaw)) /
    10 ** session.target_token_decimals
  ).toFixed(4);

  const canSubmit =
    destinationWallet.trim().startsWith("0x") &&
    destinationWallet.trim().length === 42 &&
    totpCode.trim().length === 6 &&
    (withdrawAll || (parseFloat(customAmount) > 0));

  return (
    <main className="min-h-screen p-8 max-w-2xl mx-auto space-y-6">
      {/* Header */}
      <div className="flex items-center gap-4">
        <Button variant="secondary" size="sm" onClick={() => router.back()}>
          ← Back
        </Button>
        <div>
          <h1 className="text-2xl font-bold">Withdraw Funds</h1>
          <p className="text-sm text-muted-foreground">
            Session {shortenAddress(session.session_id)} · {session.chain}
          </p>
        </div>
      </div>

      {/* Session Summary */}
      <Card>
        <CardHeader>
          <CardTitle>Session Summary</CardTitle>
        </CardHeader>
        <CardContent className="grid grid-cols-2 gap-4 text-sm">
          <div>
            <span className="text-muted-foreground">Pair</span>
            <p className="font-mono font-medium">
              {session.sell_token_symbol} → {session.target_token_symbol}
            </p>
          </div>
          <div>
            <span className="text-muted-foreground">Status</span>
            <p className="font-medium capitalize text-blue-500">
              {session.status}
            </p>
          </div>
          <div>
            <span className="text-muted-foreground">Total Sold</span>
            <p className="font-mono">
              {formatTokenAmountCompact(session.amount_sold, session.sell_token_decimals)}{" "}
              {session.sell_token_symbol}
            </p>
          </div>
          <div>
            <span className="text-muted-foreground">Session Wallet</span>
            {walletAddress ? (
              <CopyableAddress address={walletAddress} className="font-mono text-xs" />
            ) : (
              <p className="font-mono text-xs text-muted-foreground">Loading...</p>
            )}
          </div>
        </CardContent>
      </Card>

      {/* Balances */}
      <Card>
        <CardHeader>
          <CardTitle>Wallet Balances</CardTitle>
        </CardHeader>
        <CardContent className="grid grid-cols-1 md:grid-cols-3 gap-4">
          <div className="p-3 rounded-lg border border-border space-y-1">
            <p className="text-xs text-muted-foreground">Recovered {session.target_token_symbol}</p>
            <p className="text-xl font-bold font-mono text-primary">
              {tradesQuery.isLoading ? "..." : recoveredHuman}
            </p>
            <p className="text-xs text-muted-foreground">(from trades)</p>
          </div>
          <div className="p-3 rounded-lg border border-border space-y-1">
            <p className="text-xs text-muted-foreground">
              On-chain {session.target_token_symbol}
            </p>
            <p className="text-xl font-bold font-mono">
              {usdtBalanceQuery.isLoading ? "..." : usdtBalanceHuman}
            </p>
            {usdtPrice && (
              <p className="text-xs text-muted-foreground">
                ≈ {formatUsd(
                  tokenAmountToUsd(usdtBalance, usdtDecimals, usdtPrice),
                )}
              </p>
            )}
          </div>
          <div className="p-3 rounded-lg border border-border space-y-1">
            <p className="text-xs text-muted-foreground">BNB Balance</p>
            <p className="text-xl font-bold font-mono">
              {bnbBalanceQuery.isLoading ? "..." : bnbBalanceHuman}
            </p>
            <p className="text-xs text-muted-foreground">(for gas)</p>
          </div>
        </CardContent>
      </Card>

      {/* Success banner */}
      {successTx && (
        <div className="rounded-lg border border-green-500/40 bg-green-500/10 px-4 py-3 text-sm text-green-500 space-y-1">
          <p className="font-medium">Withdrawal submitted!</p>
          <p className="font-mono text-xs break-all">Tx: {successTx}</p>
          <a
            href={`https://bscscan.com/tx/${successTx}`}
            target="_blank"
            rel="noopener noreferrer"
            className="underline text-xs"
          >
            View on BscScan →
          </a>
        </div>
      )}

      {/* Withdrawal Form */}
      <Card>
        <CardHeader>
          <CardTitle>Withdrawal Details</CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          {/* Destination wallet */}
          <div className="space-y-1">
            <label className="text-sm font-medium">Destination Wallet</label>
            <Input
              placeholder="0x..."
              value={destinationWallet}
              onChange={(e) => setDestinationWallet(e.target.value)}
              className="font-mono"
            />
            {destinationWallet && !destinationWallet.startsWith("0x") && (
              <p className="text-xs text-destructive">Must start with 0x</p>
            )}
            {destinationWallet &&
              destinationWallet.startsWith("0x") &&
              destinationWallet.length !== 42 && (
                <p className="text-xs text-destructive">
                  Must be 42 characters (got {destinationWallet.length})
                </p>
              )}
          </div>

          {/* Amount */}
          <div className="space-y-2">
            <label className="text-sm font-medium">Amount ({session.target_token_symbol})</label>
            <div className="flex items-center gap-3">
              <label className="flex items-center gap-2 text-sm cursor-pointer select-none">
                <input
                  type="checkbox"
                  className="accent-primary"
                  checked={withdrawAll}
                  onChange={(e) => setWithdrawAll(e.target.checked)}
                />
                Withdraw everything (on-chain balance)
              </label>
            </div>
            {!withdrawAll && (
              <div className="space-y-1">
                <Input
                  type="number"
                  step="0.000001"
                  min="0"
                  placeholder={recoveredHuman}
                  value={customAmount}
                  onChange={(e) => setCustomAmount(e.target.value)}
                  className="font-mono"
                />
                <p className="text-xs text-muted-foreground">
                  Default is the recovered {session.target_token_symbol} from trades (
                  {recoveredHuman}). On-chain balance: {usdtBalanceHuman}.
                </p>
              </div>
            )}
            {withdrawAll && (
              <p className="text-xs text-muted-foreground">
                Will withdraw the entire on-chain {session.target_token_symbol} balance: {usdtBalanceHuman}
              </p>
            )}
          </div>

          {/* TOTP */}
          <div className="space-y-1">
            <label className="text-sm font-medium">Authenticator Code</label>
            <Input
              type="text"
              inputMode="numeric"
              pattern="[0-9]*"
              maxLength={6}
              placeholder="6-digit code"
              value={totpCode}
              onChange={(e) =>
                setTotpCode(e.target.value.replace(/\D/g, "").slice(0, 6))
              }
              className="font-mono w-40"
            />
            <p className="text-xs text-muted-foreground">
              Required: enter your current TOTP code to authorize this withdrawal.
            </p>
          </div>

          {/* Error */}
          {withdrawMutation.isError && (
            <div className="rounded-lg border border-destructive/40 bg-destructive/10 px-4 py-3 text-sm text-destructive">
              {withdrawMutation.error instanceof Error
                ? withdrawMutation.error.message
                : "Withdrawal failed. Please try again."}
            </div>
          )}

          {/* Submit */}
          <Button
            className="w-full"
            disabled={!canSubmit || withdrawMutation.isPending}
            onClick={() => withdrawMutation.mutate()}
          >
            {withdrawMutation.isPending
              ? "Submitting..."
              : `Withdraw ${withdrawAll ? "All" : customAmount || ""} ${session.target_token_symbol}`}
          </Button>
        </CardContent>
      </Card>
    </main>
  );
}
