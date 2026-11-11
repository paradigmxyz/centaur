import { ErrorAlert } from "@/components/alerts";
import { Button } from "@/components/ui/button";
import { errorMessage } from "@/lib/api";

/**
 * A failed query (initial load or background refetch). Pages show this
 * instead of an empty state, so a failed request never reads as "No X yet."
 */
export function QueryErrorAlert({ what, error, onRetry }: { what: string; error: unknown; onRetry?: () => void }) {
  return (
    <ErrorAlert className="flex flex-wrap items-center justify-between gap-3">
      <span>
        Could not load {what}: {errorMessage(error)}
      </span>
      {onRetry && (
        <Button type="button" variant="secondary" size="sm" onClick={onRetry}>
          Retry
        </Button>
      )}
    </ErrorAlert>
  );
}
