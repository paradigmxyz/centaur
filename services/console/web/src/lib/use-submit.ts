// Pending/error state for one write. It never sees or keeps the request body,
// which may carry secrets.
import { useCallback, useState } from "react";

export function useSubmit<Args extends unknown[], Result>(fn: (...args: Args) => Promise<Result>) {
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<unknown>(null);

  const run = async (...args: Args): Promise<Result> => {
    setPending(true);
    setError(null);
    try {
      return await fn(...args);
    } catch (caught) {
      setError(caught);
      throw caught;
    } finally {
      setPending(false);
    }
  };

  const clearError = useCallback(() => setError(null), []);

  return { run, pending, error, clearError };
}
