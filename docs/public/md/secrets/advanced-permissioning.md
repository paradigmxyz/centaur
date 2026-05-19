---
title: 🚧 Advanced Permissioning
description: Work-in-progress design for per-user and per-channel iron-proxy credential grants.
---

# 🚧 Advanced Permissioning

:::warning[🚧 WIP - feedback wanted]
This is an early design for per-user and per-channel credential grants in
iron-proxy. We want feedback on the primitive, the channel override behavior,
and the provider-specific scope model before this becomes production behavior.
:::

## Iron-Proxy: Per-User Auth

Sketch of how iron-proxy scopes upstream credentials based on either the user or
the channel context.

## Threat model

What we're protecting against:

- The agent leaks data the requesting user shouldn't see
- Users see things they shouldn't via the agent

Right now all keys are shared, so users can see everything the Centaur
deployment has access to regardless of their own permissions. We need to give
agents identity and permissions based on what users have access to, while still
allowing multiplayer use cases like multiple people collaborating on a thread.

## The primitive

A grant binds a principal to a scoped reference to an upstream secret.

```text
grant := (principal, secret_ref, scope, conditions)
```

- `principal`: a user or a channel
- `secret_ref`: pointer to a secret in an upstream vault (iron-proxy never holds it)
- `scope`: normalized capability set for the target provider
- `conditions`: TTL, MFA, etc.

Three tables: `principals`, `secret_refs`, `grants`.

## Resolution

Iron-proxy gets `{requesting_user, channel_id, target}` per request. Control
plane picks one:

```text
if channel_id has grants for target:
    effective = grants(channel_id, target)
else:
    effective = grants(requesting_user, target)
```

The channel wins when configured. Otherwise fall back to the user. This means a
channel is an explicit scope context, e.g. admins configure what the agent can do
in #incident-response regardless of who's asking, and DMs / solo runs use the
user's own grants.

Iron-proxy then receives scoped credentials (Postgres SET ROLE, GitHub token,
etc.) limited to effective and proxies the call.

## Why channel-based auth

Merging user permissions gets complex fast. For example, a permission set like
“administer GitHub org X” and “read repos on org Y” can’t be cleanly
intersected: you need some way to flatten everything into some normalized format
per upstream service.

Channels, meanwhile, are a clear unit of work. They have members who are
gathered in that channel for an express purpose. It’s reasonable to say that
everyone in a given channel should be able to see similar things, so making
channels the unit of authorization gets us most of the security benefit for a
fraction of the complexity.

## What a grant looks like

A single `secret_ref` can back dozens of grants, each restricting which calls
iron-proxy will let pass when that secret is substituted in. No per-user token
minting is required for providers that don't support fine-grained scoping
natively.

Each grant carries an allowlist of request shapes the principal is permitted to
make. Iron-proxy enforces these at the egress layer; the upstream token itself
stays broad.

GitHub (read public repos only, no writes):

```yaml
principal: user:matt
secret_ref: gh_pat_acme_org
scope:
  github:
    - allow: GET /repos/acme/{public-*}/**
    - allow: GET /search/code?q=org:acme+is:public+*
    - deny:  '*'  # everything else
```

Postgres (read-only on specific schemas, would include searching of Slack data):

```yaml
principal: channel:incident-response
secret_ref: pg_prod_readonly
scope:
  postgres:
    - set_role: incident_reader
    - allow_statements: [SELECT, EXPLAIN]
    - allow_schemas: [public, analytics]
```

Internal API (path + method allowlist):

```yaml
principal: user:matt
secret_ref: internal_api_key
scope:
  http:
    - allow: GET  https://api.acme.internal/customers/**
    - allow: POST https://api.acme.internal/customers/*/notes
    - deny:  '*'
```

The shape is always the same: an ordered list of allow/deny rules against the
request's method, path, query, and body. Iron-proxy walks the list; first match
wins; default deny.

For providers that do have native fine-grained tokens (Postgres roles, GitHub
fine-grained PATs, scoped Slack apps), the allowlist is belt-and-suspenders: we
use the native primitive and verify at the proxy. For providers that don't, the
allowlist is the whole mechanism.

## OAuth

OAuth is a special case since it requires active lifecycle management. Refresh
tokens have to be updated before they expire, and tokens can be revoked
upstream. They also typically require a browser-based authentication flow.

This means that the Centaur admin panel needs a UI where users can connect their
own identities per upstream (GitHub, Google, Slack, etc.) and a background
worker to perform token refresh. The grant model is the same, but the infra
around it is more complex than just “store a secret and substitute it in.”

## Examples

Solo / DM. Matt asks the agent to query Postgres in a DM. No channel grants
exist -> effective = grants(matt, postgres) -> SET ROLE matt_readonly.

Configured channel. #incident-response has channel grants for org-wide Slack
search and prod Postgres read. Anyone in the channel gets that scope when the
agent runs, regardless of their individual perms.

Unconfigured channel. Channel exists but no grants registered -> fall back to
the requesting user's grants.

Cross-organization channel. Same flow as above - cross-org channels are no
different than local channels. Note that the agent will need the ability to
query Slack data for the remote org, so each Centaur instance will need to keep
track of both local and remote users.

## Hard parts

Scope normalization. Per-provider adapters flatten capabilities to a normalized
set so grants are comparable and auditable.

Channel grant lifecycle. Channels auto-register on first use with no grants.
Admins configure them out-of-band. The agent runtime can't modify channel
grants.

## Sequencing

Start with user grants only, then move onto channel grants as an override. User
grants will already beat the god-mode service tokens we’re currently using.
