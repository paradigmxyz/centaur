// OAuth apps slice (apps, presets, and the user-facing Integrations page).
//
// Stub until this slice moves to the SPA. Nav entries and Control tabs below
// point at the legacy Rails pages; once the slice registers SPA routes for the
// same paths, the shell links to them client-side automatically.
import { Link2 } from "lucide-react";

import type { FeatureModule } from "@/app/types";

const oauthApps: FeatureModule = {
  id: "oauth_apps",
  routes: [],
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
