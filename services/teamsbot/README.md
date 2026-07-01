# teamsbot

Microsoft Teams ingress service for Centaur.

This is shaped after Centaur's Slack/Discord services:

1. Receive Teams Bot Framework activities at `/api/messages`.
2. Gate by Teams/channel policy and mention/subscribed-thread state.
3. Serialize the Teams message and attachments into Centaur session messages.
4. Call Centaur session API: create, append, execute, stream events.
5. Render the streamed answer back to Teams, persisting render obligations so a
   restarted process can resume incomplete streams proactively.

## Run Locally

```bash
pnpm install
pnpm test
pnpm simulate "Reply exactly PONG."
```

The simulator starts an in-process mock Centaur API, so it does not need real
Teams credentials.

This service stays focused on Teams transport, attachment hydration, Centaur
session forwarding, durable conversation references, and rendering Centaur
events back to Teams. Organization-specific reporting behavior belongs in
skills, tools, workflows, metric contracts, and sandbox prompt guidance.

## Runtime Settings

Common environment variables:

- `PORT`: Express port. Defaults to `3100`.
- `LOG_LEVEL`: service log level. One of `debug`, `info`, `warn`, `error`, or
  `silent`. Defaults to `info`.
- `TEAMSBOT_DATABASE_URL`: PostgreSQL state URL. `DATABASE_URL` and
  `POSTGRES_URL` are also honored. Postgres state is used for durable
  thread/conversation-reference state, render-obligation indexes, and
  crash-safe recovery leases.
- `TEAMSBOT_STATE_KEY_PREFIX`: Postgres state namespace. Defaults to
  `centaur-teamsbot`.
- `CENTAUR_API_URL`, `TEAMSBOT_API_KEY`, `CENTAUR_API_KEY`: Centaur session API
  settings. `CENTAUR_API_URL` defaults to `http://127.0.0.1:8080`.
  `TEAMSBOT_API_KEY` is preferred when both API-key env vars are set.
- `CENTAUR_REQUEST_MAX_RETRIES`, `CENTAUR_REQUEST_RETRY_DELAY_MS`: retry policy
  for transient session API failures.
- `TEAMS_BOT_APP_ID`, `TEAMS_BOT_APP_PASSWORD`, `TEAMS_BOT_APP_TENANT_ID`:
  required Bot Framework auth settings.
- `TEAMS_ALLOWED_TEAM_IDS`, `TEAMS_ALLOWED_CHANNEL_IDS`,
  `TEAMS_ALLOWED_TENANT_IDS`: comma-separated allow lists. Empty means the bot
  ignores all Teams messages. Personal chats require an allowed tenant id.
- `TEAMS_REQUIRE_MENTION`: require the bot to be mentioned before activating a
  thread. Defaults to `true`.
- `SESSION_IDLE_TIMEOUT_MS`, `SESSION_MAX_DURATION_MS`: forwarded to api-rs
  execute. `TEAMS_IDLE_TIMEOUT_MS` and `TEAMS_MAX_DURATION_MS` are also
  accepted as Teams-specific overrides.
- `TEAMS_ACTIVE_EXECUTION_TTL_MS`: stale execution timeout. Defaults to 30
  minutes.
- `TEAMS_RENDER_DELIVERY_TIMEOUT_MS`: timeout for Teams message send/update
  calls during rendering. Defaults to 15 seconds.
- `TEAMS_DOWNLOAD_ATTACHMENTS`: download allowed Teams attachment URLs into
  base64 parts before sending to Centaur. Defaults to `false`.
- `TEAMS_ATTACHMENT_MAX_BYTES`, `TEAMS_ATTACHMENT_ALLOWED_HOSTS`: download size
  cap and HTTPS host allow-list.
- `TEAMS_GRAPH_BEARER_TOKEN`, `TEAMS_GRAPH_TOKEN_SCOPE`: optional Graph auth
  fallback for Graph/SharePoint-backed attachment URLs. If no bearer token is
  provided, the service can use the bot app client credentials.
