import type { FeatureModule } from "@/app/types";

const principals: FeatureModule = {
  id: "principals",
  routes: [
    { path: "/console/principals", lazy: () => import("./pages/PrincipalsList").then((m) => ({ Component: m.default })) },
    { path: "/console/principals/new", lazy: () => import("./pages/PrincipalNew").then((m) => ({ Component: m.default })) },
    { path: "/console/principals/:id", lazy: () => import("./pages/PrincipalDetail").then((m) => ({ Component: m.default })) },
  ],
  controlTabs: [
    { id: "principals", label: "Principals", to: "/console/principals", order: 10, visible: (me) => me.nav.principals },
  ],
};

export default principals;
