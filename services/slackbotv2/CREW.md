# Crew: named Slackbots

Crew gives an existing Centaur profile its own mentionable Slack app and DM.
Creation uses `apps.manifest.create`; installation uses Slack OAuth v2. The
service stores app credentials encrypted with AES-256-GCM in Postgres and
rehydrates active installations and their render recovery on restart.

## Enable

Configure the service Secret (not plaintext Helm values):

- `SLACK_CONFIGURATION_TOKEN`: the existing workspace app configuration token,
  with `app_configurations:write`. An ordinary `xoxb` bot token is insufficient.
  Slack configuration access tokens expire; supply a current token through the
  deployment's secret rotation mechanism. This feature does not refresh them.
- `SLACK_CREW_ADMIN_TOKEN`: a separate, high-entropy management credential.
- `SLACK_CREW_ENCRYPTION_KEY`: 32 random bytes encoded as 64 hex characters.
  Keep it stable across restarts and replicas; changing it requires re-encryption.

```yaml
slackbotv2:
  crew:
    enabled: true
    publicUrl: https://your-slack-ingress.example.com
    allowedProfiles: [eng]
```

The service uses its existing Postgres connection and home workspace from
`auth.test`. Only listed profile IDs can be bound. They must also exist in the
API's profile registry; missing profiles fail closed before message execution.
No new API key or app credentials are exposed to sandboxes.

Grant the `slack_crew` tool and its management credential only to operators who
may create Slack apps. Its default target is `http://centaur-slackbotv2:3001`.
For another service name set `SLACK_CREW_URL` and the tool's credential host rule
to that exact host. The credential proxy must be able to reach that service.
The public ingress must forward `/api/slack/crew` and its subpaths here. The
management GET/POST require the separate bearer credential; installation uses
unguessable tickets, browser-bound OAuth state, expiry, and atomic replay checks.

```bash
slack-crew create research --name "Research" --crew eng --json
slack-crew list --json
```

Open the returned `install_url` and complete Slack consent. Workspace policy may
require admin approval. Creation does not bypass installation. Invite the new
bot to its intended channels or start a DM after status becomes `active`.

## Slack permissions

App provisioning requires [`app_configurations:write`](https://docs.slack.dev/reference/methods/apps.manifest.create/)
on a **configuration token**. Each installed bot requests:

| Scopes | Ingress use |
| --- | --- |
| `app_mentions:read`, `chat:write` | Mentions and replies |
| `assistant:write` | Streaming and assistant status |
| `channels:history`, `channels:read`, `groups:history`, `groups:read` | Context in conversations the bot belongs to |
| `im:history`, `im:read`, `im:write`, `mpim:history`, `mpim:read` | DM and group-DM handling |
| `users:read`, `users:read.email` | Existing requester identity and attribution |
| `files:read` | Incoming attachments |

There are no admin, user-token, channel auto-join, icon impersonation, or
app-deletion scopes. Slack's [`oauth.v2.access`](https://docs.slack.dev/reference/methods/oauth.v2.access/)
exchanges an approved install code for the bot token. No automatic Slack
mutation retries are performed.

## Boundaries and recovery

Each app has separate Chat SDK state and durable session keys of the form
`slack:<team>:<app>:<channel>:<thread_ts>`. The original channel/thread remains
the delivery destination. The fixed profile overrides inline profile flags.
Replies use that app's own token and webhooks verify that app's signing secret.
Existing user/channel connector grants still apply: this does not introduce
per-bot credential grants or permit prompts to elevate access. Tool-originated
uploads and scheduled deliveries continue using the deployment's existing Slack
proxy; only ingress-rendered conversational replies use the new bot identity.

`id` is the durable creation idempotency key. A repeated request with identical
parameters returns the existing app. A different definition with the same ID
is rejected. If a process dies after Slack created the app but before persisting
credentials, the row stays `creating`; **do not create under another ID** without
checking Slack's app dashboard. An interrupted OAuth exchange stays `installing`.
These ambiguous states require operator reconciliation; no retry can safely
recover a lost single-use response. Back up this table with the Chat SDK database
and retain the encryption key in the deployment secret store.

Model routing, per-member icons, routine editors, and self-service profile
authoring are separate from this initial identity/installation capability.
