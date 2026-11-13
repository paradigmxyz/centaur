import { createContext, useContext } from "react";

import type { ControlTab } from "@/app/types";

export interface ShellContextValue {
  /** Control tabs visible to the current user, in order. */
  controlTabs: ControlTab[];
  /** Whether a path is served by an SPA route (vs. a Rails route such as /oauth/:slug/start). */
  isSpaPath: (path: string) => boolean;
}

export const ShellContext = createContext<ShellContextValue>({
  controlTabs: [],
  isSpaPath: () => false,
});

export function useShell() {
  return useContext(ShellContext);
}
