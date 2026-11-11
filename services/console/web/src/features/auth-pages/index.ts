// Auth pages slice: login, pending, the broker OAuth consent result page, and
// MCP OAuth consent. They render outside the app shell (publicRoutes) and never
// call useSession(): the signed-out and pending browsers that use them would be
// bounced by the shell. PWA registration lives here too (pwa.ts, run from
// main.tsx).
import type { ComponentType } from "react";

import type { FeatureModule } from "@/app/types";

import { AuthLayout, AuthRouteError } from "./sections/AuthLayout";

const page = (load: () => Promise<{ default: ComponentType }>) => () =>
  load().then((m) => ({ Component: m.default }));

const authPages: FeatureModule = {
  id: "auth_pages",
  publicRoutes: [
    {
      Component: AuthLayout,
      ErrorBoundary: AuthRouteError,
      children: [
        { path: "/login", lazy: page(() => import("./pages/Login")) },
        { path: "/pending", lazy: page(() => import("./pages/Pending")) },
        { path: "/oauth/result", lazy: page(() => import("./pages/OauthResult")) },
        { path: "/mcp/oauth/consent", lazy: page(() => import("./pages/McpConsent")) },
      ],
    },
  ],
};

export default authPages;
