// Auth pages slice. Sign-in is a step of the MCP OAuth flow, so the login and
// pending pages (/login, /pending), like the OAuth consent and result pages,
// are intentionally server-rendered by Rails (layouts/oauth) and are not SPA
// routes: the api client and the shell send the browser there with a full page
// load. The SPA keeps everything after sign-in; sign-out is
// DELETE /api/v1/session from the account menu. PWA registration lives here
// (pwa.ts, run from main.tsx).
import type { FeatureModule } from "@/app/types";

const authPages: FeatureModule = {
  id: "auth_pages",
};

export default authPages;
