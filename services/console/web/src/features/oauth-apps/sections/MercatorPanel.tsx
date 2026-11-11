import { AppLink } from "@/components/app-link";
import { Button } from "@/components/ui/button";

import type { ProviderDetails } from "../api";

type Credential = NonNullable<ProviderDetails["credential"]>;

/** Legacy oauth_apps/providers/_mercator: the shared wallet panel. */
export function MercatorPanel({ credential, details }: { credential: Credential; details: Record<string, unknown> }) {
  const subject = credential.provider_subject ?? "";
  const short = `${subject.slice(0, 6)}…${subject.slice(-4)}`;
  const claimUrl = `https://mercator.sh/slack-claim?${new URLSearchParams({ wallet: subject }).toString()}`;

  return (
    <section className="mb-8 space-y-4 rounded-sm border border-border bg-card p-5" aria-label="Mercator wallet">
      <h2 className="text-sm font-semibold text-heading">Shared wallet</h2>
      <a
        href={`https://explore.tempo.xyz/address/${encodeURIComponent(subject)}`}
        title={subject}
        target="_blank"
        rel="noopener noreferrer"
        className="font-mono text-sm text-centaur-400 hover:text-centaur-300 hover:underline light:text-primary"
      >
        {short} ↗
      </a>
      <div className="flex flex-wrap gap-3">
        <Button asChild variant="secondary">
          <a href="https://mercator.sh/account" target="_blank" rel="noopener noreferrer">
            Manage wallet ↗
          </a>
        </Button>
        {details.claimed === true ? (
          <Button type="button" disabled>
            MACH already claimed
          </Button>
        ) : (
          <Button asChild>
            <a href={claimUrl} target="_blank" rel="noopener noreferrer">
              Claim MACH
            </a>
          </Button>
        )}
        {credential.static_secret_id && (
          <Button asChild variant="secondary">
            <AppLink to={`/console/secrets/static/${credential.static_secret_id}#roles`}>Agent access</AppLink>
          </Button>
        )}
      </div>
    </section>
  );
}
