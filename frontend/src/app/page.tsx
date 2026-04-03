import Link from "next/link";

export default function Home() {
  return (
    <main className="flex min-h-screen flex-col items-center justify-center gap-8 p-8">
      <div className="text-center">
        <h1 className="text-5xl font-bold tracking-tight text-primary">
          Liquifier
        </h1>
        <p className="mt-4 text-lg text-muted-foreground max-w-lg">
          Systematically offload large token positions on EVM DEXes without
          causing market price impact. POV-based execution with real-time
          monitoring.
        </p>
      </div>
      <div className="flex gap-4">
        <Link
          href="/dashboard"
          className="rounded-lg bg-primary px-6 py-3 font-semibold text-primary-foreground hover:bg-primary/90 transition"
        >
          Dashboard
        </Link>
        <Link
          href="/login"
          className="rounded-lg border border-border px-6 py-3 font-semibold hover:bg-secondary transition"
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
