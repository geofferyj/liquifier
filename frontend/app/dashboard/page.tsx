import Link from "next/link";

export default function DashboardRootPage() {
  return (
    <div className="flex flex-col items-center justify-center min-h-[80vh] text-center px-4">
      <h1 className="text-4xl font-bold text-white mb-3">💧 Liquifier</h1>
      <p className="text-lg text-muted-foreground mb-8 max-w-md">
        Systematically offload large token positions over time — without causing massive market price impact.
      </p>
      <div className="flex gap-4">
        <Link href="/login"   className="btn-primary">Sign In</Link>
        <Link href="/signup"  className="btn-secondary">Create Account</Link>
      </div>
    </div>
  );
}
