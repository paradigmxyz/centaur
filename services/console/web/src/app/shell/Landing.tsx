import { Navigate } from "react-router";

import { defaultLandingPath, useSession } from "@/app/session";

/** "/" -> admins to Principals, everyone else to Integrations. */
export function Landing() {
  const me = useSession();
  return <Navigate to={defaultLandingPath(me)} replace />;
}
