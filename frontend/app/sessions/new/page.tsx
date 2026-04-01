"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { useMutation, useQuery } from "@tanstack/react-query";
import apiClient from "@/lib/api";
import { cn } from "@/lib/utils";
import type { CreateSessionPayload, Wallet, SwapPath } from "@/types";

// ── Step components ────────────────────────────────────────────────────────

const STEPS = ["Token Setup", "Swap Paths", "Strategy & Settings", "Review"] as const;
type Step = (typeof STEPS)[number];

interface WizardState {
  walletId:            string;
  chainId:             number;
  tokenAddress:        string;
  targetTokenAddress:  string;
  totalAmount:         string;
  strategy:            "pov" | "price_impact";
  povPercentage:       number;
  maxPriceImpactBps:   number;
  minBuyTriggerUsd:    number;
  selectedPathIndex:   number;
}

const DEFAULT_STATE: WizardState = {
  walletId:            "",
  chainId:             1,
  tokenAddress:        "",
  targetTokenAddress:  "",
  totalAmount:         "",
  strategy:            "pov",
  povPercentage:       5,
  maxPriceImpactBps:   50,
  minBuyTriggerUsd:    100,
  selectedPathIndex:   0,
};

// ── Chain options ──────────────────────────────────────────────────────────

const CHAINS = [
  { id: 1,     name: "Ethereum Mainnet" },
  { id: 8453,  name: "Base" },
  { id: 42161, name: "Arbitrum One" },
  { id: 56,    name: "BNB Smart Chain" },
];

// ── Main wizard component ──────────────────────────────────────────────────

export default function NewSessionPage() {
  const router = useRouter();
  const [step,  setStep]  = useState(0);
  const [state, setState] = useState<WizardState>(DEFAULT_STATE);
  const [paths, setPaths] = useState<SwapPath[]>([]);

  // Fetch user wallets
  const { data: wallets = [] } = useQuery<Wallet[]>({
    queryKey: ["wallets"],
    queryFn:  () => apiClient.get("/api/wallets").then((r) => r.data),
  });

  // Fetch swap paths when token pair + amount changes
  const fetchPaths = useMutation({
    mutationFn: async () => {
      const resp = await apiClient.post("/api/sessions/paths", {
        chain_id:             state.chainId,
        token_address:        state.tokenAddress,
        target_token_address: state.targetTokenAddress,
        amount:               state.totalAmount,
      });
      return resp.data as SwapPath[];
    },
    onSuccess: (data) => {
      setPaths(data);
      setStep(1);
    },
  });

  // Submit session creation
  const createSession = useMutation({
    mutationFn: async () => {
      const payload: CreateSessionPayload = {
        wallet_id:            state.walletId,
        chain_id:             state.chainId,
        token_address:        state.tokenAddress,
        target_token_address: state.targetTokenAddress,
        total_amount:         state.totalAmount,
        strategy:             state.strategy,
        pov_percentage:       state.strategy === "pov" ? state.povPercentage : undefined,
        max_price_impact_bps: state.maxPriceImpactBps,
        min_buy_trigger_usd:  state.minBuyTriggerUsd,
      };
      const resp = await apiClient.post("/api/sessions", payload);
      return resp.data;
    },
    onSuccess: (session) => {
      router.push(`/sessions/${session.id}`);
    },
  });

  const update = (partial: Partial<WizardState>) =>
    setState((prev) => ({ ...prev, ...partial }));

  // ── Render ───────────────────────────────────────────────────────────────

  return (
    <div className="max-w-2xl mx-auto py-10 px-4">
      <h1 className="text-2xl font-bold mb-2 text-white">New Liquifier Session</h1>
      <p className="text-muted-foreground mb-8">
        Systematically offload tokens without price impact.
      </p>

      {/* Step tabs */}
      <div className="flex gap-2 mb-8">
        {STEPS.map((label, i) => (
          <button
            key={label}
            className={cn(
              "flex-1 py-2 rounded text-sm font-medium transition-colors",
              i === step
                ? "bg-primary text-primary-foreground"
                : i < step
                ? "bg-secondary text-secondary-foreground"
                : "bg-muted text-muted-foreground cursor-not-allowed"
            )}
            onClick={() => i < step && setStep(i)}
            disabled={i > step}
          >
            <span className="mr-1 text-xs opacity-60">{i + 1}.</span>
            {label}
          </button>
        ))}
      </div>

      {/* ── Step 1: Token Setup ── */}
      {step === 0 && (
        <div className="space-y-5">
          <Field label="Wallet">
            <select
              className="input w-full"
              value={state.walletId}
              onChange={(e) => update({ walletId: e.target.value })}
            >
              <option value="">Select a wallet…</option>
              {wallets.map((w) => (
                <option key={w.id} value={w.id}>
                  {w.label ?? w.address.slice(0, 10) + "…"}
                </option>
              ))}
            </select>
          </Field>

          <Field label="Chain">
            <select
              className="input w-full"
              value={state.chainId}
              onChange={(e) => update({ chainId: Number(e.target.value) })}
            >
              {CHAINS.map((c) => (
                <option key={c.id} value={c.id}>
                  {c.name}
                </option>
              ))}
            </select>
          </Field>

          <Field label="Token to Sell (contract address)">
            <input
              className="input w-full"
              placeholder="0x…"
              value={state.tokenAddress}
              onChange={(e) => update({ tokenAddress: e.target.value })}
            />
          </Field>

          <Field label="Target Token (desired output)">
            <input
              className="input w-full"
              placeholder="0x… (e.g. USDC)"
              value={state.targetTokenAddress}
              onChange={(e) => update({ targetTokenAddress: e.target.value })}
            />
          </Field>

          <Field label="Total Amount to Sell (raw U256)">
            <input
              className="input w-full"
              placeholder="e.g. 1000000000000000000000"
              value={state.totalAmount}
              onChange={(e) => update({ totalAmount: e.target.value })}
            />
          </Field>

          <NextButton
            disabled={
              !state.walletId ||
              !state.tokenAddress ||
              !state.targetTokenAddress ||
              !state.totalAmount
            }
            loading={fetchPaths.isPending}
            onClick={() => fetchPaths.mutate()}
          >
            Fetch Optimal Paths →
          </NextButton>

          {fetchPaths.isError && (
            <p className="text-destructive text-sm">
              Failed to fetch paths. Check addresses and try again.
            </p>
          )}
        </div>
      )}

      {/* ── Step 2: Swap Paths ── */}
      {step === 1 && (
        <div className="space-y-5">
          <p className="text-sm text-muted-foreground">
            Select one of the top liquidity paths for your sell orders:
          </p>

          {paths.length === 0 && (
            <p className="text-muted-foreground italic">No paths found.</p>
          )}

          {paths.map((path, i) => (
            <PathCard
              key={i}
              path={path}
              index={i}
              selected={state.selectedPathIndex === i}
              onSelect={() => update({ selectedPathIndex: i })}
            />
          ))}

          <div className="flex gap-3 mt-4">
            <button className="btn-secondary flex-1" onClick={() => setStep(0)}>
              ← Back
            </button>
            <NextButton onClick={() => setStep(2)}>
              Next: Strategy →
            </NextButton>
          </div>
        </div>
      )}

      {/* ── Step 3: Strategy & Settings ── */}
      {step === 2 && (
        <div className="space-y-6">
          <Field label="Execution Strategy">
            <div className="flex gap-4">
              {(["pov", "price_impact"] as const).map((s) => (
                <label key={s} className="flex items-center gap-2 cursor-pointer">
                  <input
                    type="radio"
                    checked={state.strategy === s}
                    onChange={() => update({ strategy: s })}
                    className="accent-primary"
                  />
                  <span className="capitalize text-sm">
                    {s === "pov" ? "POV (% of Volume)" : "Price Impact Cap"}
                  </span>
                </label>
              ))}
            </div>
          </Field>

          {state.strategy === "pov" && (
            <Field label={`POV Sell Percentage: ${state.povPercentage}%`}>
              <input
                type="range"
                min={0.1}
                max={50}
                step={0.1}
                value={state.povPercentage}
                onChange={(e) => update({ povPercentage: Number(e.target.value) })}
                className="w-full accent-primary"
              />
              <div className="flex justify-between text-xs text-muted-foreground mt-1">
                <span>0.1%</span>
                <span>50%</span>
              </div>
            </Field>
          )}

          <Field label={`Max Price Impact: ${(state.maxPriceImpactBps / 100).toFixed(2)}%`}>
            <input
              type="range"
              min={10}
              max={500}
              step={10}
              value={state.maxPriceImpactBps}
              onChange={(e) => update({ maxPriceImpactBps: Number(e.target.value) })}
              className="w-full accent-primary"
            />
            <div className="flex justify-between text-xs text-muted-foreground mt-1">
              <span>0.10%</span>
              <span>5.00%</span>
            </div>
          </Field>

          <Field label="Minimum Buy Trigger (USD)">
            <input
              type="number"
              className="input w-full"
              min={1}
              value={state.minBuyTriggerUsd}
              onChange={(e) => update({ minBuyTriggerUsd: Number(e.target.value) })}
            />
            <p className="text-xs text-muted-foreground mt-1">
              Only react to buys valued above this threshold.
            </p>
          </Field>

          <div className="flex gap-3 mt-4">
            <button className="btn-secondary flex-1" onClick={() => setStep(1)}>
              ← Back
            </button>
            <NextButton onClick={() => setStep(3)}>Review →</NextButton>
          </div>
        </div>
      )}

      {/* ── Step 4: Review & Create ── */}
      {step === 3 && (
        <div className="space-y-4">
          <ReviewRow label="Chain" value={CHAINS.find((c) => c.id === state.chainId)?.name ?? state.chainId} />
          <ReviewRow label="Token to Sell"   value={state.tokenAddress} mono />
          <ReviewRow label="Target Token"    value={state.targetTokenAddress} mono />
          <ReviewRow label="Total Amount"    value={state.totalAmount + " (raw)"} />
          <ReviewRow label="Strategy"        value={state.strategy === "pov" ? `POV ${state.povPercentage}%` : "Price Impact Cap"} />
          <ReviewRow label="Max Price Impact" value={`${(state.maxPriceImpactBps / 100).toFixed(2)}%`} />
          <ReviewRow label="Min Buy Trigger"  value={`$${state.minBuyTriggerUsd}`} />

          <div className="flex gap-3 mt-6">
            <button className="btn-secondary flex-1" onClick={() => setStep(2)}>
              ← Back
            </button>
            <NextButton
              loading={createSession.isPending}
              onClick={() => createSession.mutate()}
            >
              🚀 Create Session
            </NextButton>
          </div>

          {createSession.isError && (
            <p className="text-destructive text-sm">
              Failed to create session. Please check your inputs.
            </p>
          )}
        </div>
      )}
    </div>
  );
}

// ── Sub-components ─────────────────────────────────────────────────────────

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      <label className="block text-sm font-medium text-muted-foreground mb-1">{label}</label>
      {children}
    </div>
  );
}

function NextButton({
  children,
  onClick,
  disabled,
  loading,
}: {
  children: React.ReactNode;
  onClick?: () => void;
  disabled?: boolean;
  loading?: boolean;
}) {
  return (
    <button
      className="btn-primary flex-1 flex items-center justify-center gap-2"
      onClick={onClick}
      disabled={disabled || loading}
    >
      {loading && (
        <svg className="animate-spin h-4 w-4" viewBox="0 0 24 24" fill="none">
          <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="4" />
          <path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8v8H4z" />
        </svg>
      )}
      {children}
    </button>
  );
}

function PathCard({
  path,
  index,
  selected,
  onSelect,
}: {
  path: SwapPath;
  index: number;
  selected: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      onClick={onSelect}
      className={cn(
        "w-full text-left p-4 rounded-lg border transition-all",
        selected
          ? "border-primary bg-primary/10"
          : "border-border bg-card hover:border-primary/50"
      )}
    >
      <div className="flex items-center justify-between mb-2">
        <span className="text-xs font-semibold text-muted-foreground">
          PATH {index + 1} {selected && "✓"}
        </span>
        <span className="text-xs text-muted-foreground">Fee: {path.fee_bps} bps</span>
      </div>
      <div className="flex items-center gap-1 flex-wrap">
        {path.tokens.map((token, i) => (
          <span key={i} className="flex items-center gap-1">
            <span className="font-mono text-xs bg-secondary px-2 py-0.5 rounded">
              {token.slice(0, 6)}…{token.slice(-4)}
            </span>
            {i < path.tokens.length - 1 && <span className="text-muted-foreground">→</span>}
          </span>
        ))}
      </div>
      <p className="text-xs text-muted-foreground mt-2">
        Approx. output: {path.liquidity}
      </p>
    </button>
  );
}

function ReviewRow({
  label,
  value,
  mono,
}: {
  label: string;
  value: string | number;
  mono?: boolean;
}) {
  return (
    <div className="flex justify-between py-2 border-b border-border">
      <span className="text-sm text-muted-foreground">{label}</span>
      <span className={cn("text-sm font-medium", mono && "font-mono")}>{value}</span>
    </div>
  );
}
