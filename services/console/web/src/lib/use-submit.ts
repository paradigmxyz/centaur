// Pending/error state for one write. Writes are plain async functions (each
// feature's api.ts: request, then invalidate the affected query keys); this
// hook only tracks whether one is in flight and the last error, in component
// state. It never sees or keeps the request body.
//
//   const save = useSubmit(async (data: RoleFormData) => updateRole(queryClient, id, data));
//   await save.run(data);          // rethrows, like any async call
//   <Button disabled={save.pending}>  <FormErrorSummary error={save.error} />
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
