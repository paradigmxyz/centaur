# Slack reply rendering

The Slack ingress owns platform formatting. Harness and durable session text
remain Markdown so other clients can render the same answer appropriately.

Live and recovered streams send `markdown_text` chunks, which Slack renders
natively. Keep Markdown links intact on those paths, including across chunk
boundaries.

When a stream fails or the canonical answer replaces a diverging draft,
`renderFallbackFinalAnswer` delivers a regular Slack text message. This field
uses `mrkdwn`, so `slackAnswerLinks` converts parsed `[label](url)` spans into
`<url|label>` before truncation and delivery. Existing Slack links, mentions,
bare URLs, inline code and fenced code remain unchanged. The conversion keeps
the fallback's existing 35,000-character bound instead of subjecting long
answers to the native Markdown field's 12,000-character limit.

Requests that suppress interactive blocks use the same link conversion.
Explicit "plain text only" requests preserve literal output.

Validation:

```sh
pnpm --filter slackbotv2 run check:types
pnpm --filter slackbotv2 test
```

The signed-webhook emulator cases cover failed-stream fallback and in-place
replacement of a diverging answer. Check the actual outgoing message and its
link target when verifying a deployment. A Markdown link in the durable answer
alone does not prove that the Slack reply renders it correctly.
