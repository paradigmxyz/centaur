// /mcp/oauth/consent?request=<ticket> — MCP OAuth consent. GET
// /mcp/oauth/authorize validated the client's request and issued the ticket;
// this page shows what is being authorized and posts the explicit decision.
// The browser then goes to the redirect_to the server returns (always the
// client's registered redirect_uri) — never a URL taken from this page's query.
import { useSearchParams } from "react-router";

import { ErrorAlert } from "@/components/alerts";
import { useDocumentTitle } from "@/components/page-header";
import { Button } from "@/components/ui/button";
import { ApiError, errorMessage, navigation } from "@/lib/api";

import { useMcpAuthorization, useMcpDecision } from "../api";
import { AuthCard, AuthHeader, AuthLoading } from "../sections/AuthLayout";

const MISSING_TICKET = "This authorization request is invalid or has expired. Start again from your MCP client.";

function DetailRow({ label, children }: { label: string; children: string }) {
  return (
    <div>
      <dt className="text-xs font-semibold tracking-wider text-muted-foreground uppercase">{label}</dt>
      <dd className="mt-1 break-all text-heading">{children}</dd>
    </div>
  );
}

/** The client's error redirect the server vouched for (422 invalid_authorization_request). */
function clientErrorRedirect(error: unknown): string | null {
  if (!(error instanceof ApiError) || error.reason !== "invalid_authorization_request") return null;
  const target = error.details?.redirect_to;
  return typeof target === "string" && target !== "" ? target : null;
}

export default function McpConsentPage() {
  useDocumentTitle("Authorize MCP Client");
  const [searchParams] = useSearchParams();
  const ticket = searchParams.get("request") ?? "";
  const authorization = useMcpAuthorization(ticket);
  const decision = useMcpDecision(ticket);

  const decide = (choice: "approve" | "deny") =>
    decision.mutate(choice, { onSuccess: (result) => navigation.assign(result.redirect_to) });

  const failure = ticket === "" ? MISSING_TICKET : authorization.isError ? errorMessage(authorization.error) : null;
  const decisionFailure = decision.isError ? errorMessage(decision.error) : null;
  const returnTo = clientErrorRedirect(authorization.error) ?? clientErrorRedirect(decision.error);
  const busy = decision.isPending || decision.isSuccess;

  return (
    <>
      <AuthHeader subtitle="Authorize MCP access." />
      {failure ? (
        <AuthCard>
          <ErrorAlert className="mb-0">{failure}</ErrorAlert>
          {returnTo && <ReturnToClient href={returnTo} />}
        </AuthCard>
      ) : !authorization.data ? (
        <AuthLoading />
      ) : (
        <AuthCard>
          <div className="space-y-5">
            <div>
              <p className="text-sm font-semibold text-heading">{authorization.data.client.name}</p>
              <p className="mt-1 text-xs text-muted-foreground">{authorization.data.redirect_host}</p>
            </div>

            <dl className="space-y-3 border-y border-border py-4 text-sm">
              <DetailRow label="Resource">{authorization.data.resource}</DetailRow>
              <DetailRow label="Scope">{authorization.data.scopes.join(" ")}</DetailRow>
              <DetailRow label="Signed in as">{authorization.data.user.email}</DetailRow>
            </dl>
          </div>

          {decisionFailure && <ErrorAlert className="mt-6 mb-0">{decisionFailure}</ErrorAlert>}
          {returnTo && <ReturnToClient href={returnTo} />}

          {!returnTo && (
            <div className="mt-6 grid grid-cols-2 gap-3">
              <Button type="button" variant="secondary" disabled={busy} onClick={() => decide("deny")}>
                Deny
              </Button>
              <Button type="button" disabled={busy} onClick={() => decide("approve")}>
                Allow
              </Button>
            </div>
          )}
        </AuthCard>
      )}
    </>
  );
}

function ReturnToClient({ href }: { href: string }) {
  return (
    <div className="mt-6">
      <Button type="button" variant="secondary" className="w-full" onClick={() => navigation.assign(href)}>
        Return to the application
      </Button>
    </div>
  );
}
