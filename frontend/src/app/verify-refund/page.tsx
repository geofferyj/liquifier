"use client";

import { Suspense, useEffect, useState } from "react";
import { useSearchParams } from "next/navigation";
import Link from "next/link";
import { api } from "@/lib/api";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";

function VerifyRefundContent() {
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
      .verifyRefund(token)
      .then((res) => {
        setStatus("success");
        setMessage(res.message || "Refund request verified successfully!");
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
            Refund Verification
          </CardTitle>
        </CardHeader>
        <CardContent className="text-center space-y-4">
          {status === "loading" && (
            <p className="text-muted-foreground">
              Verifying your refund request...
            </p>
          )}

          {status === "success" && (
            <>
              <div className="text-4xl">✓</div>
              <p className="text-primary">{message}</p>
              <p className="text-sm text-muted-foreground">
                Your refund request has been submitted for admin review.
              </p>
              <Link href="/my-dashboard">
                <Button>Back to Dashboard</Button>
              </Link>
            </>
          )}

          {status === "error" && (
            <>
              <div className="text-4xl">✗</div>
              <p className="text-destructive">{message}</p>
              <Link href="/my-dashboard">
                <Button variant="secondary">Back to Dashboard</Button>
              </Link>
            </>
          )}
        </CardContent>
      </Card>
    </main>
  );
}

export default function VerifyRefundPage() {
  return (
    <Suspense
      fallback={
        <main className="flex min-h-screen items-center justify-center">
          <p className="text-muted-foreground">Loading...</p>
        </main>
      }
    >
      <VerifyRefundContent />
    </Suspense>
  );
}
