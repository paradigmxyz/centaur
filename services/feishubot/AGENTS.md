# Feishubot Guide

## Role

Feishubot is the China Feishu transport and renderer for the durable development
session API. It uses the shared official Node SDK with explicit
`Domain.Feishu` and `AppType.SelfBuild`; package naming must never select the
international Lark endpoint by accident.

Keep this service thin. It verifies and normalizes Feishu events, forwards
opaque identifiers and user content to `api-rs`, and renders durable state back
to the originating direct message or group topic. Session, Workspace,
ChangeSet, publication, and delivery recovery state belong in `api-rs`.

## Invariants

- Accept only configured China Feishu tenants and ignore bot/self messages.
- Acknowledge SDK callbacks before slow GitLab, sandbox, or rendering work.
- Group tasks require a bot mention and bind to `chat_id + root_id/message_id`;
  direct messages bind to the sender `open_id` and the active server-side
  generation.
- Use event ID and message ID as durable idempotency keys. Never keep channel
  bindings, selected repositories, render cursors, or approval authority only
  in process memory.
- Card actions carry opaque IDs and expected versions. The API rechecks
  initiator/admin authority; the card payload is not authority.
- Never read GitLab configuration or credentials. Do not log App Secret,
  authorization headers, raw card payloads, private file keys, or file content.
- Bound text, attachment metadata, card fallback text, card element count, and
  HTTP/SDK calls. Do not render raw upstream error bodies.

## Validation

From the repository root:

```bash
pnpm --filter feishubot run check:types
pnpm --filter feishubot test
```

Long-connection release proof must use a China Feishu enterprise self-built
application created at `open.feishu.cn`, not an international Lark app.
