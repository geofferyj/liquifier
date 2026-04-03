"use client";

import { useState, useEffect } from "react";
import { useRouter } from "next/navigation";
import { api } from "@/lib/api";
import { useAuthStore } from "@/lib/store";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";

type Step = "loading" | "setup" | "verify";

export default function Setup2faPage() {
  const router = useRouter();
  const setAuth = useAuthStore((s) => s.setAuth);

  const [step, setStep] = useState<Step>("loading");
  const [totpSecret, setTotpSecret] = useState("");
  const [otpauthUrl, setOtpauthUrl] = useState("");
  const [qrBase64, setQrBase64] = useState("");
  const [totpCode, setTotpCode] = useState("");
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    // Must have a temporary access token from login/signup
    if (!api.getAccessToken()) {
      router.replace("/login");
      return;
    }

    api
      .setup2fa()
      .then((totp) => {
        setTotpSecret(totp.secret);
        setOtpauthUrl(totp.otpauth_url);
        setQrBase64(totp.qr_code_base64 ?? "");
        setStep("setup");
      })
      .catch(() => {
        setError("Failed to initialize 2FA setup. Please sign in again.");
      });
  }, [router]);

  const handleVerify = async (e: React.FormEvent) => {
    e.preventDefault();
    setError("");
    setLoading(true);

    try {
      await api.verify2fa(totpCode);
      // 2FA is now enabled — user needs to log in fully with TOTP
      api.clearTokens();
      router.push("/login");
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "Invalid code. Try again.");
    } finally {
      setLoading(false);
    }
  };

  if (error && step === "loading") {
    return (
      <main className="flex min-h-screen items-center justify-center p-8">
        <Card className="w-full max-w-md">
          <CardContent className="text-center py-8 space-y-4">
            <p className="text-destructive">{error}</p>
            <Button onClick={() => router.push("/login")}>Back to Sign In</Button>
          </CardContent>
        </Card>
      </main>
    );
  }

  if (step === "loading") {
    return (
      <main className="flex min-h-screen items-center justify-center p-8">
        <p className="text-muted-foreground">Setting up 2FA...</p>
      </main>
    );
  }

  if (step === "setup") {
    return (
      <main className="flex min-h-screen items-center justify-center p-8">
        <Card className="w-full max-w-md">
          <CardHeader>
            <CardTitle className="text-2xl text-center">Set Up 2FA</CardTitle>
          </CardHeader>
          <CardContent className="space-y-4">
            <p className="text-sm text-muted-foreground text-center">
              Scan this QR code with your authenticator app (Google
              Authenticator, Authy, etc.)
            </p>

            <div className="flex justify-center rounded-lg bg-white p-4">
              {qrBase64 ? (
                <img
                  src={`data:image/png;base64,${qrBase64}`}
                  alt="TOTP QR Code"
                  width={200}
                  height={200}
                />
              ) : (
                <img
                  src={`https://api.qrserver.com/v1/create-qr-code/?data=${encodeURIComponent(otpauthUrl)}&size=200x200`}
                  alt="TOTP QR Code"
                  width={200}
                  height={200}
                />
              )}
            </div>

            <div>
              <p className="text-xs text-muted-foreground mb-1">
                Or enter this secret manually:
              </p>
              <code className="block text-sm bg-muted px-3 py-2 rounded font-mono break-all select-all">
                {totpSecret}
              </code>
            </div>

            <Button className="w-full" onClick={() => setStep("verify")}>
              I&apos;ve saved the code → Verify
            </Button>
          </CardContent>
        </Card>
      </main>
    );
  }

  return (
    <main className="flex min-h-screen items-center justify-center p-8">
      <Card className="w-full max-w-md">
        <CardHeader>
          <CardTitle className="text-2xl text-center">Verify 2FA</CardTitle>
        </CardHeader>
        <CardContent>
          <form onSubmit={handleVerify} className="space-y-4">
            <p className="text-sm text-muted-foreground text-center">
              Enter the 6-digit code from your authenticator app.
            </p>

            <Input
              type="text"
              inputMode="numeric"
              maxLength={6}
              value={totpCode}
              onChange={(e) => setTotpCode(e.target.value)}
              placeholder="000000"
              className="text-center text-2xl tracking-widest"
              autoFocus
            />

            {error && (
              <p className="text-sm text-destructive text-center">{error}</p>
            )}

            <Button type="submit" className="w-full" disabled={loading}>
              {loading ? "Verifying..." : "Verify & Complete Setup"}
            </Button>

            <button
              type="button"
              className="text-sm text-muted-foreground hover:text-primary w-full text-center"
              onClick={() => setStep("setup")}
            >
              ← Back to QR code
            </button>
          </form>
        </CardContent>
      </Card>
    </main>
  );
}
