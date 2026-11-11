// Roles slice: list, create, detail (Slack channel permissions and secret
// grants), and edit. Copies the principals reference slice.
import type { FeatureModule } from "@/app/types";

const roles: FeatureModule = {
  id: "roles",
  routes: [
    { path: "/console/roles", lazy: () => import("./pages/RolesList").then((m) => ({ Component: m.default })) },
    { path: "/console/roles/new", lazy: () => import("./pages/RoleNew").then((m) => ({ Component: m.default })) },
    { path: "/console/roles/:id", lazy: () => import("./pages/RoleDetail").then((m) => ({ Component: m.default })) },
    { path: "/console/roles/:id/edit", lazy: () => import("./pages/RoleEdit").then((m) => ({ Component: m.default })) },
  ],
  controlTabs: [
    { id: "roles", label: "Roles", to: "/console/roles", order: 20, visible: (me) => me.nav.roles },
  ],
};

export default roles;
