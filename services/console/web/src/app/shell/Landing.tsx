import { useEffect } from "react";
import { Navigate } from "react-router";

import { defaultLandingPath, useSession } from "@/app/session";
import { useShell } from "@/app/shell-context";

/** "/" -> admins to Principals, everyone else to Integrations. */
export function Landing() {
  const me = useSession();
  const { isSpaPath } = useShell();
  const target = defaultLandingPath(me);
  const spa = isSpaPath(target);

  useEffect(() => {
    if (!spa) window.location.replace(target);
  }, [spa, target]);

  return spa ? <Navigate to={target} replace /> : null;
}
