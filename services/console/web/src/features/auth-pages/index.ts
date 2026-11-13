// Sign-in is a step of the MCP OAuth flow, so /login and /pending stay
// server-rendered by Rails (layouts/oauth) rather than SPA routes.
import type { FeatureModule } from "@/app/types";

const authPages: FeatureModule = {
  id: "auth_pages",
};

export default authPages;
