// Admin slice: Users, API Clients, and Settings (Control tabs, acting admins
// only). Descope ("view as operator") lives in the shell's account menu and
// banner.
import { redirect } from "react-router";

import type { FeatureModule } from "@/app/types";

const admin: FeatureModule = {
  id: "admin",
  routes: [
    { path: "/console/users", lazy: () => import("./pages/UsersPage").then((m) => ({ Component: m.default })) },
    {
      // Scopes the one-shot issued-secret handoff (create -> detail) to these routes.
      path: "/console/api_clients",
      lazy: () => import("./secret-handoff").then((m) => ({ Component: m.IssuedSecretScope })),
      children: [
        { index: true, lazy: () => import("./pages/ApiClientsList").then((m) => ({ Component: m.default })) },
        { path: "new", lazy: () => import("./pages/ApiClientNew").then((m) => ({ Component: m.default })) },
        { path: ":id", lazy: () => import("./pages/ApiClientDetail").then((m) => ({ Component: m.default })) },
        { path: ":id/edit", lazy: () => import("./pages/ApiClientEdit").then((m) => ({ Component: m.default })) },
      ],
    },
    { path: "/console/settings", loader: () => redirect("/console/settings/edit") },
    { path: "/console/settings/edit", lazy: () => import("./pages/SystemSettingsPage").then((m) => ({ Component: m.default })) },
  ],
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
