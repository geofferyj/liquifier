"use client";

import { Suspense, useEffect, useState } from "react";
import { useSearchParams } from "next/navigation";
import Link from "next/link";
import { api } from "@/lib/api";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";

function VerifyEmailContent() {
  const searchParams = useSearchParams();
  const token = searchParams.get("token");

  const [status, setStatus] = useState<"loading" | "success" | "error">(
    "loading",
  );
  const [message, setMessage] = useState("");

  useEffect(() => {
    if (!token) {
      setStatus("error");
      setMessage("No verification token provided.");
      return;
    }

    api
      .verifyEmail(token)
      .then((res) => {
        setStatus("success");
        setMessage(res.message || "Email verified successfully!");
      })
      .catch((err) => {
        setStatus("error");
        setMessage(
          err instanceof Error ? err.message : "Verification failed.",
        );
      });
  }, [token]);

  return (
    <main className="flex min-h-screen items-center justify-center p-8">
      <Card className="w-full max-w-md">
        <CardHeader>
          <CardTitle className="text-2xl text-center">
            Email Verification
          </CardTitle>
        </CardHeader>
        <CardContent className="text-center space-y-4">
          {status === "loading" && (
            <p className="text-muted-foreground">Verifying your email...</p>
          )}

          {status === "success" && (
            <>
              <div className="text-4xl">✓</div>
              <p className="text-primary">{message}</p>
              <p className="text-sm text-muted-foreground">
                You can now sign in to your account.
              </p>
              <Link href="/login">
                <Button>Continue to Sign In</Button>
              </Link>
            </>
          )}

          {status === "error" && (
            <>
              <div className="text-4xl">✗</div>
              <p className="text-destructive">{message}</p>
              <Link href="/login">
                <Button variant="secondary">Back to Sign In</Button>
              </Link>
            </>
          )}
        </CardContent>
      </Card>
    </main>
  );
}

export default function VerifyEmailPage() {
  return (
    <Suspense
      fallback={
        <main className="flex min-h-screen items-center justify-center">
          <p className="text-muted-foreground">Loading...</p>
        </main>
      }
    >
      <VerifyEmailContent />
    </Suspense>
  );
}
