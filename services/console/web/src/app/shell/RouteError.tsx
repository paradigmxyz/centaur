import { useQueryClient } from "@tanstack/react-query";
import { useEffect } from "react";
import { isRouteErrorResponse, useRouteError } from "react-router";

import { ApiError, errorMessage } from "@/lib/api";
import { Landing } from "@/app/shell/Landing";
import { NotFound } from "@/app/shell/NotFound";
import { PageHeader } from "@/components/page-header";

export function RouteError() {
  const error = useRouteError();
  useForgetFailedQueries(error);

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

/**
 * A page whose suspense query failed lands here. TanStack keeps that failed
 * query in the cache and, by design, rethrows its error (without refetching)
 * whenever a page reading it mounts again, so revisiting the page would show
 * this error until a hard reload. Once the failing page has unmounted, drop
 * every failed query nobody is observing; the next visit fetches afresh.
 * Queries still on screen (the shell's session, a section's ErrorAlert) keep
 * their state.
 */
function useForgetFailedQueries(error: unknown) {
  const queryClient = useQueryClient();
  useEffect(() => {
    queryClient.removeQueries({
      predicate: (query) => query.state.status === "error" && query.getObserversCount() === 0,
    });
  }, [error, queryClient]);
}
