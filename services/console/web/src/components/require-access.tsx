import type { ReactNode } from "react";

import { useSession, type Me } from "@/app/session";
import { Landing } from "@/app/shell/Landing";

/**
 * Page guard mirroring the legacy require_admin: a caller who may not see the
 * page is sent silently to their default landing (no error toast). The API
 * enforces the same rule; this only avoids rendering a page that would 403.
 *
 *   <RequireAccess allowed={(me) => me.nav.principals}>...</RequireAccess>
 */
export function RequireAccess({ allowed, children }: { allowed: (me: Me) => boolean; children: ReactNode }) {
  const me = useSession();
  if (!allowed(me)) return <Landing />;
  return <>{children}</>;
}
