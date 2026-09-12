# googlechatbot

Google Chat ingress service for Centaur.

This is shaped after Centaur's Teams/Discord services:

1. Receive Google Chat app events at `/api/webhooks/gchat` (and Workspace Events
   Pub/Sub pushes at `/api/webhooks/gchat/pubsub`).
2. Gate by space/sender policy and mention/subscribed-thread state.
3. Serialize the Google Chat message and attachments into Centaur session
   messages.
4. Call the Centaur session API: create, append, execute, stream events.
5. Render the streamed answer back into the space by editing one progress
   message, persisting render obligations so a restarted process can resume an
   incomplete stream.

Thread keys are the Chat SDK's `gchat:<spaces/ID>[:<base64url thread>][:dm]`, so
every thread in a space maps to one Centaur principal (a DM space maps to the
acting user).

## Run Locally

```bash
pnpm install
pnpm --filter googlechatbot test
pnpm --filter googlechatbot simulate "Reply exactly PONG."
```

The simulator runs against an in-process mock Centaur API, so it needs no Google
credentials.

## Google Cloud Setup

The service does not create the Chat app; configure it once in Google Cloud:

1. Enable the Google Chat API in a Google Cloud project of a Google Workspace
   organization, and configure the Chat app (name, avatar, description).
2. Set **Connection settings** to **HTTP endpoint URL** pointing at this
   service's `/api/webhooks/gchat`.
3. Set **Authentication audience** to either the project number
   (`GOOGLE_CHAT_PROJECT_NUMBER`) or the endpoint URL
   (`GOOGLE_CHAT_ENDPOINT_URL`), and set the matching env var — the adapter
   verifies the Google-signed bearer token against it and rejects anything else.
4. Create a service account, download its JSON key, and pass it as
   `GOOGLE_CHAT_CREDENTIALS`. The app posts, edits, and downloads attachments as
   this identity.
5. Optional: to answer messages that do not @mention the app, create a Pub/Sub
   topic plus a push subscription to `/api/webhooks/gchat/pubsub`, set
   `GOOGLE_CHAT_PUBSUB_TOPIC`, `GOOGLE_CHAT_PUBSUB_AUDIENCE`, and
   `GOOGLE_CHAT_PUBSUB_SERVICE_ACCOUNT_EMAIL`, and grant domain-wide delegation
   to the service account with `GOOGLE_CHAT_IMPERSONATE_USER` set.

## Runtime Settings

- `PORT`: HTTP port. Defaults to `3101`.
- `LOG_LEVEL`: one of `debug`, `info`, `warn`, `error`, `silent`. Defaults to
  `info`.
- `GOOGLECHATBOT_DATABASE_URL`: PostgreSQL state URL. `DATABASE_URL` and
  `POSTGRES_URL` are also honored. Postgres state holds durable thread state,
  space references, render-obligation indexes, and crash-safe recovery leases.
- `GOOGLECHATBOT_STATE_KEY_PREFIX`: Postgres state namespace. Defaults to
  `centaur-googlechatbot`.
- `CENTAUR_API_URL`, `GOOGLECHATBOT_API_KEY`: Centaur session API settings.
  `CENTAUR_API_URL` defaults to `http://127.0.0.1:8080`.
- `CENTAUR_REQUEST_MAX_RETRIES`, `CENTAUR_REQUEST_RETRY_DELAY_MS`: retry policy
  for transient session API failures.
- `GOOGLE_CHAT_CREDENTIALS`: service account JSON. `GOOGLE_CHAT_USE_ADC=true`
  uses Application Default Credentials instead. One of the two is required.
- `GOOGLE_CHAT_PROJECT_NUMBER` / `GOOGLE_CHAT_ENDPOINT_URL`: webhook JWT
  audience. At least one is required; the service refuses to start without one
  rather than accepting unverified requests.
- `GOOGLE_CHAT_WORKSPACE_ADDON_SERVICE_ACCOUNT_EMAIL`: exact add-on identity for
  Workspace Add-on Chat apps, which sign webhooks with
  `service-<projectNumber>@gcp-sa-gsuiteaddons.iam.gserviceaccount.com`.
- `GOOGLE_CHAT_PUBSUB_TOPIC`, `GOOGLE_CHAT_PUBSUB_AUDIENCE`,
  `GOOGLE_CHAT_PUBSUB_SERVICE_ACCOUNT_EMAIL`: Workspace Events delivery. The
  audience and pushing service account are both required when a topic is set.
- `GOOGLE_CHAT_BOT_USER_ID`: `users/<id>` of the app, so it recognizes its own
  messages and mentions in multi-bot spaces.
- `GOOGLE_CHAT_IMPERSONATE_USER`: Workspace user to impersonate for Workspace
  Events subscriptions (domain-wide delegation).
- `GOOGLE_CHAT_USER_NAME`: mention name the Chat SDK matches. Defaults to
  `centaur`.
- `GCHAT_ALLOWED_SPACE_IDS`, `GCHAT_ALLOWED_DOMAINS`,
  `GCHAT_ALLOWED_SENDER_EMAILS`: comma-separated allowlists. Empty means the bot
  ignores all Google Chat messages. Space ids accept `spaces/AAAA` or `AAAA`.
- `GCHAT_ALLOW_DIRECT_MESSAGES`: allow DM spaces. Off by default, and a DM also
  requires a sender allowlist, since a DM has no space to scope it by.
- `GCHAT_REQUIRE_MENTION`: require a mention before activating a thread.
  Defaults to `true`.
- `SESSION_IDLE_TIMEOUT_MS`, `SESSION_MAX_DURATION_MS`: forwarded to api-rs
  execute. `GCHAT_IDLE_TIMEOUT_MS` and `GCHAT_MAX_DURATION_MS` override them.
- `GCHAT_ACTIVE_EXECUTION_TTL_MS`: stale execution timeout. Defaults to 30
  minutes.
- `GCHAT_RENDER_DELIVERY_TIMEOUT_MS`: timeout for Chat message create/update
  calls during rendering. Defaults to 15 seconds.
- `GCHAT_RENDER_MIN_EDIT_INTERVAL_MS`: minimum spacing between progress-message
  edits. Defaults to 1500ms; Google Chat has no streaming surface and rate
  limits `spaces.messages.update`.
- `GCHAT_DOWNLOAD_ATTACHMENTS`: download Chat attachments into base64 parts
  before forwarding them to Centaur. Defaults to `false`.
- `GCHAT_ATTACHMENT_MAX_BYTES`: download size cap. Defaults to 10 MiB.

## Known Gaps

- Outbound file upload is not supported: the Chat SDK adapter drops files on
  `postMessage`, so replies are text (answers longer than Google Chat's 4096
  character limit spill into follow-up messages in the same thread).
- There is no `googlechat` tool CLI yet, so the agent cannot read space history
  or post out-of-band the way it can with `slack`/`discord`.
- The end-to-end path has not been exercised against a live Google Workspace
  Chat app; everything here is covered by unit tests and the mock-API simulator.
