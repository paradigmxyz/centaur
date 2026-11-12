import type { FeatureModule } from "@/app/types";

const secrets: FeatureModule = {
  id: "secrets",
  routes: [
    { path: "/console/secrets", lazy: () => import("./pages/SecretsList").then((m) => ({ Component: m.default })) },
    { path: "/console/secrets/:kind/new", lazy: () => import("./pages/SecretNew").then((m) => ({ Component: m.default })) },
    { path: "/console/secrets/:kind/:id", lazy: () => import("./pages/SecretDetail").then((m) => ({ Component: m.default })) },
    { path: "/console/secrets/:kind/:id/edit", lazy: () => import("./pages/SecretEdit").then((m) => ({ Component: m.default })) },
  ],
  controlTabs: [
    { id: "secrets", label: "Secrets", to: "/console/secrets", order: 30, visible: (me) => me.nav.secrets },
  ],
};

export default secrets;
