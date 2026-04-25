"use client";

import { useState, useEffect, useCallback } from "react";
import { useRouter } from "next/navigation";
import { useMutation, useQuery } from "@tanstack/react-query";
import { api } from "@/lib/api";
import { useAuthStore } from "@/lib/store";
import type { AdminWallet, Chain, PoolInfo, SwapPath } from "@/lib/types";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import { CopyableAddress } from "@/components/ui/copyable-address";

// ──────────────────────────────────────────────────────
// Wizard Steps
// ──────────────────────────────────────────────────────

const CHAIN_LABELS: Record<string, string> = {
  ethereum: "Ethereum",
  base: "Base",
  arbitrum: "Arbitrum",
  bsc: "BSC",
  polygon: "Polygon",
  optimism: "Optimism",
};

const NATIVE_TOKEN_PLACEHOLDER = "0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
const BSC_WBNB_ADDRESS = "0xbb4CdB9CBd36B01bD1cBaEBF2De08d9173bc095c";
const BSC_USDT_ADDRESS = "0x55d398326f99059fF775485246999027B3197955";

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
  minMarketCapUsd: number;
  // Pool discovery & selection — lives in step 1
  discoveredPools: PoolInfo[];
  selectedPoolAddresses: Set<string>;
  // Per-pool computed swap paths: pool_address → SwapPath
  poolPaths: Record<string, SwapPath>;
  pathLoadingPools: Set<string>;
  pathErrorPools: Set<string>;
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
  minMarketCapUsd: 50_000_000,
  discoveredPools: [],
  selectedPoolAddresses: new Set(),
  poolPaths: {},
  pathLoadingPools: new Set(),
  pathErrorPools: new Set(),
};

function isValidAddress(addr: string): boolean {
  return /^0x[0-9a-fA-F]{40}$/.test(addr);
}

function isNativePlaceholder(addr: string): boolean {
  return addr.trim().toLowerCase() === NATIVE_TOKEN_PLACEHOLDER;
}

function normalizeTokenForProcessing(chain: Chain, addr: string): string {
  const token = addr.trim();
  if (!token) return token;
  if (chain === "bsc" && isNativePlaceholder(token)) {
    return BSC_WBNB_ADDRESS;
  }
  return token;
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
  const amt0 = pool.reserve0 || pool.balance0;
  const amt1 = pool.reserve1 || pool.balance1;
  let total = 0;
  let hasPrice = false;
  if (amt0 && pool.token0_price_usd > 0) {
    total += parseFloat(fromWei(amt0, sellDecimals)) * pool.token0_price_usd;
    hasPrice = true;
  }
  if (amt1 && pool.token1_price_usd > 0) {
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
  const router = useRouter();
  const role = useAuthStore((s) => s.role);

  useEffect(() => {
    if (role && role !== "admin") router.replace("/my-dashboard");
  }, [role, router]);

  const [step, setStep] = useState(0);
  const [form, setForm] = useState<WizardState>(INITIAL_STATE);

  const [walletBalanceWei, setWalletBalanceWei] = useState<string | null>(null);
  const [balanceLoading, setBalanceLoading] = useState(false);

  const walletsQuery = useQuery({
    queryKey: role === "admin" ? ["admin-all-wallets"] : ["wallets"],
    queryFn: () =>
      role === "admin" ? api.adminListAllWallets() : api.listWallets(),
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

  const normalizedSellToken = normalizeTokenForProcessing(form.chain, form.sellToken);
  const normalizedTargetToken = normalizeTokenForProcessing(form.chain, form.targetToken);

  useEffect(() => {
    if (enabledChains.length === 0) return;
    const currentExists = enabledChains.some((c) => c.name === form.chain);
    if (!currentExists) {
      update({
        chain: enabledChains[0].name as Chain,
        sellTokenMeta: null,
        targetTokenMeta: null,
      });
    }
  }, [enabledChains, form.chain, update]);

  // ── Auto-fetch sell token metadata ──
  useEffect(() => {
    if (!isValidAddress(form.sellToken)) {
      update({ sellTokenMeta: null });
      return;
    }
    if (form.chain === "bsc" && isNativePlaceholder(form.sellToken)) {
      update({
        sellTokenMeta: {
          name: "BNB",
          symbol: "BNB",
          decimals: 18,
          loading: false,
          error: false,
        },
      });
      return;
    }
    update({ sellTokenMeta: { name: "", symbol: "", decimals: 18, loading: true, error: false } });
    api
      .getTokenMetadata(form.chain, normalizedSellToken)
      .then((meta) =>
        update({
          sellTokenMeta: {
            name: meta.name,
            symbol: meta.symbol,
            decimals: meta.decimals,
            loading: false,
            error: false,
          },
        }),
      )
      .catch(() =>
        update({
          sellTokenMeta: { name: "", symbol: "", decimals: 18, loading: false, error: true },
        }),
      );
  }, [form.sellToken, form.chain, normalizedSellToken, update]);

  // ── Auto-fetch target token metadata ──
  useEffect(() => {
    if (!isValidAddress(form.targetToken)) {
      update({ targetTokenMeta: null });
      return;
    }
    if (form.chain === "bsc" && isNativePlaceholder(form.targetToken)) {
      update({
        targetTokenMeta: {
          name: "BNB",
          symbol: "BNB",
          decimals: 18,
          loading: false,
          error: false,
        },
      });
      return;
    }
    update({ targetTokenMeta: { name: "", symbol: "", decimals: 18, loading: true, error: false } });
    api
      .getTokenMetadata(form.chain, normalizedTargetToken)
      .then((meta) =>
        update({
          targetTokenMeta: {
            name: meta.name,
            symbol: meta.symbol,
            decimals: meta.decimals,
            loading: false,
            error: false,
          },
        }),
      )
      .catch(() =>
        update({
          targetTokenMeta: { name: "", symbol: "", decimals: 18, loading: false, error: true },
        }),
      );
  }, [form.targetToken, form.chain, normalizedTargetToken, update]);

  // ── Fetch wallet balance for sell token ──
  useEffect(() => {
    if (!form.walletId || !isValidAddress(form.sellToken)) {
      setWalletBalanceWei(null);
      return;
    }
    setBalanceLoading(true);
    api
      .getWalletBalance(form.walletId, normalizedSellToken)
      .then((res) => setWalletBalanceWei(res.balance))
      .catch(() => setWalletBalanceWei(null))
      .finally(() => setBalanceLoading(false));
  }, [form.walletId, form.sellToken, normalizedSellToken]);

  const sellDecimals = form.sellTokenMeta?.decimals ?? 18;
  const balanceHuman = walletBalanceWei ? fromWei(walletBalanceWei, sellDecimals) : null;

  const amountExceedsBalance =
    !!walletBalanceWei &&
    !!form.totalAmount &&
    !isNaN(Number(form.totalAmount)) &&
    BigInt(toWei(form.totalAmount, sellDecimals)) > BigInt(walletBalanceWei);

  const setAmountPercent = (pct: number) => {
    if (!walletBalanceWei) return;
    const raw = (BigInt(walletBalanceWei) * BigInt(pct)) / BigInt(100);
    update({ totalAmount: fromWei(raw.toString(), sellDecimals) });
  };

  // ── Pool discovery mutation ──
  const poolsMutation = useMutation({
    mutationFn: () =>
      api.discoverPools({
        chain: form.chain,
        token_address: normalizedSellToken,
      }),
    onSuccess: (data) => {
      const sorted = [...data.pools].sort((a, b) => {
        const usdA = poolUsdValue(a, form.sellTokenMeta?.decimals ?? 18) ?? -1;
        const usdB = poolUsdValue(b, form.sellTokenMeta?.decimals ?? 18) ?? -1;
        return usdB - usdA;
      });
      update({
        discoveredPools: sorted,
        selectedPoolAddresses: new Set(),
        poolPaths: {},
        pathLoadingPools: new Set(),
        pathErrorPools: new Set(),
      });
    },
  });

  // ── Per-pool path computation ──
  // Called when a pool is toggled; computes the path through that specific pool
  const togglePool = useCallback(
    async (pool: PoolInfo) => {
      const address = pool.pool_address;
      const isSelected = form.selectedPoolAddresses.has(address);

      if (isSelected) {
        // Deselect pool and remove its path
        setForm((prev) => {
          const nextSelected = new Set(prev.selectedPoolAddresses);
          nextSelected.delete(address);
          const { [address]: _removed, ...restPaths } = prev.poolPaths;
          const nextErrors = new Set(prev.pathErrorPools);
          nextErrors.delete(address);
          return {
            ...prev,
            selectedPoolAddresses: nextSelected,
            poolPaths: restPaths,
            pathErrorPools: nextErrors,
          };
        });
      } else {
        // Select pool and compute its swap path
        setForm((prev) => {
          const nextSelected = new Set(prev.selectedPoolAddresses);
          nextSelected.add(address);
          const nextLoading = new Set(prev.pathLoadingPools);
          nextLoading.add(address);
          const nextErrors = new Set(prev.pathErrorPools);
          nextErrors.delete(address);
          return { ...prev, selectedPoolAddresses: nextSelected, pathLoadingPools: nextLoading, pathErrorPools: nextErrors };
        });

        try {
          const result = await api.computePoolPath({
            chain: form.chain,
            sell_token: normalizedSellToken,
            target_token: normalizedTargetToken,
            pool_address: address,
            pool_type: pool.pool_type,
            token0: normalizeTokenForProcessing(form.chain, pool.token0),
            token1: normalizeTokenForProcessing(form.chain, pool.token1),
            fee_tier: pool.fee_tier,
          });

          setForm((prev) => {
            const nextLoading = new Set(prev.pathLoadingPools);
            nextLoading.delete(address);

            if (result.path) {
              return {
                ...prev,
                pathLoadingPools: nextLoading,
                poolPaths: { ...prev.poolPaths, [address]: result.path },
              };
            } else {
              // No route found — deselect and mark as error
              const nextSelected = new Set(prev.selectedPoolAddresses);
              nextSelected.delete(address);
              const nextErrors = new Set(prev.pathErrorPools);
              nextErrors.add(address);
              return {
                ...prev,
                pathLoadingPools: nextLoading,
                selectedPoolAddresses: nextSelected,
                pathErrorPools: nextErrors,
              };
            }
          });
        } catch {
          setForm((prev) => {
            const nextLoading = new Set(prev.pathLoadingPools);
            nextLoading.delete(address);
            const nextSelected = new Set(prev.selectedPoolAddresses);
            nextSelected.delete(address);
            const nextErrors = new Set(prev.pathErrorPools);
            nextErrors.add(address);
            return {
              ...prev,
              pathLoadingPools: nextLoading,
              selectedPoolAddresses: nextSelected,
              pathErrorPools: nextErrors,
            };
          });
        }
      }
    },
    [
      form.chain,
      form.sellToken,
      form.targetToken,
      form.selectedPoolAddresses,
      normalizedSellToken,
      normalizedTargetToken,
    ],
  );

  // ── Session creation ──
  const createMutation = useMutation({
    mutationFn: () => {
      // Build pools list with per-pool swap_path_json attached
      const selectedPools = form.discoveredPools
        .filter((p) => form.selectedPoolAddresses.has(p.pool_address))
        .map((p) => ({
          ...p,
          token0: normalizeTokenForProcessing(form.chain, p.token0),
          token1: normalizeTokenForProcessing(form.chain, p.token1),
          swap_path_json: form.poolPaths[p.pool_address]
            ? JSON.stringify(form.poolPaths[p.pool_address])
            : "",
        }));

      return api.createSession({
        wallet_id: form.walletId,
        chain: form.chain,
        sell_token: normalizedSellToken,
        sell_token_symbol: form.sellTokenMeta?.symbol ?? "",
        sell_token_decimals: form.sellTokenMeta?.decimals ?? 18,
        target_token: normalizedTargetToken,
        target_token_symbol: form.targetTokenMeta?.symbol ?? "",
        target_token_decimals: form.targetTokenMeta?.decimals ?? 18,
        total_amount: toWei(form.totalAmount, sellDecimals),
        pov_percent: form.povPercent,
        max_price_impact: form.maxPriceImpact,
        min_buy_trigger_usd: form.minBuyTriggerUsd,
        min_market_cap_usd: form.minMarketCapUsd,
        // Session-level swap path is not used; routing is per-pool
        swap_path_json: undefined,
        pools: selectedPools,
      });
    },
    onSuccess: (session) => {
      window.location.href = `/sessions/${session.session_id}`;
    },
  });

  // ── Navigation helpers ──
  const goToPoolStep = () => {
    // Clear previous pool discovery when re-entering step 1
    update({
      discoveredPools: [],
      selectedPoolAddresses: new Set(),
      poolPaths: {},
      pathLoadingPools: new Set(),
      pathErrorPools: new Set(),
    });
    setStep(1);
  };

  const wallets = walletsQuery.data?.wallets ?? [];
  const adminWallets = wallets as AdminWallet[];

  // Computed: selected pools that have successfully computed paths
  const selectedPoolsWithPath = form.discoveredPools.filter(
    (p) =>
      form.selectedPoolAddresses.has(p.pool_address) &&
      form.poolPaths[p.pool_address] !== undefined,
  );

  // Any pool still computing
  const anyPathLoading = form.pathLoadingPools.size > 0;

  // Step 0 can advance when basic token info is complete
  const step0Complete =
    !!form.walletId &&
    isValidAddress(form.sellToken) &&
    isValidAddress(form.targetToken) &&
    !!form.totalAmount &&
    !amountExceedsBalance &&
    !form.sellTokenMeta?.loading &&
    !form.sellTokenMeta?.error &&
    !form.targetTokenMeta?.loading &&
    !form.targetTokenMeta?.error;

  // Step 1 can advance when at least one pool has a computed path
  const step1Complete = selectedPoolsWithPath.length > 0 && !anyPathLoading;

  return (
    <main className="min-h-screen p-8 max-w-3xl mx-auto">
      <h1 className="text-3xl font-bold mb-8">Create Liquifier Session</h1>

      {/* Step indicators */}
      <div className="flex gap-2 mb-8">
        {["Token Setup", "Pools & Routing", "Review"].map((label, i) => (
          <div
            key={label}
            className={cn(
              "flex-1 h-1 rounded-full transition-colors",
              i <= step ? "bg-primary" : "bg-secondary",
            )}
          />
        ))}
      </div>

      {/* ── Step 0: Token & Settings Setup ───────────────────── */}
      {step === 0 && (
        <Card>
          <CardHeader>
            <CardTitle>Token Setup</CardTitle>
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
                        sellTokenMeta: null,
                        targetTokenMeta: null,
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
                  {role === "admin"
                    ? adminWallets.map((w) => (
                      <option key={w.wallet_id} value={w.wallet_id}>
                        {w.owner_name} — {w.address}
                      </option>
                    ))
                    : wallets.map((w) => (
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
                onChange={(e) => update({ sellToken: e.target.value })}
              />
              <div className="mt-2 flex flex-wrap items-center gap-2">
                <span className="text-xs text-muted-foreground">Shortcuts:</span>
                <button
                  type="button"
                  className="px-2 py-1 text-xs rounded border border-border hover:border-primary transition-colors"
                  onClick={() => update({ sellToken: "0x6Ec90334d89dBdc89E08A133271be3d104128Edb" })}
                >
                  WKC
                </button>
              </div>
              {form.sellTokenMeta && <TokenMetaCard meta={form.sellTokenMeta} />}
            </div>

            {/* Target Token */}
            <div>
              <label className="text-sm text-muted-foreground mb-1 block">
                Target Token (Contract Address)
              </label>
              <Input
                placeholder="0x..."
                value={form.targetToken}
                onChange={(e) => update({ targetToken: e.target.value })}
              />
              <div className="mt-2 flex flex-wrap items-center gap-2">
                <span className="text-xs text-muted-foreground">Shortcuts:</span>
                <button
                  type="button"
                  className="px-2 py-1 text-xs rounded border border-border hover:border-primary transition-colors"
                  onClick={() => update({ targetToken: NATIVE_TOKEN_PLACEHOLDER })}
                >
                  BNB (eeeee)
                </button>
                <button
                  type="button"
                  className="px-2 py-1 text-xs rounded border border-border hover:border-primary transition-colors"
                  onClick={() => update({ targetToken: BSC_USDT_ADDRESS })}
                >
                  USDT
                </button>
              </div>
              {form.targetTokenMeta && <TokenMetaCard meta={form.targetTokenMeta} />}
            </div>

            {/* Total Amount */}
            <div>
              <label className="text-sm text-muted-foreground mb-1 block">
                Total Amount to Sell
                {form.sellTokenMeta?.symbol ? ` (${form.sellTokenMeta.symbol})` : ""}
              </label>
              <Input
                type="text"
                inputMode="decimal"
                placeholder="1000.0"
                value={form.totalAmount}
                onChange={(e) => update({ totalAmount: e.target.value })}
              />
              <div className="flex items-center gap-2 mt-2">
                {[25, 50, 100].map((pct) => (
                  <button
                    key={pct}
                    type="button"
                    onClick={() => setAmountPercent(pct)}
                    disabled={!walletBalanceWei}
                    className="px-2 py-1 text-xs rounded border border-border hover:border-primary disabled:opacity-40 transition-colors"
                  >
                    {pct === 100 ? "Max" : `${pct}%`}
                  </button>
                ))}
                {balanceLoading && (
                  <span className="text-xs text-muted-foreground animate-pulse">
                    Loading balance...
                  </span>
                )}
                {balanceHuman && !balanceLoading && (
                  <span className="text-xs text-muted-foreground ml-auto">
                    Balance: {balanceHuman} {form.sellTokenMeta?.symbol ?? ""}
                  </span>
                )}
              </div>
              {amountExceedsBalance && (
                <p className="text-xs text-destructive mt-1">
                  Amount exceeds wallet balance of {balanceHuman} {form.sellTokenMeta?.symbol ?? ""}
                </p>
              )}
            </div>

            {/* Advanced Settings — inline on step 0 */}
            <div className="border rounded-lg p-4 space-y-4">
              <p className="text-sm font-medium">Advanced Settings</p>

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
                  onChange={(e) =>
                    update({ maxPriceImpact: parseFloat(e.target.value) || 0 })
                  }
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
                  onChange={(e) =>
                    update({ minBuyTriggerUsd: parseFloat(e.target.value) || 0 })
                  }
                />
                <p className="text-xs text-muted-foreground mt-1">
                  Only react to buy events larger than this USD value.
                </p>
              </div>

              <div>
                <label className="text-sm text-muted-foreground mb-1 block">
                  Minimum Market Cap (USD)
                </label>
                <Input
                  type="number"
                  step="1"
                  min="0"
                  value={form.minMarketCapUsd}
                  onChange={(e) =>
                    update({ minMarketCapUsd: parseFloat(e.target.value) || 0 })
                  }
                />
                <p className="text-xs text-muted-foreground mt-1">
                  Skip sells when token market cap falls below this value at execution time.
                </p>
              </div>
            </div>

            <Button
              onClick={goToPoolStep}
              disabled={!step0Complete}
              className="w-full"
            >
              Next: Select Pools
            </Button>
          </CardContent>
        </Card>
      )}

      {/* ── Step 1: Pool Discovery & Routing ─────────────────── */}
      {step === 1 && (
        <Card>
          <CardHeader>
            <CardTitle>Pools & Routing</CardTitle>
          </CardHeader>
          <CardContent className="space-y-4">
            <p className="text-sm text-muted-foreground">
              Discover liquidity pools that contain{" "}
              <span className="font-medium text-foreground">
                {form.sellTokenMeta?.symbol ?? form.sellToken.slice(0, 10) + "…"}
              </span>
              . Select the pools to monitor — the system will only react to swaps
              on these pools. A swap route from your sell token through each pool
              to{" "}
              <span className="font-medium text-foreground">
                {form.targetTokenMeta?.symbol ?? form.targetToken.slice(0, 10) + "…"}
              </span>{" "}
              will be computed automatically when you select a pool.
            </p>

            {/* Discover button */}
            <Button
              variant="outline"
              onClick={() => poolsMutation.mutate()}
              disabled={poolsMutation.isPending}
              className="w-full"
            >
              {poolsMutation.isPending
                ? "Discovering Pools…"
                : form.discoveredPools.length > 0
                  ? `Re-Discover Pools on ${form.chain}`
                  : `Discover Pools on ${form.chain}`}
            </Button>

            {poolsMutation.isError && (
              <p className="text-sm text-destructive">
                Failed to discover pools. Check the token address and try again.
              </p>
            )}

            {/* Pool list */}
            {form.discoveredPools.length > 0 && (
              <div className="border rounded-lg p-4 space-y-2">
                <div className="flex items-center justify-between">
                  <span className="text-sm font-medium">
                    {form.discoveredPools.length} pool
                    {form.discoveredPools.length !== 1 ? "s" : ""} found — select
                    the pools to watch
                  </span>
                  <div className="flex gap-2 text-xs">
                    <span className="px-2 py-0.5 rounded bg-blue-500/10 text-blue-500">
                      V2:{" "}
                      {form.discoveredPools.filter((p) => p.pool_type === "v2").length}
                    </span>
                    <span className="px-2 py-0.5 rounded bg-purple-500/10 text-purple-500">
                      V3:{" "}
                      {form.discoveredPools.filter((p) => p.pool_type === "v3").length}
                    </span>
                  </div>
                </div>

                <div className="max-h-80 overflow-y-auto space-y-1">
                  {form.discoveredPools.map((pool) => {
                    const selected = form.selectedPoolAddresses.has(pool.pool_address);
                    const pathLoading = form.pathLoadingPools.has(pool.pool_address);
                    const pathError = form.pathErrorPools.has(pool.pool_address);
                    const path = form.poolPaths[pool.pool_address];

                    return (
                      <button
                        key={pool.pool_address}
                        onClick={() => togglePool(pool)}
                        disabled={pathLoading}
                        className={cn(
                          "w-full text-xs p-2 rounded transition-colors text-left",
                          selected
                            ? "bg-primary/10 border border-primary"
                            : pathError
                              ? "bg-destructive/10 border border-destructive/40"
                              : "bg-muted/50 border border-transparent hover:border-muted-foreground",
                          pathLoading && "opacity-60 cursor-wait",
                        )}
                      >
                        <div className="flex items-center justify-between gap-2">
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
                                <span className="text-green-500 font-medium">
                                  {formatUsd(usd)}
                                </span>
                              ) : null;
                            })()}
                            <code className="text-muted-foreground">
                              {pool.pool_address.slice(0, 8)}…
                              {pool.pool_address.slice(-6)}
                            </code>
                          </div>
                        </div>

                        {/* Per-pool path status */}
                        {pathLoading && (
                          <p className="mt-1.5 text-xs text-muted-foreground animate-pulse pl-6">
                            Computing swap route…
                          </p>
                        )}
                        {path && !pathLoading && (
                          <div className="mt-1.5 pl-6 flex items-center gap-2 text-green-600">
                            <span>✓</span>
                            <span>
                              {path.hops.length} hop
                              {path.hops.length !== 1 ? "s" : ""}
                            </span>
                            <span className="text-muted-foreground">
                              {path.hop_tokens
                                .map((t) => `${t.slice(0, 6)}…${t.slice(-4)}`)
                                .join(" → ")}
                            </span>
                            {path.estimated_price_impact > 0 && (
                              <span className="text-muted-foreground ml-auto">
                                est. {path.estimated_price_impact.toFixed(2)}% impact
                              </span>
                            )}
                          </div>
                        )}
                        {pathError && !pathLoading && (
                          <p className="mt-1.5 text-xs text-destructive pl-6">
                            No route found to target token — cannot select this pool.
                          </p>
                        )}
                      </button>
                    );
                  })}
                </div>

                {selectedPoolsWithPath.length > 0 && (
                  <p className="text-xs text-green-600 pt-1">
                    {selectedPoolsWithPath.length} pool
                    {selectedPoolsWithPath.length !== 1 ? "s" : ""} selected with
                    valid swap routes.
                  </p>
                )}
              </div>
            )}

            <div className="flex gap-3 pt-2">
              <Button variant="outline" onClick={() => setStep(0)}>
                Back
              </Button>
              <Button
                onClick={() => setStep(2)}
                disabled={!step1Complete}
                className="flex-1"
              >
                Review & Create
              </Button>
            </div>
          </CardContent>
        </Card>
      )}

      {/* ── Step 2: Review ────────────────────────────────────── */}
      {step === 2 && (
        <Card>
          <CardHeader>
            <CardTitle>Review Session</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3">
            <div className="grid grid-cols-2 gap-3 text-sm">
              <div className="text-muted-foreground">Chain</div>
              <div className="font-mono">{form.chain}</div>

              <div className="text-muted-foreground">Wallet</div>
              <div>
                <CopyableAddress address={wallets.find((w) => w.wallet_id === form.walletId)?.address ?? form.walletId} className="text-sm" />
              </div>

              <div className="text-muted-foreground">Sell Token</div>
              <div className="font-mono">
                {form.sellTokenMeta?.symbol || "?"} ({form.sellToken.slice(0, 10)}…)
              </div>

              <div className="text-muted-foreground">Target Token</div>
              <div className="font-mono">
                {form.targetTokenMeta?.symbol || "?"} ({form.targetToken.slice(0, 10)}…)
              </div>

              <div className="text-muted-foreground">Total Amount</div>
              <div className="font-mono">
                {form.totalAmount} {form.sellTokenMeta?.symbol ?? ""}
              </div>

              <div className="text-muted-foreground">POV %</div>
              <div className="font-mono">{form.povPercent}%</div>

              <div className="text-muted-foreground">Max Price Impact</div>
              <div className="font-mono">{form.maxPriceImpact}%</div>

              <div className="text-muted-foreground">Min Buy Trigger</div>
              <div className="font-mono">${form.minBuyTriggerUsd}</div>

              <div className="text-muted-foreground">Min Market Cap</div>
              <div className="font-mono">${form.minMarketCapUsd}</div>

              <div className="text-muted-foreground">Monitored Pools</div>
              <div className="font-mono">
                {selectedPoolsWithPath.length} pool
                {selectedPoolsWithPath.length !== 1 ? "s" : ""}
              </div>
            </div>

            {/* Pool + path summary */}
            {selectedPoolsWithPath.length > 0 && (
              <div className="border rounded-lg p-3 space-y-2">
                <p className="text-xs font-medium text-muted-foreground">
                  Swap routes (first hop through selected pool):
                </p>
                {selectedPoolsWithPath.map((pool) => {
                  const path = form.poolPaths[pool.pool_address];
                  return (
                    <div
                      key={pool.pool_address}
                      className="text-xs flex items-center gap-2 p-2 rounded bg-muted/50"
                    >
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
                      <code className="text-muted-foreground">
                        {pool.pool_address.slice(0, 8)}…{pool.pool_address.slice(-6)}
                      </code>
                      {path && (
                        <span className="ml-auto text-green-600">
                          {path.hops.length} hop{path.hops.length !== 1 ? "s" : ""}
                        </span>
                      )}
                    </div>
                  );
                })}
              </div>
            )}

            <div className="flex gap-3 pt-6">
              <Button variant="outline" onClick={() => setStep(1)}>
                Back
              </Button>
              <Button
                onClick={() => createMutation.mutate()}
                disabled={createMutation.isPending}
                className="flex-1"
              >
                {createMutation.isPending ? "Creating…" : "Create Session"}
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
        Fetching token info…
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
