// Roles slice.
//
// Stub until this slice moves to the SPA. Nav entries and Control tabs below
// point at the legacy Rails pages; once the slice registers SPA routes for the
// same paths, the shell links to them client-side automatically.
import type { FeatureModule } from "@/app/types";

const roles: FeatureModule = {
  id: "roles",
  routes: [],
  controlTabs: [
    { id: "roles", label: "Roles", to: "/console/roles", order: 20, visible: (me) => me.nav.roles },
  ],
};

export default roles;
