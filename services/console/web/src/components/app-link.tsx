import type { AnchorHTMLAttributes } from "react";
import { Link } from "react-router";

import { useShell } from "@/app/shell-context";

type AppLinkProps = Omit<AnchorHTMLAttributes<HTMLAnchorElement>, "href"> & { to: string };

/**
 * Link to any console path. SPA paths navigate client-side; Rails protocol
 * routes (such as /oauth/:slug/start or /auth/:provider/start) do a full page
 * load. Use this whenever the target may belong to another feature.
 */
export function AppLink({ to, ...props }: AppLinkProps) {
  const { isSpaPath } = useShell();
  if (isSpaPath(to)) return <Link to={to} {...props} />;
  return <a href={to} {...props} />;
}
