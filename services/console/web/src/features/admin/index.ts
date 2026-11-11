// Admin slice (users, API clients, system settings).
//
// Stub until this slice moves to the SPA. Nav entries and Control tabs below
// point at the legacy Rails pages; once the slice registers SPA routes for the
// same paths, the shell links to them client-side automatically.
import type { FeatureModule } from "@/app/types";

const admin: FeatureModule = {
  id: "admin",
  routes: [],
  controlTabs: [
    { id: "users", label: "Users", to: "/console/users", order: 60, visible: (me) => me.nav.users },
    {
      id: "api_clients",
      label: "API Clients",
      to: "/console/api_clients",
      order: 70,
      visible: (me) => me.nav.api_clients,
    },
    {
      id: "settings",
      label: "Settings",
      to: "/console/settings/edit",
      match: "/console/settings",
      order: 80,
      visible: (me) => me.nav.settings,
    },
  ],
};

export default admin;
