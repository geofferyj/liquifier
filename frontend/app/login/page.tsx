"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import Link from "next/link";
import apiClient from "@/lib/api";
import { useAuthStore } from "@/store/authStore";

export default function LoginPage() {
  const router   = useRouter();
  const setAuth  = useAuthStore((s) => s.setAuth);

  const [email,    setEmail]    = useState("");
  const [password, setPassword] = useState("");
  const [totpCode, setTotpCode] = useState("");
  const [needTotp, setNeedTotp] = useState(false);
  const [loading,  setLoading]  = useState(false);
  const [error,    setError]    = useState<string | null>(null);

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    setLoading(true);
    setError(null);

    try {
      const resp = await apiClient.post("/api/auth/login", {
        email,
        password,
        totp_code: totpCode || undefined,
      });

      const data = resp.data;

      if (data.totp_enabled && !totpCode) {
        setNeedTotp(true);
        setLoading(false);
        return;
      }

      localStorage.setItem("liquifier_token", data.token);
      setAuth(data.token, { id: data.user_id, email, totp_enabled: data.totp_enabled });
      router.push("/dashboard");
    } catch (err: any) {
      setError(err.response?.data?.error ?? "Login failed");
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="min-h-screen flex items-center justify-center bg-background px-4">
      <div className="w-full max-w-sm bg-card border border-border rounded-xl p-8 space-y-6">
        <div className="text-center">
          <h1 className="text-2xl font-bold text-white">💧 Liquifier</h1>
          <p className="text-muted-foreground text-sm mt-1">Sign in to your account</p>
        </div>

        <form onSubmit={handleSubmit} className="space-y-4">
          {!needTotp ? (
            <>
              <div>
                <label className="block text-sm text-muted-foreground mb-1">Email</label>
                <input
                  type="email"
                  className="input w-full"
                  value={email}
                  onChange={(e) => setEmail(e.target.value)}
                  required
                  autoComplete="email"
                />
              </div>
              <div>
                <label className="block text-sm text-muted-foreground mb-1">Password</label>
                <input
                  type="password"
                  className="input w-full"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  required
                  autoComplete="current-password"
                />
              </div>
            </>
          ) : (
            <div>
              <label className="block text-sm text-muted-foreground mb-1">
                2FA Code (6 digits)
              </label>
              <input
                type="text"
                inputMode="numeric"
                pattern="\d{6}"
                maxLength={6}
                className="input w-full tracking-widest text-center text-xl"
                value={totpCode}
                onChange={(e) => setTotpCode(e.target.value)}
                autoFocus
                required
              />
            </div>
          )}

          {error && (
            <p className="text-destructive text-sm bg-destructive/10 rounded px-3 py-2">
              {error}
            </p>
          )}

          <button
            type="submit"
            disabled={loading}
            className="btn-primary w-full"
          >
            {loading ? "Signing in…" : needTotp ? "Verify" : "Sign In"}
          </button>
        </form>

        <p className="text-center text-sm text-muted-foreground">
          Don&apos;t have an account?{" "}
          <Link href="/signup" className="text-primary hover:underline">
            Sign up
          </Link>
        </p>
      </div>
    </div>
  );
}
