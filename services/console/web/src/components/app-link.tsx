import type { AnchorHTMLAttributes } from "react";
import { Link } from "react-router";

import { useShell } from "@/app/shell-context";

type AppLinkProps = Omit<AnchorHTMLAttributes<HTMLAnchorElement>, "href"> & { to: string };

/**
 * Link to any console page. SPA paths navigate client-side; paths still served
 * by a legacy Rails page (or by a Rails protocol route such as /oauth/:slug/start)
 * do a full page load. Use this whenever the target may belong to another slice.
 */
export function AppLink({ to, ...props }: AppLinkProps) {
  const { isSpaPath } = useShell();
  if (isSpaPath(to)) return <Link to={to} {...props} />;
  return <a href={to} {...props} />;
}
