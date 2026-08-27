# RFC 0005: Per-Turn Requester Credentials

Status: Draft
Owner: TBD
Target: `services/api-rs`, `services/console`

## Summary

Bind the requesting user's principal to the sandbox proxy for each turn,
alongside the conversation principal. A user's connected OAuth credentials
(for example GitHub) then work on turns they start in shared channels: the
bot opens PRs as the requester instead of a shared machine account, with no
admin steps per user.

Only OAuth credentials are ever served this way, and only from integrations
an admin explicitly allowlisted; everything else a user principal holds stays
DM-only.

The end-to-end flow:

1. A user signs into console via Slack SSO once. `UserIdentity` stores their
   Slack user id and team id.
2. The user connects GitHub once via OAuth Apps.
3. Reconciliation auto-grants the credential's wrapper secret to the user's
   `slack-user-*` principal, matched through the Slack SSO identity.
4. The user @-mentions the bot in a channel. slackbotv2 already sends
   `slack_user_id` and `slack_team_id` in every execute's metadata.
5. api-rs upserts the requester's `slack-user-*` principal and passes it as
   an optional requester principal on the proxy assignment.
6. Console renders the proxy config as the union of the conversation
   principal's grants and the requester's always-available credentials.
7. The existing config barrier applies the new config before the turn's
   input runs. `git push` / `gh pr create` egress carries the requester's
   token.

## Motivation

Per-user credentials only take effect in 1:1 DMs today. A DM thread runs as
the user's own principal, so a granted personal credential works there. A
channel thread runs as the channel principal, shared by everyone in the
channel. A personal credential granted there would be shared with the whole
channel.

Auto-granting is also broken for GitHub in practice: reconciliation matches
GitHub credentials by `provider_email`, the public GitHub profile email,
which is usually empty (the consent flow requests no email scope). The Slack
SSO identity is not consulted at all. The grant silently never happens and an
admin has to create it by hand.

The outcome we want: connect GitHub once, then every channel mention runs
with the channel's credentials plus your own GitHub credential. PRs open as
you. Other users' turns never get access to your credential.

## Goals

- PRs and other provider actions in channels are attributed to the requester.
- Zero admin steps per user: Slack SSO once, OAuth consent once.
- Channel turns keep the conversation principal's grants unchanged.
- Other users' turns never include the credential. It is absent from their
  rendered config, not merely lower priority.
- Default deny: an integration's credentials stay out of channels until an
  admin enables that integration.
- DM behavior is unchanged.

## Non-Goals

- Sub-turn (per-message) credential switching. The binding unit is the turn.
- Per-channel availability policy.
- User-configurable availability in v1. The admin sets the ceiling and the
  consent page discloses the behavior. A narrowing-only user opt-out can
  follow later.
- Changing manual grants. Admins can still grant any secret to any
  principal, exactly as today.
- Narrowing what a credential can do. Provider scopes remain the provider's
  concern.

## Current State

- `derive_principal` keys a session on the conversation. A `slack-user-*`
  principal exists only for 1:1 DMs. The channel branch deliberately ignores
  the actor (test: `channel_keys_on_the_channel_even_with_a_user`).
- A proxy binds exactly one principal. `POST /api/v1/proxy/sync` renders that
  principal's effective grants. Two principals' grants cannot be served
  together.
- slackbotv2 resolves the requester and sends `slack_user_id` and
  `slack_team_id` in the metadata of every execute, channels included.
- The proxy assignment is applied at sandbox creation and re-applied on each
  execution of an existing sandbox, with a config barrier before input runs
  (`ensure_iron_control_proxy_resources`).
- Executions are serialized per thread
  (`session_executions_one_active_idx`).
- `PrincipalCredentialReconciliation` auto-grants OAuth wrapper secrets to
  user principals. GitHub matches by `provider_email` only. `UserIdentity`
  has no callbacks, so an SSO login that happens after the GitHub consent
  never triggers a grant.

## Design

### 1. Requester principal per turn (api-rs)

Today a `slack-user-*` principal is created only when a user DMs the bot.
This change also creates and uses it from channel turns. On each execute:

1. Read the requester (`slack_user_id`, `slack_team_id`) from the execute
   metadata. slackbotv2 already sends these on every execute.
2. Upsert the requester's principal: the same foreign_id and labels that
   DM sessions produce today. A user therefore has exactly one principal,
   whether first seen in a DM or a channel. New principals receive the
   namespace's default roles console-side (`Role.assign_by_default`).
   (This needs a small helper: `derive_principal` builds this shape only for
   DM thread keys.)
3. Put the principal's id on the proxy assignment for this turn, as an
   optional `requester_principal_id`. Clear the field when the metadata has
   no requester (workflow executions).

The session's own principal, the conversation, is untouched. No requester
in the metadata means exactly today's behavior.

Console-driven runs carry a requester too: the console provisions the
authenticated user's console-user principal
(`ConsoleUserPrincipalProvisioner`) and passes its foreign ID as
`requester_principal_foreign_id` in the execute metadata. api-rs resolves it
fetch-only for every execution submitted by the authenticated console service,
including replies to readable Slack threads, and never upserts console-user
principals, whose identity fields and reconciliation the console owns. The API
server strips `requester_principal_foreign_id` from every other caller class
before persisting the execution, so ingress callers cannot assert a console
identity through metadata.

### 2. Grant union on the proxy (console)

Add nullable `proxies.requester_principal_id`, accepted by the proxy
assignment API. The rendered proxy config becomes the union of:

- the conversation principal's full effective grants (direct + roles), and
- the requester principal's **direct** grants wrapping an OAuth credential
  whose integration is marked always-available (section 3).

The requester's roles are ignored: shared infrastructure already comes from
the conversation principal. Secret kinds other than OAuth wrapper secrets
never join the union.

No new conflict logic is needed:

- Effective priority is the MAX over grant rows, regardless of which
  principal contributed them.
- Direct grants (100) outrank role grants (0).
- Cross-type suppression already withholds the weaker claimant of the same
  host/header.

So on the requester's turns their GitHub secret wins over a role-granted
shared PAT. On everyone else's turns the PAT serves alone.

`config_hash` must include the requester principal oid and its cache
version, so the sync poll and the barrier observe requester swaps. Postgres
DSN entries and the api-server JWT stay keyed on the conversation principal.

One open implementation choice: config snapshots are cached per principal
and their rendered form drops grant priorities, so the union cannot be built
by merging two cached snapshots. It needs either pair-keyed snapshots or
live assembly from grant rows on hash mismatch.

### 3. Availability policy (console)

`oauth_apps.always_available`, boolean, default false, admin-set. This is
the explicit whitelist of which credentials may be used per-turn: nothing is
hoisted into shared threads unless an admin enabled it for that integration.

Semantics: connected credentials are always bound to the connecting user and
automatically used in their DMs (unchanged). With the flag set, the
credential is also used on any turn the connecting user starts, channels
included. The setting widens *where* the credential follows its owner, never
*who* can use it.

The consent and Integrations pages disclose the resulting behavior ("used
automatically whenever you ask the bot" vs "used only in your DMs"). Manual
grants are unaffected either way.

### 4. Identity-anchored auto-grant (console)

Extend `PrincipalCredentialReconciliation` so providers without a native
subject (GitHub) match through the credential owner's Slack SSO identity:

- `credential.created_by` must have exactly one Slack `UserIdentity` with
  subject and team present (the same ambiguity refusal as
  `Mcp::OauthController#slack_identity_labels_for`), and
- it must equal the principal's first-class `slack_user_id` and
  `slack_team_id`, in the same namespace.

Never match on `github_handle` (a self-asserted Slack profile field) or
`provider_email`.

Add an `after_commit` on `UserIdentity` (provider slack) that re-runs
reconciliation over the user's credentials. Together with the existing hooks
on `Principal`, `BrokerCredential`, and `StaticSecret`, the grant then
happens under every ordering: consent first, SSO first, or the user
principal created later at the user's first mention.

## Security Considerations

Trust anchors: `created_by` is set server-side from the authenticated console
session at consent time and never overwritten. The Slack identity comes from
Slack's OIDC id_token. The principal foreign_id is derived from
Slack-signature-verified events. Nothing user-editable participates in
matching.

Isolation: the requester binding is applied through the config barrier before
the turn's input runs, executions are thread-serialized, and the token only
exists at proxy egress; the sandbox env carries a placeholder. Another
user's turn renders a config in which the credential is absent.
