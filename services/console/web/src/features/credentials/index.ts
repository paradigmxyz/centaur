import type { FeatureModule } from "@/app/types";

const credentials: FeatureModule = {
  id: "credentials",
  routes: [
    { path: "/console/credentials", lazy: () => import("./pages/CredentialsList").then((m) => ({ Component: m.default })) },
    { path: "/console/credentials/new", lazy: () => import("./pages/CredentialNew").then((m) => ({ Component: m.default })) },
    { path: "/console/credentials/:id", lazy: () => import("./pages/CredentialDetail").then((m) => ({ Component: m.default })) },
    {
      path: "/console/credentials/:id/edit",
      lazy: () => import("./pages/CredentialEdit").then((m) => ({ Component: m.default })),
    },
  ],
  controlTabs: [
    {
      id: "credentials",
      label: "Credentials",
      to: "/console/credentials",
      order: 40,
      visible: (me) => me.nav.credentials,
    },
  ],
};

export default credentials;
