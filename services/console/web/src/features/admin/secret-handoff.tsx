// One-shot handoff of a freshly issued API client secret from the create page
// to that client's detail page, scoped to the /console/api_clients route
// subtree. Nothing is written to history state, storage, or the query cache.
//
// Cleanup does not depend on the detail page rendering: the scope drops the
// handoff as soon as the location is anywhere but the target detail path, and
// the whole scope (and the secret with it) unmounts when the user leaves the
// subtree or a route error replaces it.
import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import { Outlet, useLocation } from "react-router";

/** Holds at most one pending handoff for the lifetime of the scope. */
class HandoffStore {
  #pending: { clientId: string; secret: string; path: string } | null = null;

  stash(clientId: string, secret: string) {
    this.#pending = { clientId, secret, path: apiClientPath(clientId) };
  }

  peek(clientId: string): string | null {
    return this.#pending?.clientId === clientId ? this.#pending.secret : null;
  }

  take(clientId: string) {
    if (this.#pending?.clientId === clientId) this.#pending = null;
  }

  /** Drops the handoff unless `pathname` is its detail page. */
  keepOnlyAt(pathname: string) {
    if (this.#pending && this.#pending.path !== pathname) this.#pending = null;
  }
}

const HandoffContext = createContext<HandoffStore | null>(null);

export const apiClientPath = (id: string) => `/console/api_clients/${id}`;

/** Layout for the API client routes. `children` replaces the Outlet in tests. */
export function IssuedSecretScope({ children }: { children?: ReactNode }) {
  const [store] = useState(() => new HandoffStore());
  const { pathname } = useLocation();
  useEffect(() => store.keepOnlyAt(pathname), [store, pathname]);
  return <HandoffContext.Provider value={store}>{children ?? <Outlet />}</HandoffContext.Provider>;
}

/** Hands `secret` to the detail page of `clientId`; navigate there next. */
export function useStashIssuedSecret() {
  const store = useContext(HandoffContext);
  return (clientId: string, secret: string) => store?.stash(clientId, secret);
}

/**
 * The secret handed off for `clientId`, moved into component state on mount.
 * Call it above any suspending query so it is consumed even if that query
 * fails.
 */
export function useIssuedSecret(clientId: string) {
  const store = useContext(HandoffContext);
  const [secret, setSecret] = useState<string | null>(() => store?.peek(clientId) ?? null);
  useEffect(() => store?.take(clientId), [store, clientId]);
  return [secret, setSecret] as const;
}
