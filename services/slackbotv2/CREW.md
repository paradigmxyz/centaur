# Crew: named Slackbots

Crew gives each bot its own Slack app, fully editable system prompt, skills,
model defaults, and credential roles. Crew bots have no base profile or profile
registry dependency. Slackbot stores app credentials encrypted
with AES-256-GCM in Postgres. Console owns behavior and access configuration.

## Enable

Configure the service Secret (never plaintext Helm values):

- `SLACK_CONFIGURATION_TOKEN`: a workspace app configuration **access** token
  with `app_configurations:write`. Ordinary `xoxb` bot tokens are insufficient.
  Access tokens expire after 12 hours; this service does not yet rotate them.
- `SLACK_CREW_ADMIN_TOKEN`: a separate high-entropy management credential.
- `SLACK_CREW_ENCRYPTION_KEY`: 32 random bytes as 64 hex characters. Keep this
  stable; changing it requires re-encrypting the stored credentials.

```yaml
slackbotv2:
  crew:
    enabled: true
    publicUrl: https://your-slack-ingress.example.com
```

Helm wires the Console's Slackbot management URL and server-only admin
token. Self-management also uses the Console's existing API session DB connection.
The public ingress must forward `/api/slack/crew` and subpaths.

## Console management and automatic installation

Open **Crew → Create bot** as a Console admin. Set identity, system prompt,
custom skills, Codex/Claude default models, and secret roles.
**Create and install bot** creates and installs the Slack app automatically.

Installation reuses the preview provisioner's mechanism:
`apps.manifest.create` followed by configuration-token-authenticated
`apps.developerInstall` with `app_id` and a JSON array of `bot_scopes`.
The returned `api_access_tokens.bot` is verified with `auth.test` against the
configured workspace. It does **not** require a browser OAuth consent flow or
the partner-only managed-apps API. Workspace restrictions may still reject an
installation. Invite an installed bot to a channel or open its DM.

Each bot has a dedicated editor and a Console principal
`slack-crew-<team lowercase>-<app lowercase>`. Admins explicitly attach existing
credential roles to that principal. There are no implicit default roles; select
the infrastructure/model-provider roles the bot needs. Role changes use the
existing proxy sync/invalidation mechanism and do not grant access to another
bot or the shared channel principal. Channel permissions can also be supplied
by roles or configured on the bot principal.

The Crew system prompt replaces Centaur's base, deployment overlay, and persona
prompts completely, including when it is empty. Provider/harness built-in
instructions and code-enforced permissions still apply. Slackbot does not send
personas, and the API skips requested, default, and legacy stored personas for
Crew principals.
New Console forms start with the editable six-section template in
`services/console/config/crew_system_prompt.md`. Saving copies the text to that
bot; later template changes never overwrite saved prompts. Existing bots keep
their saved text (formerly additive), now used as their complete Crew prompt.
Custom skills become isolated
`SKILL.md` files alongside standard skills; replacing a standard skill with the
same name is intentional, but the reserved `search` skill cannot be replaced.
Model IDs map to `CODEX_MODEL` (Codex/Nanocodex) and `CLAUDE_MODEL` (Claude Code).
Explicit model overrides retain precedence. Running sandboxes keep their current
configuration; new threads receive the latest configuration when their sandbox
is created, and existing threads receive it after a sandbox rebuild. Stale editor
writes are rejected.

Pause stops new incoming work, not an execution or delivery already in flight.
The Slack identity remains immutable.

## Bot self-management

```bash
slack-crew me --json
slack-crew edit --name "Research" --description "Research and analysis"
slack-crew edit --prompt-file instructions.md --skills-file skills.json --codex-model MODEL_ID
```

Skills JSON replaces the bot's custom skill list:

```json
[{"name":"reviewing-incidents","description":"Reviews incidents. Use when investigating failures.","content":"Follow the runbook and cite evidence."}]
```

Commands use `GET/PATCH /api/v1/sandbox/crew/me` through iron-proxy's short-lived
sandbox JWT. Console verifies the unique current durable sandbox assignment,
principal and app-scoped thread key, then matches the bot's configured principal.
No bot selector or shared admin token is accepted. Bots can edit only their own
name, description, prompt, skills and model defaults. They may read their own
role names but **cannot grant/revoke roles**, pause bots, create
bots, or manage another bot. Behavior edits use optimistic revisions.
Paused, unassigned, non-Crew and ambiguous sessions fail closed.

Name/description updates use `apps.manifest.update` and require a current
configuration token. Local prompt/skill/model edits do not call Slack manifest APIs.
Remove any historical grants of `SLACK_CREW_ADMIN_TOKEN` from sandbox roles or
principals; it belongs only to the Console-to-Slackbot management connection.

## Permissions and recovery

App scopes cover mentions/replies, assistant streaming, conversation history,
DMs, user identity and incoming files. There are no workspace-admin, user-token,
channel auto-join, icon-impersonation or app-deletion scopes.

Each app has separate Chat SDK state and session keys:
`slack:<team>:<app>:<channel>:<thread_ts>`. API resolves only the existing
Console-provisioned bot principal, never a caller-supplied bot identity or a
shared channel principal. Webhooks verify the app's own signing secret and
replies use its own token. Existing requester credential rules still apply.
Tool-originated uploads and scheduled deliveries retain the deployment Slack identity.

The permanent bot ID deduplicates app creation. A conflicting definition is
rejected. Credentials are persisted before installation, and an `installing`
claim is committed **before** contacting Slack. Explicit Slack rejection leaves
a known app in `needs_install` so an admin can retry without creating another app.
A crash, timeout, malformed success, or failed final persistence leaves an
ambiguous `creating`/`installing` state that requires operator reconciliation;
do not blindly retry or create under a new ID. Existing active install requests
are idempotent. Back up both the Slackbot encrypted table and Console Crew records,
and retain the encryption key.

Deploy the Console migration, API and Slackbot changes together. Older Crew
threads bound to shared channel principals cannot silently change credential
identity: configure their bot in Console and start a new thread.
