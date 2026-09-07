# Slack Chat SDK Staging Validation

The candidate upgrades Slack from 4.31.0 to 4.40.0 and replaces multipart task
cards with shorter details on a single card. Record previous and candidate image
digests, thread/message IDs, durable session and execution IDs, and delivery
outcomes. Use dedicated staging conversations and the existing release procedure.
Keep credentials and private message contents out of the report.

## Before Upgrading

Create a Slack conversation on the previous version and record its durable
session ID. Preserve the previous image for rollback.

## Candidate Checks

| Scenario | Expected Outcome |
| --- | --- |
| Reply in the existing conversation | The original durable session continues. One accepted user turn produces one execution and one terminal answer. |
| Long answer with paragraphs, Unicode, code, and a table | Continuation messages contain the complete answer in order. Fences and headers remain readable. Physical splits can insert a newline in code; use a file when byte-exact code is required. |
| Many tasks, long file lists, and large tool output | Task details are shortened; a task does not become numbered multipart cards. Progress finishes and the complete final answer arrives once. |
| Run crossing the configured stream age | A new segment continues the answer. Earlier task cards stop showing work in progress. |
| Mentioned stop or steering during a run | The existing stop/steering behavior works. An unmentioned follow-up does not interrupt execution. |
| Restart after a durable terminal event but before final delivery | Recovery delivers the terminal answer once and clears its obligation. Another restart does not repeat completed delivery. |
| Approved trigger-bot alert and denied bot input | The approved alert activates with context; denied input creates no session or execution. |
| Authorized attachment download | The actual Slack file reaches the durable input with the expected bytes. |
| Plain-text-only request | The answer is delivered without task cards. |

Inspect durable messages, executions, terminal events, render obligations, and
final Slack messages together. Pod health alone does not prove delivery. Tests
cover synthetic API errors; do not induce real rate limiting or broad outages.

## Rollback

Restore the previous Slack image and reply in the same conversation. Confirm
that the session, subscription, and recovery state remain usable. Upgrade again
and confirm continuity. No state schema migration or bulk rekey is expected.

Hold promotion on a missing or duplicate terminal answer, a new durable session
for an existing conversation, failed recovery, or unintended activation. Preserve
a bounded evidence snapshot before rollback.
