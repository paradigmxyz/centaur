// OAuth apps slice: the admin Apps pages (list, detail, new, edit), provider
// presets, and the user-facing Integrations page. Consent itself runs on the
// Rails protocol routes /oauth/:slug/start|callback, linked by full page loads.
import { Link2 } from "lucide-react";

import type { FeatureModule } from "@/app/types";

const oauthApps: FeatureModule = {
  id: "oauth_apps",
  routes: [
    { path: "/console/oauth_apps", lazy: () => import("./pages/OauthAppsList").then((m) => ({ Component: m.default })) },
    { path: "/console/oauth_apps/new", lazy: () => import("./pages/OauthAppNew").then((m) => ({ Component: m.default })) },
    { path: "/console/oauth_apps/:id", lazy: () => import("./pages/OauthAppDetail").then((m) => ({ Component: m.default })) },
    {
      path: "/console/oauth_apps/:id/edit",
      lazy: () => import("./pages/OauthAppEdit").then((m) => ({ Component: m.default })),
    },
    { path: "/console/integrations", lazy: () => import("./pages/Integrations").then((m) => ({ Component: m.default })) },
  ],
  controlTabs: [
    { id: "oauth_apps", label: "Apps", to: "/console/oauth_apps", order: 50, visible: (me) => me.nav.oauth_apps },
  ],
  nav: [
    {
      id: "integrations",
      label: "Integrations",
      to: "/console/integrations",
      icon: Link2,
      placement: "primary",
      order: 10,
      visible: (me) => me.nav.integrations,
    },
  ],
};

export default oauthApps;
