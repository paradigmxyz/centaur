import { isRouteErrorResponse, useRouteError } from "react-router";

import { ApiError, errorMessage } from "@/lib/api";
import { Landing } from "@/app/shell/Landing";
import { NotFound } from "@/app/shell/NotFound";
import { PageHeader } from "@/components/page-header";

export function RouteError() {
  const error = useRouteError();
  if (error instanceof ApiError && error.status === 404) return <NotFound />;
  if (isRouteErrorResponse(error) && error.status === 404) return <NotFound />;
  // Lost admin access (descoped elsewhere, demoted): back to the landing page.
  if (error instanceof ApiError && error.reason === "admin_required") return <Landing />;
  if (error instanceof ApiError && (error.status === 401 || error.reason === "account_pending")) {
    return <p className="p-8 text-sm text-muted-foreground">Redirecting…</p>;
  }

  return (
    <div className="p-8">
      <PageHeader title="Something went wrong" />
      <div role="alert" className="rounded-sm border border-red-500/40 bg-red-500/[0.08] px-4 py-3 text-sm text-destructive-foreground">
        {errorMessage(error)}
      </div>
    </div>
  );
}
