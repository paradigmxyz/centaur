---
title: Slack huddle transcripts
description: Read the verbatim, speaker-attributed transcript of a recorded Slack huddle — the one Slack record no bot token can reach.
---

# Slack huddle transcripts

Slack records every huddle and saves a full, speaker-attributed transcript. **A bot token cannot read
it, and no public API exposes it.** `files.info` on the transcript's file id returns metadata and no
words, whatever scopes the app holds; Slack support confirms this is deliberate.

That leaves a hole in an agent's view of a workspace, and it is the wrong hole. The huddle is where
the standup sets the day's priorities, where the incident call finds the cause, where the design
argument is actually settled. Channels get the summary; the huddle had the reasoning. An agent that
reads every channel and no huddle is missing the part that mattered.

Centaur's Slack tool can read them, given a **user web session**.

## How it works

The Slack *web client* reads transcripts perfectly well. It calls `files.info` with
`include_transcription=true` against the **workspace host** (`<workspace>.slack.com` — never
`slack.com`), authenticated by a user web session: an `xoxc` token paired with the `d` cookie.
The tool replays exactly that call.

This is not a scope escalation and not a permission bypass. A user session sees precisely what that
user could already read by opening the huddle in their own Slack client. If they cannot see the
huddle, neither can this.

```bash
slack huddle-transcript F0123456789
```

```
34 turns, 3 speakers
<@U01ABC> [0:04]: the migration is blocked on the schema review
<@U02DEF> [0:11]: I can review it this afternoon
<@U01ABC> [0:19]: then we ship Friday
```

The agent-facing method is `get_huddle_transcript(file_id)`. It returns `{file_id, speakers, turns,
text}`. Speakers stay as **Slack ids**, not names — an id round-trips to a real mention and cannot
drift, while a name resolved at read time silently rots when someone changes their display name.

Find a `file_id` from the huddle's message in `conversations.history` (the room object references its
transcript file), or from the huddle's AI-notes canvas, which names it.

## The credentials

Two optional secrets. Without them every other Slack method works exactly as before, and only
`get_huddle_transcript` reports what is missing.

| Secret | What it is |
| --- | --- |
| `SLACK_WEB_TOKEN` | The `xoxc` token from a signed-in Slack web session |
| `SLACK_WEB_COOKIE` | The paired `d` cookie (`d=xoxd-…`) |

Both are declared as header-injected HTTP secrets scoped to `*.slack.com`, so **the sandbox holds
placeholders and never the real session** — iron-proxy swaps in the true values at the network
boundary, and only for Slack hosts.

:::warning[Scope these to a service account, not to a person]
A user web session carries **that user's full Slack visibility**, including their DMs. Mint it from an
account whose visibility you are content for agents to inherit. This is the same trust decision as
`SLACK_SEARCH_TOKEN`, but the blast radius is larger, so make it deliberately.
:::

### Obtaining them

From a browser signed in to the workspace:

1. Open the workspace in Slack (web or the desktop app), then open developer tools.
2. `SLACK_WEB_COOKIE` — copy the `d` cookie for `.slack.com`. Its value begins `xoxd-`. Store it in
   the `d=xoxd-…` form.
3. `SLACK_WEB_TOKEN` — the paired `xoxc` token is embedded in the workspace boot page. Find it in any
   authenticated XHR to `/api/…` (the `token` form field), or search the page source for `xoxc-`.

The two are a **pair**: an `xoxc` token only authenticates alongside the `d` cookie it was issued
with. Rotating one without the other yields `invalid_auth`.

## Sessions expire — and the failure is loud on purpose

A web session lapses. When it does, the tool raises `needs_reauth` rather than returning an empty
transcript.

That distinction is the whole design. **A silent empty read looks exactly like "nobody spoke."** An
agent that quietly concludes a meeting was silent is far worse than one that says it cannot see —
the first fabricates an absence of evidence, the second reports a broken credential. So a lapsed
session fails loudly, and a caller can tell "sign in again" apart from "this is broken":

```json
{
  "error": "huddle_transcript_failed",
  "message": "the Slack web session expired — a human must sign in again …",
  "slack_error": "invalid_auth",
  "needs_reauth": true
}
```

Treat `needs_reauth` as an operational alert, not an empty result. Discovery — which huddles exist —
runs on the ordinary bot token and never needs the session, so nothing is lost while a session is
stale: refresh it and the backlog reads fine.
