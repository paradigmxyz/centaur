import { matchRoutes, type RouteObject } from "react-router";

import { features } from "@/app/registry";

const spaRoutes: RouteObject[] = [
  { path: "/" },
  ...features.flatMap((feature) => [...(feature.routes ?? []), ...(feature.publicRoutes ?? [])]),
];

/** True when `path` (no query string) is handled by an SPA route. */
export function isSpaPath(path: string): boolean {
  const pathname = path.split(/[?#]/)[0] ?? path;
  return (matchRoutes(spaRoutes, pathname)?.length ?? 0) > 0;
}
