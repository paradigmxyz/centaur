import { useProviderDetails, type OauthApp } from "../api";
import { MercatorPanel } from "./MercatorPanel";

/** Providers with a detail-page panel (legacy oauth_apps/providers/_<provider>). */
const PANELS = { mercator: MercatorPanel } as const;

function hasPanel(provider: string): provider is keyof typeof PANELS {
  return provider in PANELS;
}

/**
 * Provider-specific panel for a shared app's connected account. Details are
 * fetched live and are optional: while loading, or when nothing is connected,
 * no panel is shown.
 */
export function ProviderPanel({ app }: { app: OauthApp }) {
  const enabled = app.shared && hasPanel(app.provider);
  const query = useProviderDetails(app.id, enabled);
  if (!enabled || !query.data?.credential || !hasPanel(app.provider)) return null;
  const Panel = PANELS[app.provider];
  return <Panel credential={query.data.credential} details={query.data.details} />;
}
