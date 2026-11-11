// One-shot, in-memory handoff of a freshly issued API client secret from the
// create page to the client's detail page. Nothing is written to history
// state, storage, or the query cache, so a reload or a later visit can never
// show the secret again.
import { useEffect, useState } from "react";

const pending = new Map<string, string>();

export function stashIssuedSecret(clientId: string, secret: string) {
  pending.set(clientId, secret);
}

/**
 * The secret handed off for `clientId`, held in component state only. The
 * handoff is consumed on mount (peek in the initializer, delete in an effect,
 * so StrictMode's double render still sees it).
 */
export function useIssuedSecret(clientId: string) {
  const [secret, setSecret] = useState<string | null>(() => pending.get(clientId) ?? null);
  useEffect(() => {
    pending.delete(clientId);
  }, [clientId]);
  return [secret, setSecret] as const;
}
