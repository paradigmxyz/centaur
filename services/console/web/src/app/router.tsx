import { createBrowserRouter, type RouteObject } from "react-router";

import { features } from "@/app/registry";
import { AppShell, ShellLoading } from "@/app/shell/AppShell";
import { Landing } from "@/app/shell/Landing";
import { NotFound } from "@/app/shell/NotFound";
import { RouteError } from "@/app/shell/RouteError";
import type { FeatureModule } from "@/app/types";

export function buildRoutes(modules: FeatureModule[] = features): RouteObject[] {
  return [
    ...modules.flatMap((feature) => feature.publicRoutes ?? []),
    {
      element: <AppShell />,
      // Shown while the matched lazy route loads on a full page load (the
      // router's initial "hydration"); same placeholder as the shell's Suspense.
      hydrateFallbackElement: <ShellLoading />,
      errorElement: <RouteError />,
      children: [
        {
          errorElement: <RouteError />,
          children: [
            { path: "/", element: <Landing /> },
            ...modules.flatMap((feature) => feature.routes ?? []),
            { path: "*", element: <NotFound /> },
          ],
        },
      ],
    },
  ];
}

export function createRouter() {
  return createBrowserRouter(buildRoutes());
}
