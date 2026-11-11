// The centered card layout of the signed-out pages (legacy layouts/auth):
// logo, a one-line subtitle, then the page card. Rendered outside the app
// shell; never reads the session.
import type { ReactNode } from "react";
import { isRouteErrorResponse, Outlet, useRouteError } from "react-router";

import { Loader2 } from "lucide-react";

import { Card } from "@/components/ui/card";
import { errorMessage } from "@/lib/api";
import { cn } from "@/lib/utils";
import lockup from "@/assets/centaur-lockup-white.svg";

export function AuthLayout() {
  return (
    <AuthFrame>
      <Outlet />
    </AuthFrame>
  );
}

/** Full-height centered column with a faint brand glow behind the card. */
function AuthFrame({ children }: { children: ReactNode }) {
  return (
    <main className="relative isolate flex min-h-svh items-center justify-center overflow-hidden bg-background px-6 py-16 text-foreground">
      <div
        aria-hidden="true"
        className="pointer-events-none absolute inset-x-0 top-0 -z-10 h-[28rem] bg-[radial-gradient(ellipse_at_top,color-mix(in_oklab,var(--primary)_14%,transparent),transparent_70%)]"
      />
      <div className="w-full max-w-sm">{children}</div>
    </main>
  );
}

/** Logo and subtitle above the card. */
export function AuthHeader({ subtitle }: { subtitle: string }) {
  return (
    <div className="mb-8 text-center">
      <img src={lockup} alt="Centaur" width={497} height={127} className="logo-themed mx-auto h-8 w-auto" />
      <p className="mt-4 text-sm text-muted-foreground">{subtitle}</p>
    </div>
  );
}

export function AuthCard({ children, className }: { children: ReactNode; className?: string }) {
  return <Card className={cn("gap-0 p-6 shadow-lg shadow-black/5", className)}>{children}</Card>;
}

/** Neutral notice box above the card (e.g. "Signed out."). */
export function AuthNotice({ children }: { children: ReactNode }) {
  return (
    <div role="status" className="mb-6 rounded-lg border bg-card px-4 py-3 text-sm text-muted-foreground">
      {children}
    </div>
  );
}

export function AuthLoading() {
  return (
    <div role="status" className="flex items-center justify-center gap-2 text-sm text-muted-foreground">
      <Loader2 className="size-4 animate-spin" aria-hidden="true" />
      Loading…
    </div>
  );
}

/** Error boundary for the auth routes (they sit outside the shell's). */
export function AuthRouteError() {
  const error = useRouteError();
  const message = isRouteErrorResponse(error) ? error.statusText : errorMessage(error);
  return (
    <AuthFrame>
      <AuthHeader subtitle="Operator console." />
      <AuthCard>
        <h1 className="text-lg font-semibold text-destructive">Something went wrong</h1>
        <p className="mt-2 text-sm text-muted-foreground">{message}</p>
      </AuthCard>
    </AuthFrame>
  );
}
