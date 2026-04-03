"use client";

import { useState, useEffect, useCallback } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { api } from "@/lib/api";
import type { Chain, PoolInfo, SwapPath } from "@/lib/types";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";

// ─────────────────────────────────────────────────────────────
// Wizard Steps
// ─────────────────────────────────────────────────────────────

const CHAIN_LABELS: Record<string, string> = {
  ethereum: "Ethereum",
  base: "Base",
  arbitrum: "Arbitrum",
  bsc: "BSC",
  polygon: "Polygon",
  optimism: "Optimism",
};

interface TokenMeta {
  name: string;
  symbol: string;
  decimals: number;
  loading: boolean;
  error: boolean;
}

interface WizardState {
  chain: Chain;
  walletId: string;
  sellToken: string;
  sellTokenMeta: TokenMeta | null;
  targetToken: string;
  targetTokenMeta: TokenMeta | null;
  totalAmount: string;
  povPercent: number;
  maxPriceImpact: number;
  minBuyTriggerUsd: number;
  discoveredPools: PoolInfo[];
  selectedPoolAddresses: Set<string>;
  computedPaths: SwapPath[];
}

const INITIAL_STATE: WizardState = {
  chain: "ethereum",
  walletId: "",
  sellToken: "",
  sellTokenMeta: null,
  targetToken: "",
  targetTokenMeta: null,
  totalAmount: "",
  povPercent: 10,
  maxPriceImpact: 1.0,
  minBuyTriggerUsd: 100,
  discoveredPools: [],
  selectedPoolAddresses: new Set(),
  computedPaths: [],
};

function isValidAddress(addr: string): boolean {
  return /^0x[0-9a-fA-F]{40}$/.test(addr);
}

/** Convert a human-readable amount (e.g. "1.5") to wei string given decimals */
function toWei(amount: string, decimals: number): string {
  if (!amount || isNaN(Number(amount))) return "0";
  const [whole = "0", frac = ""] = amount.split(".");
  const paddedFrac = frac.padEnd(decimals, "0").slice(0, decimals);
  const raw = (whole + paddedFrac).replace(/^0+/, "") || "0";
  return raw;
}

/** Convert a wei string to human-readable amount given decimals */
function fromWei(wei: string, decimals: number): string {
  if (!wei || wei === "0") return "0";
  const padded = wei.padStart(decimals + 1, "0");
  const whole = padded.slice(0, padded.length - decimals) || "0";
  const frac = padded.slice(padded.length - decimals);
  const trimmed = frac.replace(/0+$/, "");
  return trimmed ? `${whole}.${trimmed}` : whole;
}

function poolUsdValue(pool: PoolInfo, sellDecimals: number): number | null {
  // Compute total USD value of pool's token holdings
  const amt0 = pool.reserve0 || pool.balance0;
  const amt1 = pool.reserve1 || pool.balance1;
  let total = 0;
  let hasPrice = false;
  if (amt0 && pool.token0_price_usd > 0) {
    total += parseFloat(fromWei(amt0, sellDecimals)) * pool.token0_price_usd;
    hasPrice = true;
  }
  if (amt1 && pool.token1_price_usd > 0) {
    // Use 18 as default for paired token (base tokens are typically 18 or we approximate)
    total += parseFloat(fromWei(amt1, 18)) * pool.token1_price_usd;
    hasPrice = true;
  }
  return hasPrice ? total : null;
}

function formatUsd(value: number): string {
  if (value >= 1_000_000) return `$${(value / 1_000_000).toFixed(2)}M`;
  if (value >= 1_000) return `$${(value / 1_000).toFixed(2)}K`;
  return `$${value.toFixed(2)}`;
}

export default function SessionCreatePage() {
  const [step, setStep] = useState(0);
  const [form, setForm] = useState<WizardState>(INITIAL_STATE);

  const [walletBalanceWei, setWalletBalanceWei] = useState<string | null>(null);
  const [balanceLoading, setBalanceLoading] = useState(false);

  const walletsQuery = useQuery({
    queryKey: ["wallets"],
    queryFn: () => api.listWallets(),
  });

  const chainsQuery = useQuery({
    queryKey: ["chains"],
    queryFn: () => api.listChains(),
  });

  const enabledChains = (chainsQuery.data?.chains ?? []) as { name: string; chain_id: number }[];

  const update = useCallback(
    (partial: Partial<WizardState>) => setForm((prev) => ({ ...prev, ...partial })),
    [],
  );

  // ── Auto-fetch sell token metadata ──
  useEffect(() => {
    if (!isValidAddress(form.sellToken)) {
      update({ sellTokenMeta: null });
      return;
    }
    update({ sellTokenMeta: { name: "", symbol: "", decimals: 18, loading: true, error: false } });
    api
      .getTokenMetadata(form.chain, form.sellToken)
      .then((meta) =>
        update({
          sellTokenMeta: { name: meta.name, symbol: meta.symbol, decimals: meta.decimals, loading: false, error: false },
        }),
      )
      .catch(() =>
        update({ sellTokenMeta: { name: "", symbol: "", decimals: 18, loading: false, error: true } }),
      );
  }, [form.sellToken, form.chain, update]);

  // ── Auto-fetch target token metadata ──
  useEffect(() => {
    if (!isValidAddress(form.targetToken)) {
      update({ targetTokenMeta: null });
      return;
    }
    update({ targetTokenMeta: { name: "", symbol: "", decimals: 18, loading: true, error: false } });
    api
      .getTokenMetadata(form.chain, form.targetToken)
      .then((meta) =>
        update({
          targetTokenMeta: { name: meta.name, symbol: meta.symbol, decimals: meta.decimals, loading: false, error: false },
        }),
      )
      .catch(() =>
        update({ targetTokenMeta: { name: "", symbol: "", decimals: 18, loading: false, error: true } }),
      );
  }, [form.targetToken, form.chain, update]);

  // ── Fetch wallet balance for sell token ──
  useEffect(() => {
    if (!form.walletId || !isValidAddress(form.sellToken)) {
      setWalletBalanceWei(null);
      return;
    }
    setBalanceLoading(true);
    api
      .getWalletBalance(form.walletId, form.sellToken)
      .then((res) => setWalletBalanceWei(res.balance))
      .catch(() => setWalletBalanceWei(null))
      .finally(() => setBalanceLoading(false));
  }, [form.walletId, form.sellToken]);

  const sellDecimals = form.sellTokenMeta?.decimals ?? 18;
  const balanceHuman = walletBalanceWei ? fromWei(walletBalanceWei, sellDecimals) : null;

  const setAmountPercent = (pct: number) => {
    if (!walletBalanceWei) return;
    const raw = BigInt(walletBalanceWei) * BigInt(pct) / BigInt(100);
    update({ totalAmount: fromWei(raw.toString(), sellDecimals) });
  };

  const poolsMutation = useMutation({
    mutationFn: () =>
      api.discoverPools({
        chain: form.chain,
        token_address: form.sellToken,
      }),
    onSuccess: (data) => {
      // Sort by USD value descending (pools with prices first)
      const sorted = [...data.pools].sort((a, b) => {
        const usdA = poolUsdValue(a, form.sellTokenMeta?.decimals ?? 18) ?? -1;
        const usdB = poolUsdValue(b, form.sellTokenMeta?.decimals ?? 18) ?? -1;
        return usdB - usdA;
      });
      update({ discoveredPools: sorted, selectedPoolAddresses: new Set(), computedPaths: [] });
    },
  });

  const pathsMutation = useMutation({
    mutationFn: () =>
      api.getSwapPaths({
        chain: form.chain,
        sell_token: form.sellToken,
        target_token: form.targetToken,
        amount: toWei(form.totalAmount, sellDecimals),
      }),
    onSuccess: (data) => {
      update({ computedPaths: data.paths });
    },
  });

  const createMutation = useMutation({
    mutationFn: () => {
      const selectedPools = form.discoveredPools.filter((p) =>
        form.selectedPoolAddresses.has(p.pool_address),
      );
      const bestPath = form.computedPaths[0] ?? null;
      return api.createSession({
        wallet_id: form.walletId,
        chain: form.chain,
        sell_token: form.sellToken,
        sell_token_symbol: form.sellTokenMeta?.symbol ?? "",
        sell_token_decimals: form.sellTokenMeta?.decimals ?? 18,
        target_token: form.targetToken,
        target_token_symbol: form.targetTokenMeta?.symbol ?? "",
        target_token_decimals: form.targetTokenMeta?.decimals ?? 18,
        total_amount: toWei(form.totalAmount, sellDecimals),
        pov_percent: form.povPercent,
        max_price_impact: form.maxPriceImpact,
        min_buy_trigger_usd: form.minBuyTriggerUsd,
        swap_path_json: bestPath ? JSON.stringify(bestPath) : undefined,
        pools: selectedPools,
      });
    },
    onSuccess: (session) => {
      window.location.href = `/sessions/${session.session_id}`;
    },
  });

  const togglePool = (address: string) => {
    setForm((prev) => {
      const next = new Set(prev.selectedPoolAddresses);
      if (next.has(address)) next.delete(address);
      else next.add(address);
      return { ...prev, selectedPoolAddresses: next, computedPaths: [] };
    });
  };

  // Check if any selected pool has a direct pair with target token
  const selectedPools = form.discoveredPools.filter((p) =>
    form.selectedPoolAddresses.has(p.pool_address),
  );
  const targetLower = form.targetToken.toLowerCase();
  const needsRouting =
    selectedPools.length > 0 &&
    isValidAddress(form.targetToken) &&
    !selectedPools.some(
      (p) =>
        p.token0.toLowerCase() === targetLower ||
        p.token1.toLowerCase() === targetLower,
    );

  const wallets = walletsQuery.data?.wallets ?? [];

  return (
    <main className="min-h-screen p-8 max-w-3xl mx-auto">
      <h1 className="text-3xl font-bold mb-8">Create Liquifier Session</h1>

      {/* Step indicators */}
      <div className="flex gap-2 mb-8">
        {["Token & Pool Setup", "Advanced Settings", "Review"].map(
          (label, i) => (
            <div
              key={label}
              className={cn(
                "flex-1 h-1 rounded-full transition-colors",
                i <= step ? "bg-primary" : "bg-secondary",
              )}
            />
          ),
        )}
      </div>

      {/* ── Step 0: Token, Pool & Wallet Setup ───────────── */}
      {step === 0 && (
        <Card>
          <CardHeader>
            <CardTitle>Token & Pool Setup</CardTitle>
          </CardHeader>
          <CardContent className="space-y-4">
            {/* Chain selector */}
            <div>
              <label className="text-sm text-muted-foreground mb-1 block">Chain</label>
              <div className="flex flex-wrap gap-2">
                {enabledChains.map((c) => (
                  <button
                    key={c.name}
                    onClick={() =>
                      update({
                        chain: c.name as Chain,
                        discoveredPools: [],
                        selectedPoolAddresses: new Set(),
                        computedPaths: [],
                      })
                    }
                    className={cn(
                      "px-3 py-1.5 rounded-md text-sm border transition-colors",
                      form.chain === c.name
                        ? "border-primary bg-primary/10 text-primary"
                        : "border-border hover:border-muted-foreground",
                    )}
                  >
                    {CHAIN_LABELS[c.name] ?? c.name}
                  </button>
                ))}
              </div>
            </div>

            {/* Wallet selector */}
            <div>
              <label className="text-sm text-muted-foreground mb-1 block">Source Wallet</label>
              {wallets.length === 0 ? (
                <p className="text-sm text-muted-foreground">
                  No wallets found.{" "}
                  <button
                    onClick={() => api.createWallet().then(() => walletsQuery.refetch())}
                    className="text-primary underline"
                  >
                    Create one
                  </button>
                </p>
              ) : (
                <select
                  value={form.walletId}
                  onChange={(e) => update({ walletId: e.target.value })}
                  className="w-full rounded-md border border-input bg-transparent px-3 py-2 text-sm"
                >
                  <option value="">Select wallet...</option>
                  {wallets.map((w) => (
                    <option key={w.wallet_id} value={w.wallet_id}>
                      {w.address}
                    </option>
                  ))}
                </select>
              )}
            </div>

            {/* Sell Token */}
            <div>
              <label className="text-sm text-muted-foreground mb-1 block">
                Token to Sell (Contract Address)
              </label>
              <Input
                placeholder="0x..."
                value={form.sellToken}
                onChange={(e) =>
                  update({
                    sellToken: e.target.value,
                    discoveredPools: [],
                    selectedPoolAddresses: new Set(),
                    computedPaths: [],
                  })
                }
              />
              {form.sellTokenMeta && (
                <TokenMetaCard meta={form.sellTokenMeta} />
              )}
            </div>

            {/* Pool Discovery & Selection */}
            {isValidAddress(form.sellToken) && (
              <div className="space-y-3">
                <Button
                  variant="outline"
                  onClick={() => poolsMutation.mutate()}
                  disabled={poolsMutation.isPending}
                  className="w-full"
                >
                  {poolsMutation.isPending
                    ? "Discovering Pools..."
                    : `Discover Pools on ${form.chain}`}
                </Button>

                {form.discoveredPools.length > 0 && (
                  <div className="border rounded-lg p-4 space-y-2">
                    <div className="flex items-center justify-between">
                      <span className="text-sm font-medium">
                        {form.discoveredPools.length} Pool
                        {form.discoveredPools.length !== 1 ? "s" : ""} Found —
                        select the pools to watch
                      </span>
                      <div className="flex gap-2 text-xs">
                        <span className="px-2 py-0.5 rounded bg-blue-500/10 text-blue-500">
                          V2: {form.discoveredPools.filter((p) => p.pool_type === "v2").length}
                        </span>
                        <span className="px-2 py-0.5 rounded bg-purple-500/10 text-purple-500">
                          V3: {form.discoveredPools.filter((p) => p.pool_type === "v3").length}
                        </span>
                      </div>
                    </div>

                    <div className="max-h-60 overflow-y-auto space-y-1">
                      {form.discoveredPools.map((pool) => {
                        const selected = form.selectedPoolAddresses.has(pool.pool_address);
                        return (
                          <button
                            key={pool.pool_address}
                            onClick={() => togglePool(pool.pool_address)}
                            className={cn(
                              "w-full flex items-center justify-between text-xs p-2 rounded transition-colors",
                              selected
                                ? "bg-primary/10 border border-primary"
                                : "bg-muted/50 border border-transparent hover:border-muted-foreground",
                            )}
                          >
                            <div className="flex items-center gap-2">
                              <input
                                type="checkbox"
                                checked={selected}
                                readOnly
                                className="rounded"
                              />
                              <span
                                className={cn(
                                  "px-1.5 py-0.5 rounded font-medium",
                                  pool.pool_type === "v2"
                                    ? "bg-blue-500/10 text-blue-500"
                                    : "bg-purple-500/10 text-purple-500",
                                )}
                              >
                                {pool.pool_type.toUpperCase()}
                              </span>
                              <span className="text-muted-foreground">{pool.dex_name}</span>
                              {pool.fee_tier > 0 && (
                                <span className="text-muted-foreground">
                                  ({(pool.fee_tier / 10000).toFixed(2)}%)
                                </span>
                              )}
                            </div>
                            <div className="flex items-center gap-3">
                              {(() => {
                                const usd = poolUsdValue(pool, sellDecimals);
                                return usd !== null ? (
                                  <span className="text-green-500 font-medium">{formatUsd(usd)}</span>
                                ) : null;
                              })()}
                              <code className="text-muted-foreground">
                                {pool.pool_address.slice(0, 8)}...{pool.pool_address.slice(-6)}
                              </code>
                            </div>
                          </button>
                        );
                      })}
                    </div>
                  </div>
                )}

                {poolsMutation.isError && (
                  <p className="text-sm text-destructive">
                    Failed to discover pools. Check the token address.
                  </p>
                )}
              </div>
            )}

            {/* Target Token */}
            <div>
              <label className="text-sm text-muted-foreground mb-1 block">
                Target Token (Contract Address)
              </label>
              <Input
                placeholder="0x..."
                value={form.targetToken}
                onChange={(e) => update({ targetToken: e.target.value, computedPaths: [] })}
              />
              {form.targetTokenMeta && (
                <TokenMetaCard meta={form.targetTokenMeta} />
              )}
            </div>

            {/* Routing info — auto-compute path if needed */}
            {selectedPools.length > 0 && isValidAddress(form.targetToken) && (
              <div className="border rounded-lg p-4 space-y-2">
                {needsRouting ? (
                  <>
                    <p className="text-sm text-amber-500 font-medium">
                      Selected pool(s) don&apos;t directly contain the target token — a swap route is needed.
                    </p>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => pathsMutation.mutate()}
                      disabled={pathsMutation.isPending}
                    >
                      {pathsMutation.isPending ? "Computing Route..." : "Compute Swap Route"}
                    </Button>
                    {form.computedPaths.length > 0 && (
                      <div className="space-y-1 mt-2">
                        {form.computedPaths.slice(0, 3).map((path) => (
                          <div
                            key={path.rank}
                            className="flex items-center gap-2 text-xs p-2 rounded bg-muted/50"
                          >
                            <span className="font-medium">Route #{path.rank}</span>
                            <span className="text-muted-foreground">
                              {path.hops.length} hop{path.hops.length > 1 ? "s" : ""}
                            </span>
                            <span className="text-muted-foreground">
                              Impact: {path.estimated_price_impact.toFixed(2)}%
                            </span>
                            <div className="flex items-center gap-1 ml-auto">
                              {path.hop_tokens.map((token, i) => (
                                <span key={i} className="flex items-center gap-1">
                                  {i > 0 && <span className="text-primary">→</span>}
                                  <code>{token.slice(0, 6)}...{token.slice(-4)}</code>
                                </span>
                              ))}
                            </div>
                          </div>
                        ))}
                      </div>
                    )}
                    {pathsMutation.isError && (
                      <p className="text-xs text-destructive">Failed to compute route.</p>
                    )}
                  </>
                ) : (
                  <p className="text-sm text-green-500 font-medium">
                    ✓ Direct pair — selected pool(s) contain the target token. No extra routing needed.
                  </p>
                )}
              </div>
            )}

            {/* Total Amount */}
            <div>
              <label className="text-sm text-muted-foreground mb-1 block">
                Total Amount to Sell{form.sellTokenMeta?.symbol ? ` (${form.sellTokenMeta.symbol})` : ""}
              </label>
              <Input
                type="text"
                inputMode="decimal"
                placeholder="1000.0"
                value={form.totalAmount}
                onChange={(e) => update({ totalAmount: e.target.value })}
              />
              {/* Preset buttons */}
              <div className="flex items-center gap-2 mt-2">
                <button
                  type="button"
                  onClick={() => setAmountPercent(25)}
                  disabled={!walletBalanceWei}
                  className="px-2 py-1 text-xs rounded border border-border hover:border-primary disabled:opacity-40 transition-colors"
                >
                  25%
                </button>
                <button
                  type="button"
                  onClick={() => setAmountPercent(50)}
                  disabled={!walletBalanceWei}
                  className="px-2 py-1 text-xs rounded border border-border hover:border-primary disabled:opacity-40 transition-colors"
                >
                  50%
                </button>
                <button
                  type="button"
                  onClick={() => setAmountPercent(100)}
                  disabled={!walletBalanceWei}
                  className="px-2 py-1 text-xs rounded border border-border hover:border-primary disabled:opacity-40 transition-colors"
                >
                  Max
                </button>
                {balanceLoading && (
                  <span className="text-xs text-muted-foreground animate-pulse">Loading balance...</span>
                )}
                {balanceHuman && !balanceLoading && (
                  <span className="text-xs text-muted-foreground ml-auto">
                    Balance: {balanceHuman} {form.sellTokenMeta?.symbol ?? ""}
                  </span>
                )}
              </div>
            </div>

            <Button
              onClick={() => setStep(1)}
              disabled={
                !form.walletId ||
                !isValidAddress(form.sellToken) ||
                !isValidAddress(form.targetToken) ||
                !form.totalAmount ||
                form.selectedPoolAddresses.size === 0 ||
                (needsRouting && form.computedPaths.length === 0)
              }
              className="w-full"
            >
              Next: Advanced Settings
            </Button>
          </CardContent>
        </Card>
      )}

      {/* ── Step 1: Advanced Settings ────────────────────── */}
      {step === 1 && (
        <Card>
          <CardHeader>
            <CardTitle>Advanced Settings</CardTitle>
          </CardHeader>
          <CardContent className="space-y-4">
            <div>
              <label className="text-sm text-muted-foreground mb-1 block">
                POV Percentage (% of incoming buy volume to sell)
              </label>
              <div className="flex items-center gap-3">
                <input
                  type="range"
                  min={1}
                  max={100}
                  step={0.5}
                  value={form.povPercent}
                  onChange={(e) => update({ povPercent: parseFloat(e.target.value) })}
                  className="flex-1"
                />
                <span className="text-sm font-mono w-16 text-right">{form.povPercent}%</span>
              </div>
            </div>

            <div>
              <label className="text-sm text-muted-foreground mb-1 block">
                Max Price Impact (%)
              </label>
              <Input
                type="number"
                step="0.01"
                min="0.01"
                max="50"
                value={form.maxPriceImpact}
                onChange={(e) => update({ maxPriceImpact: parseFloat(e.target.value) || 0 })}
              />
              <p className="text-xs text-muted-foreground mt-1">
                Trades exceeding this impact will be skipped.
              </p>
            </div>

            <div>
              <label className="text-sm text-muted-foreground mb-1 block">
                Minimum Buy Trigger (USD)
              </label>
              <Input
                type="number"
                step="1"
                min="1"
                value={form.minBuyTriggerUsd}
                onChange={(e) => update({ minBuyTriggerUsd: parseFloat(e.target.value) || 0 })}
              />
              <p className="text-xs text-muted-foreground mt-1">
                Only react to buy events larger than this USD value.
              </p>
            </div>

            <div className="flex gap-3 pt-4">
              <Button variant="outline" onClick={() => setStep(0)}>
                Back
              </Button>
              <Button onClick={() => setStep(2)} className="flex-1">
                Review & Create
              </Button>
            </div>
          </CardContent>
        </Card>
      )}

      {/* ── Step 2: Review ───────────────────────────────── */}
      {step === 2 && (
        <Card>
          <CardHeader>
            <CardTitle>Review Session</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3">
            <div className="grid grid-cols-2 gap-3 text-sm">
              <div className="text-muted-foreground">Chain</div>
              <div className="font-mono">{form.chain}</div>

              <div className="text-muted-foreground">Sell Token</div>
              <div className="font-mono">
                {form.sellTokenMeta?.symbol || "?"} ({form.sellToken.slice(0, 10)}...)
              </div>

              <div className="text-muted-foreground">Target Token</div>
              <div className="font-mono">
                {form.targetTokenMeta?.symbol || "?"} ({form.targetToken.slice(0, 10)}...)
              </div>

              <div className="text-muted-foreground">Total Amount</div>
              <div className="font-mono">{form.totalAmount} {form.sellTokenMeta?.symbol ?? ""}</div>

              <div className="text-muted-foreground">POV %</div>
              <div className="font-mono">{form.povPercent}%</div>

              <div className="text-muted-foreground">Max Price Impact</div>
              <div className="font-mono">{form.maxPriceImpact}%</div>

              <div className="text-muted-foreground">Min Buy Trigger</div>
              <div className="font-mono">${form.minBuyTriggerUsd}</div>

              <div className="text-muted-foreground">Selected Pools</div>
              <div className="font-mono">
                {form.selectedPoolAddresses.size} pool
                {form.selectedPoolAddresses.size !== 1 ? "s" : ""}
              </div>

              <div className="text-muted-foreground">Routing</div>
              <div className="font-mono">
                {form.computedPaths.length > 0
                  ? `${form.computedPaths[0].hops.length} hop${form.computedPaths[0].hops.length > 1 ? "s" : ""}`
                  : "Direct"}
              </div>
            </div>

            <div className="flex gap-3 pt-6">
              <Button variant="outline" onClick={() => setStep(1)}>
                Back
              </Button>
              <Button
                onClick={() => createMutation.mutate()}
                disabled={createMutation.isPending}
                className="flex-1"
              >
                {createMutation.isPending ? "Creating..." : "Create Session"}
              </Button>
            </div>

            {createMutation.isError && (
              <p className="text-sm text-destructive mt-2">
                Failed to create session. Please try again.
              </p>
            )}
          </CardContent>
        </Card>
      )}
    </main>
  );
}

// ─────────────────────────────────────────────────────────────
// Token Metadata Card
// ─────────────────────────────────────────────────────────────

function TokenMetaCard({ meta }: { meta: TokenMeta }) {
  if (meta.loading) {
    return (
      <div className="mt-1 px-3 py-2 rounded-md bg-muted/50 text-xs text-muted-foreground animate-pulse">
        Fetching token info...
      </div>
    );
  }
  if (meta.error) {
    return (
      <div className="mt-1 px-3 py-2 rounded-md bg-destructive/10 text-xs text-destructive">
        Could not fetch token metadata. Verify address and chain.
      </div>
    );
  }
  return (
    <div className="mt-1 px-3 py-2 rounded-md bg-green-500/10 text-xs flex items-center gap-3">
      <span className="font-medium text-green-600">{meta.symbol}</span>
      <span className="text-muted-foreground">{meta.name}</span>
      <span className="text-muted-foreground ml-auto">{meta.decimals} decimals</span>
    </div>
  );
}
