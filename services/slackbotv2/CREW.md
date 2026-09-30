# Crew identities

Set `SLACKBOTV2_CREW_FILE` to an operator-managed JSON file to serve additional
Slack apps from this Slackbot process. The file is secret and contains an array:

```json
[
  {
    "id": "atlas",
    "appId": "A123",
    "teamId": "T123",
    "botUserId": "U123",
    "botToken": "xoxb-REPLACE",
    "signingSecret": "REPLACE"
  }
]
```

Each `id` must exist in the deployed instruction registry (the existing internal
`persona` type). Each app's event and interactivity URL is
`https://<ingress>/api/webhooks/slack/crew/<id>`. Configure ingress with a prefix
route for `/api/webhooks/slack` to this service. Each route uses its own token,
signature verifier, Chat SDK state prefix, and app-scoped durable session key:
`slack:<team>:<app>:<channel>:<thread-ts>`. The default bot keeps its existing keys.
The API must include the app-scoped Slack destination parser before enabling Crew.

The runtime verifies bot identity and workspace against Slack on startup. A Crew
member is fixed to its instruction ID; inline selectors cannot switch it, and
missing or mismatched instructions prevent execution. Harness/model flags work
as before. User/conversation authorization remains unchanged: Crew does not
grant credentials or introduce a separate permission or memory model.

In Helm, put the array in the `crew.json` key of an existing Secret and set
`slackbotv2.crew.existingSecret`. Restart Slackbot after updating the Secret;
configuration is loaded on startup. Creating/installing Slack apps is an
operator provisioning concern; this service does not hold configuration tokens.

Before rollout, test signed URL verification, a channel mention and a DM for
each member, two members in the same thread, interruption, restart/replay, and
reply identity. Deploy both Slackbot and API changes before registering webhooks.
