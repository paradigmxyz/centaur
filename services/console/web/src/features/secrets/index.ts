// Secrets slice (secrets list/detail/forms and the personal SSH key page).
//
// Stub until this slice moves to the SPA. Nav entries and Control tabs below
// point at the legacy Rails pages; once the slice registers SPA routes for the
// same paths, the shell links to them client-side automatically.
import type { FeatureModule } from "@/app/types";

const secrets: FeatureModule = {
  id: "secrets",
  routes: [],
  controlTabs: [
    { id: "secrets", label: "Secrets", to: "/console/secrets", order: 30, visible: (me) => me.nav.secrets },
  ],
};

export default secrets;
