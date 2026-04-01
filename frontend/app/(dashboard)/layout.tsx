import { redirect } from "next/navigation";

// The dashboard route group root redirects to the actual dashboard page
export default function DashboardLayout({ children }: { children: React.ReactNode }) {
  return (
    <div className="min-h-screen bg-background">
      <nav className="border-b border-border px-6 py-3 flex items-center justify-between">
        <span className="font-bold text-lg text-white">💧 Liquifier</span>
        <div className="flex items-center gap-4 text-sm text-muted-foreground">
          <a href="/dashboard" className="hover:text-white transition-colors">Dashboard</a>
          <a href="/sessions/new" className="hover:text-white transition-colors">New Session</a>
        </div>
      </nav>
      <main>{children}</main>
    </div>
  );
}
