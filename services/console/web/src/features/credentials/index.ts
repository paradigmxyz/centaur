// Credentials slice (broker credentials).
//
// Stub until this slice moves to the SPA. Nav entries and Control tabs below
// point at the legacy Rails pages; once the slice registers SPA routes for the
// same paths, the shell links to them client-side automatically.
import type { FeatureModule } from "@/app/types";

const credentials: FeatureModule = {
  id: "credentials",
  routes: [],
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
