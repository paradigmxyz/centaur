# Console Multi-Repository Workspaces

> Superseded by
> [Feishu And Web Multi-Repository Development Flow](./2026-08-13-feishu-web-multi-repository-development-flow-design.md).
> Keep this document as design history; do not use it as the implementation
> specification for the first release.

## Objective

Allow a Console Chat to work across zero, one, or many repositories from one
trusted self-hosted GitLab instance.

The user can search and select projects when starting a Chat. The selected
projects become part of a durable Workspace owned by that Chat session. Later
turns reuse the same repositories, branches, uncommitted changes, and harness
state without asking the user to select projects again. A user can also start
with an empty Workspace and add projects later.

An agent may request another project while working, but the project is added
only after the user approves the request in Console. Each repository has its
own working tree and branch inside the shared Workspace.

This first release changes the Console Chat ingress only. Slack and Feishu do
not gain project pickers in this change, but the durable Workspace interfaces
must remain platform-neutral so those ingresses can use them later.

## Scope

Included:

- one operator-configured GitLab instance;
- one deployment-level GitLab token shared by Console Chat users;
- GitLab project search with pagination;
- empty, single-repository, and multi-repository Workspaces;
- one session-owned persistent Workspace volume per new Console Chat;
- one independent local branch per repository;
- user-initiated additions and agent-requested additions with approval;
- durable provisioning, retry, and recovery state in Postgres; and
- compatibility with existing sessions and the deployment-level `AGENT_REPO`.

Not included:

- per-user GitLab OAuth or per-user repository visibility;
- arbitrary Git hosts or user-supplied clone URLs;
- repository removal after it has been provisioned;
- Git fetch or pull after initial provisioning;
- pushing branches, opening merge requests, or managing write credentials;
- Slack or Feishu project-selection UI; and
- automatic Workspace retention or deletion.

## Chosen Approach

Centaur will provision selected projects directly into a session-owned
Workspace volume. It will not pre-clone every GitLab project and will not add
runtime selections to the existing deployment-level repo-cache.

The alternatives were:

1. Pre-clone every project into repo-cache. This makes session startup fast but
   consumes storage for unused projects and makes GitLab discovery and cache
   membership the same concern.
2. Mutate the shared repo-cache whenever a user selects a project. This
   improves repeat startup time but introduces mutable cluster-wide cache state,
   synchronization, and eviction before the core Chat workflow is proven.
3. Provision only the projects attached to each Workspace. This has a slower
   first start and cannot use the generic warm pool, but it gives each Chat a
   durable, isolated set of working trees and keeps the Git credential out of
   the agent container.

The third approach is the first-release design. A shared content-addressed
object cache may be added behind the provisioning interface later without
changing the session or ingress contracts.

## Domain Model

### Repository

A Repository is a GitLab project visible to the configured shared token.
`repository_id` is an opaque stable identifier formatted as `gitlab:<project
id>`. Callers must not derive clone URLs or filesystem paths from it.

Project names, `path_with_namespace`, default branch, and HTTP clone URL are
snapshots returned by GitLab when a Repository is resolved. Before storing a
binding, api-rs verifies the project still exists, is not archived, has a
default branch, and returns a clone URL on the configured GitLab origin.

### Workspace

A Workspace is the durable coding environment owned by exactly one session.
It contains the persistent volume reference, repository bindings, generated
manifest, harness state, uploads, and working trees. A sandbox is temporary
compute attached to a Workspace; it does not own Workspace storage.

Every new Console Chat explicitly creates a Workspace, even when the initial
repository list is empty. This prevents an empty Chat from needing a lossy
storage migration when a project is added later.

### Workspace Repository

A Workspace Repository binds one Repository to one Workspace and records:

- the opaque `repository_id`;
- GitLab project ID and path snapshots;
- the sanitized relative working-tree path;
- the GitLab default branch used as the base;
- the deterministic Centaur branch name;
- whether it came from initial selection, direct user addition, or an agent
  request; and
- provisioning status and a sanitized failure reason.

The unique key is `(workspace_id, repository_id)`. A second unique constraint
on `(workspace_id, relative_path)` prevents path collisions. The relative path
is generated as `repos/<gitlab-project-id>-<project-slug>` rather than accepting
a path from GitLab or a caller. This makes it stable across namespace renames
and prevents path traversal or case-folding collisions.

### Repository Addition Request

A Repository Addition Request is a durable approval record. It stores the
Workspace, Repository, requesting execution, rationale, idempotency key,
requesting sandbox/principal identity, reviewer, linked Workspace Repository,
and timestamps.

Its state machine is:

```text
pending -> approved -> provisioning -> ready
       \-> rejected
provisioning -> failed -> provisioning (retry)
```

Approval is not a long-running execution state. The execution that requests a
Repository finishes normally. After approval and provisioning, api-rs starts a
new continuation execution.

## Deep Modules And Seams

### Repository Catalog Module

The Repository Catalog seam lives in api-rs. Its interface exposes only:

- paginated search returning safe project summaries; and
- resolution of opaque Repository IDs into validated project snapshots.

The GitLab adapter owns `/api/v4/projects` requests, pagination, response
parsing, token headers, timeouts, same-origin validation, and error redaction.
Console, Slack, Feishu, and session runtime code never construct GitLab URLs or
handle the GitLab token.

Search results are a short-lived cache, not a source of truth. Session creation
and every addition resolve each selected Repository directly again before a
durable binding is written.

### Workspace Manager Module

The Workspace Manager seam sits between session runtime and storage/runtime
infrastructure. Its small interface prepares a Workspace plan and returns a
mount that can be attached to a sandbox. Its implementation owns:

- creation and lookup of the session-owned persistent volume;
- serialized provisioning of missing Workspace Repositories;
- the short-lived Kubernetes provisioning Job;
- the Job's narrow egress policy and Git credential mount;
- structured, redacted provisioning results; and
- generation of the agent-visible Workspace manifest.

The Agent Kubernetes adapter uses a PVC and Kubernetes Job. A local filesystem
adapter supports focused development and tests. Session runtime does not know
about Kubernetes Jobs, Secret volumes, or PVC naming.

### Console And Tool Adapters

Console remains a thin platform adapter. It renders Repository search and
Workspace state, then calls api-rs for every mutation. It does not become a
second source of Workspace state.

The `centaur-workspace` agent tool is also thin. It can search the Repository
Catalog and request a Repository, but cannot approve requests or provision
storage. It authenticates through the existing sandbox-entitlement seam.
Console forwards the authenticated sandbox and principal identity to api-rs;
api-rs derives the session from the current sandbox assignment instead of
accepting a caller-supplied thread key.

## Durable Data

Add these Postgres tables through new SQLx migrations:

### `session_workspaces`

- `workspace_id` primary key;
- `thread_key` unique foreign key to `sessions`;
- `storage_ref` unique, nullable until storage creation succeeds;
- `status`: `provisioning`, `ready`, or `degraded`;
- `generation`, incremented whenever the Repository set changes; and
- created and updated timestamps.

### `session_repositories`

- `session_repository_id` primary key;
- `workspace_id` foreign key;
- `repository_id` and numeric GitLab project ID;
- project name and `path_with_namespace` snapshots;
- validated HTTP clone URL snapshot;
- default branch, Centaur branch, and relative path;
- `source`: `initial`, `user`, or `agent_request`;
- `status`: `provisioning`, `ready`, or `failed`;
- sanitized failure code and message; and
- created and updated timestamps.

The clone URL is control-plane data. It is never returned by user-facing
Repository or Workspace responses and never written to agent-visible files.

### `session_repository_requests`

- `request_id` primary key;
- `workspace_id` and `repository_id`;
- requester execution, sandbox, and principal identities;
- rationale and idempotency key;
- status and linked `session_repository_id`;
- reviewed-by identity and review timestamps;
- sanitized failure information; and
- created and updated timestamps.

All state transitions use conditional updates in a transaction. Unique
constraints make duplicate create requests, tool retries, button replays, and
provisioning retries idempotent. New tables receive the same read-only and
Console visibility treatment as the existing durable session tables.

## Control-Plane Interfaces

The external shapes are platform-neutral even though Console is their first
caller.

### Repository discovery

```text
GET /api/repositories?provider=gitlab&query=<text>&cursor=<opaque>&limit=<n>
```

The response contains `repository_id`, display name, path with namespace, and
default branch. It does not contain clone URLs or credentials. The cursor is
opaque to callers and is derived from GitLab pagination metadata.

### Session creation

`CreateSessionRequest` gains an optional `workspace` object:

```json
{
  "workspace": {
    "repository_ids": ["gitlab:123", "gitlab:456"]
  }
}
```

An absent `workspace` preserves legacy ingress behavior, including
deployment-level `AGENT_REPO`. `workspace` with an empty list explicitly
creates an empty durable Workspace. Console always sends the object; existing
Slack and other ingress requests remain unchanged.

Creation resolves all selected Repository IDs before the transaction. A retry
must carry the same initial Repository set. A different initial set for an
existing session returns `409`; later additions use the Workspace interfaces.

### Workspace reads and user additions

```text
GET  /api/session/:thread_key/workspace
POST /api/session/:thread_key/workspace/repositories
POST /api/session/:thread_key/workspace/repositories/:session_repository_id/retry
DELETE /api/session/:thread_key/workspace/repositories/:session_repository_id
POST /api/session/:thread_key/workspace/repository-requests/:request_id/approve
POST /api/session/:thread_key/workspace/repository-requests/:request_id/reject
```

The direct Repository-add operation represents an explicit user action and
enters `approved` immediately. Agent-created requests enter `pending`. Only the
Console Chat owner or an administrator may add, approve, reject, retry, or
remove a failed binding in this release. Retry and delete address the durable
Workspace Repository binding, so they work for initial selection, direct user
addition, and approved agent requests. Delete applies only to a binding that
has never reached `ready` and is not currently being provisioned. Removing a
failed initial binding may make the Workspace ready and release its queued
first execution. A linked agent request mirrors its binding's provisioning
state but does not provide a separate retry mechanism.

### Agent request adapter

The sandbox-authenticated Console endpoint accepts Repository search and
request operations for the current sandbox. It forwards the verified sandbox
and principal identities to api-rs. api-rs looks up the active session by
sandbox ID and rejects stale, unassigned, cross-session, or non-Workspace
sandboxes.

The tool response tells the agent that the request was recorded and that the
current turn must finish while it waits for user approval.

## Workspace Layout

The persistent Workspace root is mounted at `/workspace` and exported as
`CENTAUR_WORKSPACE_ROOT`. Persistent harness state remains under
`/workspace/.centaur/state`; the entrypoint uses the explicit Workspace root
instead of treating the state directory itself as the project checkout.

```text
/workspace/
  AGENTS.md
  .centaur/
    workspace.json
    state/
  repos/
    123-frontend/
    456-backend/
```

`workspace.json` contains only Repository IDs, paths, base branches, local
branches, and readiness. It contains no clone URL or credential. It is a
generated convenience view, not a source of truth; api-rs regenerates it from
Postgres before sandbox startup.

The harness starts at `/workspace`, not inside one Repository. The generated
root `AGENTS.md` identifies the available repositories and their branches.
Repository-owned `AGENTS.md` files remain inside each working tree and apply
when the agent works below that tree.

Branch names are deterministic and stored durably, for example
`centaur/ws-<workspace-id-prefix>`. The provisioner creates the same branch
name independently in each Repository. After cloning, it removes the `origin`
remote so the control-plane clone URL is not retained in agent-visible
`.git/config`; fetch, pull, and push are outside this release. It never resets,
cleans, or replaces an existing matching working tree during retry or sandbox
reconstruction.

## Provisioning And Execution Flow

### New Chat

1. Console searches GitLab through the Repository Catalog and maintains a
   local multi-selection. The user may leave it empty.
2. Console creates a new session with an explicit Workspace and selected
   Repository IDs, appends the durable user message, and requests execution in
   the existing create -> append -> execute order.
3. api-rs resolves the selected IDs, creates the Workspace and Repository rows,
   schedules provisioning, and returns without waiting for clone completion.
   This keeps session creation within the existing HTTP timeout.
4. For non-empty pending bindings, Workspace Manager runs one serialized,
   short-lived provisioning Job. The Job mounts the Workspace PVC and Git
   Secret, clones each new Repository into a staging directory, creates its
   branch, and atomically moves the completed tree into place.
5. `execute_session` durably creates the execution in `queued` state but does
   not mark it running while the Workspace is not ready. The Console redirects
   to the Chat immediately and renders provisioning from durable events.
6. api-rs consumes structured per-Repository Job results and persists
   `ready`/`failed` states. It executes no user turn while any initial binding
   is not ready.
7. Once the Workspace is ready, the recovery/dispatch worker claims the same
   queued execution, cold-creates a sandbox with the existing Workspace PVC
   mounted, and starts the user's turn. No ingress retry is required.

An empty Workspace skips Git provisioning but still creates and mounts its
persistent volume before the first execution.

### Later Chat Turns

Console sends no Repository list. api-rs loads the Workspace by thread key,
attaches or reuses its sandbox, and executes against the existing working
trees and branches.

Sandbox drain, harness restart, capability replacement, and process recovery
must reuse the Workspace's `storage_ref`. Stopping a sandbox must not delete a
session-owned Workspace PVC.

### Adding A Repository

1. A user chooses Add Project, or the agent calls `centaur-workspace request`
   with an exact Catalog result and a rationale.
2. A user addition is approved immediately. An agent addition renders a
   durable approval card in the Chat.
3. Approval waits for the current execution to become terminal. It never keeps
   that execution open while waiting.
4. api-rs stops the current sandbox, increments the Workspace generation, and
   provisions only bindings that are not ready.
5. On success, api-rs starts a new sandbox on the same Workspace volume,
   appends a durable system-originated continuation message, and starts a new
   execution telling the harness which Repository became available.
6. Rejection records the reviewer and terminal state and leaves the Workspace
   and current sandbox unchanged.

## Console Experience

The New Chat composer adds a searchable multi-select project control alongside
the existing model and effort controls. Selected projects render as removable
compact rows or chips before submission. An empty selection is valid.

An open Chat shows its Repository list and branch state in the thread panel and
offers an Add Project action. Repository search is server-side and paginated;
Console does not download the entire project catalog into the browser.

Pending agent requests render as approval cards with Approve and Reject
commands. Provisioning, retry, and failure states update in place from durable
state. Duplicate clicks return the current state and do not create another
binding or execution.

## Credentials And Network Policy

Configuration uses a dedicated Workspace GitLab block rather than overloading
repo-cache membership:

```yaml
workspaceRepositories:
  enabled: true
  gitlab:
    baseUrl: http://git.example.internal:82
    gitUsername: oauth2
    existingSecretName: centaur-workspace-gitlab
    secretKey: token
  storage:
    size: 20Gi
    storageClassName: ""
```

The Secret is mounted read-only into api-rs for GitLab project discovery and
into the short-lived provisioning Job for `GIT_ASKPASS`. It is never mounted
into the sandbox agent container, copied into a Workspace, included in a Pod
argument, or returned by an interface.

The shared GitLab credential must be a token whose effective access is limited
to the intended project set and whose scopes permit only project discovery and
repository reads (`read_api` and `read_repository` for GitLab token types that
use those scopes). No write scope is required in this release. api-rs sends it
to the GitLab API through a secret-backed header; Git uses the separately
configured non-secret username and the same token through `GIT_ASKPASS`.

The first deployment target is a literal private IP and custom HTTP port. Helm
derives an exact host CIDR and port for api-rs and provisioning egress. It does
not grant the agent general access to that destination. Configured clone URLs
with a different scheme, host, or port from `baseUrl`, URL credentials, query
strings, or fragments are rejected.

Plain HTTP is an explicit operator choice: the GitLab token and repository
contents are not encrypted on the network. Documentation must recommend HTTPS
when the internal GitLab endpoint supports it.

## Failure And Recovery

- If project search fails, Console keeps the user's prompt and selections and
  offers retry. Starting an empty Workspace remains possible.
- If any selected Repository fails resolution, the session is not created.
- If provisioning fails after session creation, the durable Chat remains, the
  user task remains queued, and the failed binding can be retried or removed
  only while it has never reached `ready`.
- Provisioning writes to a binding-specific staging path. A failed clone never
  replaces or modifies an existing working tree.
- Structured Job output identifies Repository IDs and stable error codes but
  redacts token values and raw authenticated transport errors.
- Partial multi-Repository provisioning records each completed binding. A
  retry skips matching ready trees and continues unresolved bindings.
- A missing ready working tree or identity-marker mismatch marks the Workspace
  degraded and fails closed. Centaur does not silently re-clone and discard
  unpushed work.
- An addition failure leaves all pre-existing working trees usable. Retry uses
  the same request and binding IDs.
- Approval during an active execution remains durably `approved` until a
  recovery worker can provision it after the execution terminates.
- api-rs restart recovery scans non-terminal Workspace and request states and
  resumes the same idempotent operation. It also dispatches executions that
  remained queued only because their Workspace was not ready. Process-local
  tasks are not the source of truth.
- Automatic Workspace deletion is disabled in this release. Operators can
  identify claims by Workspace ID; a later retention design must account for
  unpushed changes before deleting them.

## Authorization

All Console users see the same Repository Catalog because the first release
uses one deployment token. This is deliberate and must be visible in operator
documentation.

The Repository Catalog never accepts a clone URL from a user. api-rs resolves
opaque IDs through GitLab and validates the returned origin before persisting a
binding. Session and approval operations enforce Chat ownership or
administrator access before mutation.

Console is the authenticated human-facing resource seam and enforces Chat
ownership before calling api-rs. The internal api-rs mutation persists the
actor identity supplied by the trusted ingress and rechecks Workspace/session
invariants. A later Slack or Feishu adapter must perform its own platform user
authorization before invoking the same internal operation; no browser calls
api-rs directly.

Agent requests are bound to the sandbox identity supplied by the existing
sandbox-entitlement adapter. api-rs derives the session and Workspace from the
current sandbox assignment. The agent cannot name another thread, approve its
own request, read the GitLab token, or add a project without user approval.

## Compatibility And Rollout

- Existing sessions have no `session_workspaces` row and keep current behavior.
- An absent `workspace` in `CreateSessionRequest` preserves `AGENT_REPO` and
  the current single-repository entrypoint path.
- When Workspace Repositories are enabled, new Console sessions always send a
  Workspace object, including for an empty list, and do not set `AGENT_REPO`.
- Existing Slack, Discord, GitHub, Linear, and Teams clients need no request
  change in this release.
- Workspace sessions skip the generic warm pool because a warm sandbox cannot
  be pre-attached to an unknown session PVC. A later pool may warm only the
  harness layer while attaching storage before process startup.
- The existing sandbox-owned state volume path remains compatible for legacy
  sessions. Workspace PVC ownership and deletion are separate from sandbox
  lifecycle.

Rollout is gated by `workspaceRepositories.enabled` and valid GitLab/storage
configuration. When it is disabled or invalid, the Console project picker is
unavailable and legacy Chat behavior remains unchanged; invalid enabled
configuration fails api-rs startup rather than silently omitting credentials or
network restrictions.

## Verification

Focused automated coverage includes:

- GitLab search pagination, URL encoding, direct ID resolution, archived and
  empty-project rejection, same-origin clone validation, timeout handling, and
  token/error redaction;
- SQL migration, read/write, uniqueness, conditional transitions, concurrent
  approvals, retry idempotency, and restart recovery;
- local Workspace provisioning for empty, single, and multiple Repositories;
- staging and atomic move behavior, deterministic branches, ready-tree
  preservation, partial failure, retry, and identity-marker mismatch;
- Kubernetes PVC ownership, Job Secret mounting, exact IP/port egress,
  structured results, cleanup, and proof that the sandbox container has no
  GitLab credential or clone URL;
- runtime refusal to execute before initial provisioning, Workspace mount reuse
  after sandbox replacement, durable queued-execution dispatch, warm-pool
  bypass, and addition continuation;
- sandbox-authenticated Repository requests, stale sandbox rejection,
  cross-session denial, owner/admin approval, rejection, and duplicate clicks;
- Console empty selection, multi-select search, creation payload, open-Chat
  Repository list, direct addition, approval cards, retry, narrow layout,
  keyboard navigation, and focus behavior; and
- legacy CreateSession requests and `AGENT_REPO` behavior.

End-to-end local proof uses an HTTP GitLab-compatible fixture or test Git server
reachable from the explicit local Kubernetes context. It verifies one empty
Chat, one two-Repository Chat, a second turn reusing both branches, an approved
agent request, sandbox replacement with uncommitted changes intact, and
terminal durable events. A final operator smoke test against the trusted
GitLab instance uses a Kubernetes Secret and must not print or persist its
token.
