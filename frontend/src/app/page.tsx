"use client";

import Link from "next/link";
import { useEffect } from "react";
import { useRouter } from "next/navigation";
import { useAuthStore } from "@/lib/store";

export default function Home() {
  const router = useRouter();
  const isAuthenticated = useAuthStore((s) => s.isAuthenticated);
  const role = useAuthStore((s) => s.role);
  const hydrated = useAuthStore((s) => s.hydrated);

  useEffect(() => {
    if (!hydrated) return;
    if (isAuthenticated) {
      if (role === "admin") {
        router.replace("/dashboard");
      } else {
        router.replace("/my-dashboard");
      }
    }
  }, [hydrated, isAuthenticated, role, router]);

  // Wait for client-side hydration before rendering anything
  if (!hydrated) return null;

  // If authenticated, show nothing while redirecting
  if (isAuthenticated) return null;

  return (
    <main className="flex min-h-screen flex-col items-center justify-center gap-8 p-8">
      <div className="text-center">
        <h1 className="text-5xl font-bold tracking-tight text-primary inline-flex flex-col items-end">
          <span>Liquifier Bot</span>
          <span className="text-xs font-normal text-muted-foreground tracking-widest">
            by penitools
          </span>
        </h1>
        <p className="mt-4 text-xl font-semibold max-w-lg">
          Offload large amounts of tokens without triggering price drops.
        </p>
        <p className="mt-4 text-base text-muted-foreground max-w-lg">
          Our liquifier bot helps you sell your position intelligently in a way that minimizes slippage and price impact on any token.
        </p>
      </div>
      <div className="flex gap-4">
        <Link
          href="/login"
          className="rounded-lg bg-primary px-6 py-3 font-semibold text-primary-foreground hover:bg-primary/90 transition"
        >
          Sign In
        </Link>
        <Link
          href="/signup"
          className="rounded-lg border border-border px-6 py-3 font-semibold hover:bg-secondary transition"
        >
          Sign Up
        </Link>
      </div>
    </main>
  );
}
