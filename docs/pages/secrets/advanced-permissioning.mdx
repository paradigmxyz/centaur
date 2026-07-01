---
title: Per-User Permissions
description: Configure user and channel-specific access to tool credentials with the Centaur Console and centaur-perms.
---

# Per-User Permissions

Centaur routes tool and harness traffic through iron-proxy. The proxy only
injects a credential when the active principal has a grant for that credential
and the outbound request matches the credential's request rules.

Use per-user permissions when different chat users, channels, or conversations
should receive different access to the same Centaur installation. This is the
normal production model for shared workspaces: sandboxes still receive placeholders, while
the Centaur Console decides which real credentials each session can use.

## How Access Is Resolved

Centaur represents every chat execution context as a console principal.
Canonical principal ids are:

| Context | Principal foreign id |
|---------|----------------------|
| Slack user | `slack-user-[<team-id-slug>-]<user-id-slug>` |
| Slack channel | `slack-channel-[<team-id-slug>-]<channel-id-slug>` |
| Discord channel | `discord-channel-<guild-id>-<channel-id>` |
| Teams user | `teams-user-<tenant-id-slug>-<user-id-slug>` |
| Teams conversation | `teams-conversation-<tenant-id-slug>-<conversation-id-slug>` |

Channel grants are shared by everyone in that channel. DMs and one-person runs
normally use the user principal directly. In the Slack rows, brackets mark the
optional team scope; Slack principal ids include the team id when the Slack thread
key carries it, such as `slack-channel-t123-c456`.

Roles group secrets together. A principal's effective access is the union of:

- Secrets granted directly to the principal.
- Secrets granted to every role assigned to the principal.

The standard roles are `infra`, `tools`, and one `tool-<slug>` role per tool.
For example, granting the `tool-github` role to a user lets that user use every
GitHub secret registered for the GitHub tool.

## Prerequisites

Enable the Centaur Console, then set the admin API connection
used by `centaur-perms`:

```bash
export IRON_CONTROL_URL=http://localhost:3000
export IRON_CONTROL_API_KEY=iak_...
export IRON_CONTROL_NAMESPACE=default
```

Point the CLI at the same tool directories the API uses. Explicit
`--tools-dir` values are evaluated before the `TOOL_DIRS` environment variable,
and later directories shadow earlier ones. This matches overlay ordering.

```bash
export TOOL_DIRS="$PWD/tools:$HOME/centaur-overlay/tools"
```

Build and run the operator CLI from `services/api-rs`:

```bash
cd services/api-rs
cargo run -p centaur-perms -- --help
```

## Register Tool Secrets

Granting a tool registers the tool's declared secrets in the Centaur Console, creates
or updates the matching `tool-<slug>` role, and grants that role to the selected
principal.

```bash
cargo run -p centaur-perms -- \
  --tools-dir ../../tools \
  principals grant slack-user-u123 \
  --tool github
```

For 1Password-backed secrets, pass the source policy and vault:

```bash
cargo run -p centaur-perms -- \
  --source-policy onepassword-connect \
  --op-vault Engineering \
  --tools-dir ../../tools \
  principals grant slack-user-u123 \
  --tool github
```

Source policies:

| Policy | Secret source |
|--------|---------------|
| `env` | The Centaur Console resolves from environment variables. |
| `onepassword` | The Centaur Console resolves from a 1Password service account. |
| `onepassword-connect` | The Centaur Console resolves through 1Password Connect. |

## Grant A User

The Centaur Console can grant roles and secrets directly from the UI. Open
**Principals**, choose the user principal, then use **Assigned Roles** to assign
a role or **Direct Grants** to grant one secret. The **Effective Grants** table
shows the union of direct grants and grants inherited from roles.

Use `centaur-perms` when you want to script the same changes.

Grant a whole tool to one Slack user:

```bash
cargo run -p centaur-perms -- \
  principals grant slack-user-u123 \
  --tool github
```

Grant an existing role:

```bash
cargo run -p centaur-perms -- \
  principals grant slack-user-u123 \
  --role tool-github
```

Grant one secret directly by OID:

```bash
cargo run -p centaur-perms -- \
  principals grant slack-user-u123 \
  --secret ssr_...
```

Use `principals show` to verify the user's direct grants, assigned roles, and
effective secrets:

```bash
cargo run -p centaur-perms -- \
  principals show slack-user-u123
```

## Grant A Channel

The UI flow is the same for channel principals. Open **Principals**, choose the
channel principal, then assign roles or grant secrets from the detail page.

Grant the channel principal when everyone in a chat channel should share the
same agent permissions:

```bash
cargo run -p centaur-perms -- \
  principals grant slack-channel-c456 \
  --tool linear \
  --tool github
```

When a session runs in that channel, Centaur uses the channel's grants for
matching tools. This is useful for incident channels, support rooms, and other
shared work contexts where the channel defines the authorization boundary.

Inspect the configured channel:

```bash
cargo run -p centaur-perms -- \
  principals show slack-channel-c456
```

## Revoke Access

In the console, open the principal detail page and revoke direct grants from
**Direct Grants** or remove role assignments from **Assigned Roles**.

Revoke access using the same selector shape used for grants:

```bash
cargo run -p centaur-perms -- \
  principals revoke slack-user-u123 \
  --tool github
```

Revoke one direct secret:

```bash
cargo run -p centaur-perms -- \
  principals revoke slack-user-u123 \
  --secret ssr_...
```

Revoke one grant by grant OID:

```bash
cargo run -p centaur-perms -- \
  principals revoke slack-user-u123 \
  --grant-id grant_...
```

Revoking a role assignment leaves the role and its secrets in place for other
principals. Deleting a secret removes grants that point at it.

## Manage Roles

Roles are useful when several users need the same access package.

```bash
cargo run -p centaur-perms -- roles list --managed
cargo run -p centaur-perms -- roles show tool-github
```

Grant an existing secret to a role:

```bash
cargo run -p centaur-perms -- \
  roles grant tool-support \
  --secret ssr_...
```

Register a tool and grant its declared secrets to a role:

```bash
cargo run -p centaur-perms -- \
  --tools-dir ../../tools \
  roles grant tool-support \
  --tool github
```

Then assign the role to users or channels:

```bash
cargo run -p centaur-perms -- \
  principals grant slack-channel-c456 \
  --role tool-support
```

## OAuth Credentials

OAuth credentials created through the console become broker credentials. The
consent flow also creates a grantable static secret that references the broker
credential with a `token_broker` source. Grant that static secret to a user,
channel, or role like any other secret.

See [OAuth Apps](/secrets/oauth-apps) for the app setup and consent flow.
