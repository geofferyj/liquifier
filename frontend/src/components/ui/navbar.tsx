"use client";

import { usePathname, useRouter } from "next/navigation";
import { useAuthStore } from "@/lib/store";

/** Pages where the navbar should NOT appear */
const HIDDEN_PATHS = ["/", "/login", "/signup", "/verify-email", "/setup-2fa"];

/** Determine where the back button should go based on current path and role */
function getBackTarget(
  pathname: string,
  role: string | null,
): string | null {
  const home = role === "admin" ? "/dashboard" : "/my-dashboard";

  // Main dashboards — no back button
  if (pathname === "/dashboard" || pathname === "/my-dashboard") return null;

  // Admin sub-pages → admin dashboard
  if (pathname.startsWith("/admin/")) return "/dashboard";
  if (pathname === "/dashboard/settings") return "/dashboard";

  // Session detail → home
  if (pathname.startsWith("/sessions/") && pathname !== "/sessions/new") return home;

  // Session create → home
  if (pathname === "/sessions/new") return home;

  // Public share pages — no back button
  if (pathname.startsWith("/share/")) return null;

  // Fallback → home
  return home;
}

export function Navbar() {
  const pathname = usePathname();
  const router = useRouter();
  const { isAuthenticated, role } = useAuthStore();

  // Don't show on unauthenticated / landing pages
  if (HIDDEN_PATHS.includes(pathname)) return null;

  // Don't show on public share pages
  if (pathname.startsWith("/share/")) return null;

  // Don't show if not authenticated
  if (!isAuthenticated) return null;

  const backTarget = getBackTarget(pathname, role);

  return (
    <nav className="sticky top-0 z-40 w-full border-b border-border/50 bg-background/80 backdrop-blur-sm">
      <div className="mx-auto flex h-12 max-w-5xl items-center justify-between px-4">
        <div className="flex items-center gap-3">
          {backTarget && (
            <button
              onClick={() => router.push(backTarget)}
              className="flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground transition-colors"
            >
              <svg
                xmlns="http://www.w3.org/2000/svg"
                width="16"
                height="16"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
              >
                <path d="m15 18-6-6 6-6" />
              </svg>
              Back
            </button>
          )}
        </div>
        <span className="text-sm font-semibold tracking-tight text-muted-foreground">
          Liquifier
        </span>
      </div>
    </nav>
  );
}
