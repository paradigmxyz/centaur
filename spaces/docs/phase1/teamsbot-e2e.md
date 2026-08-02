# Teamsbot E2E checklist (deferred to always-on host)

Do not run this on a sleeping laptop. Prefer the Droplet migration checklist in
[`../phase0/droplet-migration.md`](../phase0/droplet-migration.md).

1. Lock iron-proxy `domains` allowlist (remove `*`) before registering real OAuth clients.
2. Create HubSpot + Microsoft OAuth apps; set callback to  
   `https://<public-console>/oauth/<slug>/callback`.
3. Enable teamsbot in the values overlay; configure Bot Framework secrets; set
   messaging endpoint to `https://<public-teamsbot>/api/messages`.
4. Fail-closed tenant/team/channel allowlists for the two pilot users only.
5. Each user consents HubSpot (and Microsoft if in scope); grant wrapper secrets
   to their Teams principals.
6. User A asks for a HubSpot read that only A’s portal can satisfy → success.
7. User B mirrors → success with B’s data only; A must not see B’s hosts/secrets.
8. Capture principal ids, grant rows, and redacted proxy effective config.

**Exit:** identity preserved end-to-end for two staff. **Kill:** if sessions do
not bind to the consenting user’s grants.
