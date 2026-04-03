"use client";

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { api } from "@/lib/api";
import { useAuthStore } from "@/lib/store";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { cn } from "@/lib/utils";
import type { Wallet } from "@/lib/types";
import { useState } from "react";
import { useRouter } from "next/navigation";

export default function SettingsPage() {
  const router = useRouter();
  const queryClient = useQueryClient();
  const clearAuth = useAuthStore((s) => s.clearAuth);

  const profileQuery = useQuery({
    queryKey: ["profile"],
    queryFn: () => api.getProfile(),
  });

  const walletsQuery = useQuery({
    queryKey: ["wallets"],
    queryFn: () => api.listWallets(),
  });

  const resendMutation = useMutation({
    mutationFn: () => api.resendVerification(),
  });

  const createWalletMutation = useMutation({
    mutationFn: (chain: string) => api.createWallet(chain),
    onSuccess: () =>
      queryClient.invalidateQueries({ queryKey: ["wallets"] }),
  });

  const [selectedChain, setSelectedChain] = useState("ethereum");
  const chains = ["ethereum", "base", "arbitrum", "bsc", "polygon", "optimism"];

  const profile = profileQuery.data;
  const wallets = walletsQuery.data?.wallets ?? [];

  const handleLogout = () => {
    api.clearTokens();
    clearAuth();
    router.push("/login");
  };

  return (
    <main className="min-h-screen p-8 max-w-3xl mx-auto space-y-6">
      <h1 className="text-3xl font-bold">Settings</h1>

      {/* Profile */}
      <Card>
        <CardHeader>
          <CardTitle>Profile</CardTitle>
        </CardHeader>
        <CardContent className="space-y-3">
          {profileQuery.isLoading ? (
            <p className="text-muted-foreground">Loading...</p>
          ) : profile ? (
            <>
              <div className="flex items-center justify-between">
                <div>
                  <p className="text-sm text-muted-foreground">Email</p>
                  <p className="font-mono">{profile.email}</p>
                </div>
                <div className="flex items-center gap-2">
                  <span
                    className={cn(
                      "px-2.5 py-0.5 rounded-full text-xs font-medium",
                      profile.email_verified
                        ? "bg-green-500/10 text-green-500"
                        : "bg-yellow-500/10 text-yellow-500",
                    )}
                  >
                    {profile.email_verified ? "Verified" : "Unverified"}
                  </span>
                  {!profile.email_verified && (
                    <Button
                      variant="secondary"
                      size="sm"
                      onClick={() => resendMutation.mutate()}
                      disabled={resendMutation.isPending}
                    >
                      {resendMutation.isPending
                        ? "Sending..."
                        : "Resend Verification"}
                    </Button>
                  )}
                </div>
              </div>

              <div className="flex items-center justify-between">
                <div>
                  <p className="text-sm text-muted-foreground">
                    Two-Factor Authentication
                  </p>
                  <p className="text-sm">
                    {profile.totp_enabled ? "Enabled" : "Disabled"}
                  </p>
                </div>
                <span
                  className={cn(
                    "px-2.5 py-0.5 rounded-full text-xs font-medium",
                    profile.totp_enabled
                      ? "bg-green-500/10 text-green-500"
                      : "bg-red-500/10 text-red-500",
                  )}
                >
                  {profile.totp_enabled ? "Active" : "Inactive"}
                </span>
              </div>
            </>
          ) : (
            <p className="text-destructive">Failed to load profile</p>
          )}

          <div className="pt-2">
            <Button variant="destructive" size="sm" onClick={handleLogout}>
              Sign Out
            </Button>
          </div>
        </CardContent>
      </Card>

      {/* Wallets */}
      <Card>
        <CardHeader>
          <CardTitle>Wallets</CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          {wallets.length === 0 && !walletsQuery.isLoading && (
            <p className="text-sm text-muted-foreground">
              No wallets yet. Create one to start sessions.
            </p>
          )}

          {wallets.map((w: Wallet) => (
            <div
              key={w.wallet_id}
              className="flex items-center justify-between border-b border-border pb-3 last:border-0"
            >
              <div>
                <p className="font-mono text-sm">{w.address}</p>
                <p className="text-xs text-muted-foreground">
                  {w.chain} · Created{" "}
                  {new Date(w.created_at).toLocaleDateString()}
                </p>
              </div>
            </div>
          ))}

          <div className="flex items-center gap-2 pt-2">
            <select
              className="rounded-md border border-border bg-background px-3 py-2 text-sm"
              value={selectedChain}
              onChange={(e) => setSelectedChain(e.target.value)}
            >
              {chains.map((c) => (
                <option key={c} value={c}>
                  {c}
                </option>
              ))}
            </select>
            <Button
              size="sm"
              onClick={() => createWalletMutation.mutate(selectedChain)}
              disabled={createWalletMutation.isPending}
            >
              {createWalletMutation.isPending
                ? "Creating..."
                : "Generate Wallet"}
            </Button>
          </div>
        </CardContent>
      </Card>
    </main>
  );
}
