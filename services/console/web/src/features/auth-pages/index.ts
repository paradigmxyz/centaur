// Auth pages slice: login, pending, SSO/OAuth result pages, MCP consent.
//
// Stub until the slice is ported. These pages render outside the app shell:
// register them as `publicRoutes` (they must not call useSession(), which
// requires a signed-in active account). Add any new top-level path outside
// /console to SpaRequest only if it collides with an excluded prefix.
import type { FeatureModule } from "@/app/types";

const authPages: FeatureModule = {
  id: "auth_pages",
  publicRoutes: [],
};

export default authPages;
