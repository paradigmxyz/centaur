import type { ReactNode } from "react";

import { useSession, type Me } from "@/app/session";
import { Landing } from "@/app/shell/Landing";

/** Sends a caller who may not see the page to their landing; the API enforces the same rule. */
export function RequireAccess({ allowed, children }: { allowed: (me: Me) => boolean; children: ReactNode }) {
  const me = useSession();
  if (!allowed(me)) return <Landing />;
  return <>{children}</>;
}
