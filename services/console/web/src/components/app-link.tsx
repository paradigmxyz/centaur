import type { AnchorHTMLAttributes } from "react";
import { Link } from "react-router";

import { useShell } from "@/app/shell-context";

type AppLinkProps = Omit<AnchorHTMLAttributes<HTMLAnchorElement>, "href"> & { to: string };

/** SPA paths navigate client-side; Rails routes (e.g. /oauth/:slug/start) do a full page load. */
export function AppLink({ to, ...props }: AppLinkProps) {
  const { isSpaPath } = useShell();
  if (isSpaPath(to)) return <Link to={to} {...props} />;
  return <a href={to} {...props} />;
}
