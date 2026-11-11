// The centered card layout of the signed-out pages (legacy layouts/auth):
// logo, a one-line subtitle, then the page card. Rendered outside the app
// shell; never reads the session.
import type { ReactNode } from "react";
import { isRouteErrorResponse, Outlet, useRouteError } from "react-router";

import { errorMessage } from "@/lib/api";
import { cn } from "@/lib/utils";
import lockup from "@/assets/centaur-lockup-white.svg";

export function AuthLayout() {
  return (
    <main className="flex min-h-full items-center justify-center overflow-y-auto bg-background px-6 py-16 font-mono text-foreground">
      <div className="w-full max-w-sm">
        <Outlet />
      </div>
    </main>
  );
}

/** Logo and subtitle above the card. */
export function AuthHeader({ subtitle }: { subtitle: string }) {
  return (
    <div className="mb-8 text-center">
      <img src={lockup} alt="Centaur" width={497} height={127} className="logo-themed mx-auto h-9 w-auto" />
      <p className="mt-4 text-sm text-muted-foreground">{subtitle}</p>
    </div>
  );
}

export function AuthCard({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={cn("rounded-xl border border-border bg-card/60 p-6", className)}>{children}</div>;
}

/** Neutral notice box above the card (e.g. "Signed out."). */
export function AuthNotice({ children }: { children: ReactNode }) {
  return (
    <div role="status" className="mb-6 rounded-sm border border-border bg-card/60 px-4 py-3 text-sm text-muted-foreground">
      {children}
    </div>
  );
}

export function AuthLoading() {
  return <p className="text-center text-sm text-muted-foreground">Loading…</p>;
}

/** Error boundary for the auth routes (they sit outside the shell's). */
export function AuthRouteError() {
  const error = useRouteError();
  const message = isRouteErrorResponse(error) ? error.statusText : errorMessage(error);
  return (
    <main className="flex min-h-full items-center justify-center bg-background px-6 py-16 font-mono">
      <div className="w-full max-w-sm">
        <AuthHeader subtitle="Operator console." />
        <AuthCard>
          <h1 className="text-lg font-bold text-red-300 light:text-red-700">Something went wrong</h1>
          <p className="mt-3 text-sm text-muted-foreground">{message}</p>
        </AuthCard>
      </div>
    </main>
  );
}
