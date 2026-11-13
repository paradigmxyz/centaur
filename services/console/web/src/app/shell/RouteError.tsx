import { useQueryClient } from "@tanstack/react-query";
import { useEffect } from "react";
import { isRouteErrorResponse, useRouteError } from "react-router";

import { ApiError, errorMessage } from "@/lib/api";
import { Landing } from "@/app/shell/Landing";
import { NotFound } from "@/app/shell/NotFound";
import { ErrorAlert } from "@/components/alerts";
import { PageHeader } from "@/components/page-header";

export function RouteError() {
  const error = useRouteError();
  useForgetFailedQueries(error);

  if (error instanceof ApiError && error.status === 404) return <NotFound />;
  if (isRouteErrorResponse(error) && error.status === 404) return <NotFound />;
  // Lost admin access (demoted): back to the landing page.
  if (error instanceof ApiError && error.reason === "admin_required") return <Landing />;
  if (error instanceof ApiError && (error.status === 401 || error.reason === "account_pending")) {
    return <p className="text-sm text-muted-foreground">Redirecting…</p>;
  }

  return (
    <div>
      <PageHeader title="Something went wrong" />
      <ErrorAlert>{errorMessage(error)}</ErrorAlert>
    </div>
  );
}

/**
 * TanStack rethrows a cached failed suspense query on remount without
 * refetching, so once the failing page unmounts, drop unobserved failed queries.
 */
function useForgetFailedQueries(error: unknown) {
  const queryClient = useQueryClient();
  useEffect(() => {
    queryClient.removeQueries({
      predicate: (query) => query.state.status === "error" && query.getObserversCount() === 0,
    });
  }, [error, queryClient]);
}
