# Feishu And Web Multi-Repository Development Flow

**Status:** Approved design

## Objective

Extend Centaur from a Slack-oriented coding assistant into a durable
development product that can accept a task from China Feishu or Web Chat, bind
that task to zero, one, or many repositories from one self-hosted GitLab
instance, run Codex in an isolated persistent Workspace, present an exact
reviewable ChangeSet, and publish approved branches and merge requests.

The first release remains a **single-Agent, multi-repository** product. One
Execution may modify several repositories, but concurrent specialist Agents do
not share a working tree. The architecture leaves room for Assistants,
connections to operational systems, and multi-Agent delegation later.

## Product Decisions

The decisions below are fixed for the first release:

- The complete new workflow is available in China Feishu and Web Chat. Slack
  keeps its current behavior and can adopt the same control-plane interfaces
  later.
- Feishu uses an enterprise self-built application with bot capability, not a
  one-way custom group webhook and not the international Lark platform.
- `services/feishubot/` is a new, thin ingress and renderer.
- In Feishu, the user sends the task first and selects projects second. The
  original message and queued Execution are durable before selection, but no
  sandbox starts until selection and provisioning succeed.
- A direct-message Session stays active until the user sends `/new` or clicks
  **New task**. A group root topic maps to one Session.
- The project picker can search and page through every project visible to one
  deployment-level GitLab token. It supports cross-page multi-select,
  confirmation, cancellation, and **No project**.
- One read/write GitLab token is used for discovery, clone, push, and merge
  request creation. It is never exposed to Feishu, Web Chat, the browser, the
  sandbox, or the persistent Workspace.
- Codex makes ordinary local Git commits. Centaur records their exact SHAs and
  review artifacts; it does not manufacture hidden snapshot commits with a
  temporary Git index.
- Publishing is a separate privileged operation. The task initiator or a
  Centaur administrator clicks **Create MR** before any push occurs.
- Multi-repository publishing is a batch of independent items. Successful
  merge requests are preserved and retries process failed items only.
- Console includes China Feishu OAuth login. The durable identity key is
  `(tenant_key, union_id)`; `open_id` remains the app-scoped delivery identity.

## Scope

### Included

- China Feishu bot direct messages and group `@Centaur` messages;
- Feishu project-selection, progress, result, failure, and approval cards;
- Web Chat creation with zero, one, or many GitLab projects;
- durable Session, Workspace, repository, execution, review, and publication
  state in Postgres;
- one persistent Workspace per Session and one working tree per repository;
- user-initiated, append-only project additions after Session creation;
- GitLab search with pagination through the token's full visible project set;
- exact base/head commit capture, per-repository diff, and test evidence;
- branch push and one GitLab merge request per changed repository after
  approval;
- retry-safe partial publication;
- Feishu OAuth login and identity linking for Console; and
- audit records for repository binding, execution, approval, push, and merge
  request creation.

### Excluded

- multiple visible Feishu bot identities such as `@工程助手` and `@研究助手`;
- concurrent multi-Agent execution inside one Workspace;
- arbitrary user-supplied Git URLs or multiple GitLab origins;
- per-user GitLab OAuth, permissions, or tokens;
- automatic project addition requested by an Agent;
- automatic push, merge request creation, merge, or deployment;
- repository removal, fetch, pull, or rebase after a repository is provisioned;
- Slack project selection, ChangeSet review, and publication in this release;
- production-operation connectors, knowledge bases, visual workflows, and
  Assistant publishing; and
- automatic Workspace retention or deletion policy.

## Existing Baseline

Centaur already has the durable core required by this design:

1. An ingress normalizes a platform message and calls the Session API.
2. `api-rs` persists messages, serializes Executions, assigns sandboxes, and
   stores replayable events.
3. `harness-server` starts Codex App Server in the sandbox and normalizes its
   event stream.
4. Tool traffic uses `iron-proxy` so agent credentials do not have to exist in
   the sandbox.
5. The ingress renders durable events back to the originating platform.

Slack currently supplies the conversational entry point, thread continuity,
and status/result rendering. It does not provide the project picker,
server-owned ChangeSet, approval, or Publisher described here. Today Codex and
the existing `git-branch` helper can branch, commit, push, and create a PR when
explicitly authorized. This design moves the final GitLab write into a durable,
auditable control-plane operation while leaving code judgment with Codex.

The earlier
[Console Multi-Repository Workspaces](./2026-08-12-console-multi-repository-workspaces-design.md)
document explored Web-only repository selection. This document supersedes it
for implementation.

## Architecture

```text
China Feishu                      Web browser
     |                                 |
     v                                 v
services/feishubot                services/console
  verify + normalize               login + render
  cards + delivery                 project picker
     |                                 |
     +---------------+-----------------+
                     v
                services/api-rs
        sessions / executions / authorization
        repository catalog / workspace manager
        changesets / publication coordinator
             |              |              |
             v              v              v
       Postgres       Provisioner Job    Publisher Job
                          |                   |
                          v                   v
                 persistent Workspace   GitLab API/Git
                          |
                          v
                 sandbox + harness-server
                          |
                          v
                         Codex
```

The platform services own transport details only. All durable lifecycle and
authorization decisions remain in `api-rs`.

### Deep Modules

#### Feishu Adapter

`services/feishubot/` owns China Feishu event transport, signature or connection
validation, event normalization, mention removal, command parsing, card
rendering, and delivery retries. It sends opaque platform keys and normalized
content to `api-rs`; it does not own Session state, GitLab credentials, or
publication policy.

#### Repository Catalog

The Repository Catalog in `api-rs` exposes paginated search and resolution of
opaque Repository IDs. Its GitLab adapter owns `/api/v4/projects` requests,
pagination, timeouts, token headers, response parsing, exact-origin validation,
and error redaction. Clients never construct clone URLs.

#### Workspace Manager

The Workspace Manager turns durable repository bindings into a mountable
Workspace. The Kubernetes adapter owns the persistent volume and short-lived
Provisioner Jobs; a local adapter supports focused tests. The Session runtime
sees only preparation state and a mount, not Kubernetes secrets or jobs.

#### ChangeSet Collector

The collector reads repository state after a successful Execution. It verifies
the working tree contract, records existing commit SHAs, produces bounded diff
artifacts, and persists test evidence. It never stages, commits, pushes, or
rewrites the repository.

#### Publisher

The Publisher accepts one authorized immutable ChangeSet and performs only the
Git operations required to push the recorded commits and create or find the
corresponding merge requests. It cannot read arbitrary sandbox files and does
not run agent-authored code.

## Domain Model

The canonical short definitions live in [`CONTEXT.md`](../../../CONTEXT.md).

### Channel Binding

A Channel Binding maps one transport conversation to a durable Session.

- Feishu group key: `(tenant_key, chat_id, root_message_id)`. For the first
  message, `root_message_id` is its own `message_id`; replies use `root_id`.
- Feishu direct-message key: `(tenant_key, open_id, active_session_generation)`.
- Web Chat key: the Console thread identifier.

The binding also records the initiating Centaur user when one can be resolved.
Platform IDs are never treated as cross-platform identities.

### Session And Execution

A Session owns durable conversation history and exactly one Workspace. Each
user turn creates at most one Execution using a platform event idempotency key.
Executions remain serialized by the existing Session API.

The first Feishu Execution is created in `queued` state as soon as the task
message is durably accepted. It carries the blocker `awaiting_project_selection`
and cannot acquire a sandbox. Project confirmation changes the blocker to
`workspace_provisioning`; successful provisioning releases that same Execution
for normal dispatch.

### Repository

A Repository is a GitLab project visible to the configured shared token.
`repository_id` is opaque and formatted as `gitlab:<numeric-project-id>`.
Display name, namespace path, default branch, and clone URL are provider
snapshots, not caller-controlled authority.

Before binding a Repository, `api-rs` resolves it again and verifies that it:

- still exists and is visible;
- is not archived;
- has a default branch; and
- returns a clone URL whose scheme, host, and port exactly match the configured
  GitLab origin.

### Workspace

A Workspace is persistent development storage owned by exactly one Session. It
exists even when the user selects **No project**, so later turns can retain
uploads, harness state, and task artifacts and the user can add a project when
the conversation becomes a coding task.

Repositories live under deterministic paths:

```text
/workspace/
  AGENTS.md
  workspace.json
  repos/
    <gitlab-project-id>-<sanitized-project-slug>/
```

The numeric project ID prevents namespace renames and case-folding collisions.
No platform or GitLab path is used directly as a filesystem path.

### Workspace Repository

A Workspace Repository binds a Repository to a Workspace and records:

- Repository and provider project identifiers;
- display and namespace snapshots;
- deterministic relative working path;
- base branch and exact base SHA;
- local branch name and current head SHA;
- selection order and initiator;
- provisioning attempt and state; and
- a sanitized failure category and message.

The unique keys are `(workspace_id, repository_id)` and
`(workspace_id, relative_path)`. Membership is append-only: confirmation fixes
the initial set, and a later explicit user action may add repositories while no
Execution or publication is active. Repositories cannot be removed or replaced
in the first release.

### ChangeSet

A ChangeSet is the immutable review record created after one successful
Execution. Each changed repository entry contains:

- `base_sha` and `head_sha` from ordinary local commits;
- commit subjects and authorship metadata;
- changed-file counts and bounded diff statistics;
- a content-addressed patch artifact or durable artifact reference;
- patch hash;
- test commands, exit status, duration, and bounded output reference; and
- collector timestamp and status.

The ChangeSet is publishable only while each repository still has the recorded
`head_sha` checked out and a clean working tree. Later user messages may make it
stale; they cannot silently alter what an approval publishes.

### Publish Batch And Item

A Publish Batch records one approval of one ChangeSet. It has one Publish Item
for every changed repository. Each item records the branch, exact head SHA,
target branch, push result, merge request identity/URL, attempt count, and
sanitized failure.

Batch states are `pending`, `running`, `succeeded`, `partially_succeeded`, and
`failed`. Item states are `pending`, `pushing`, `pushed`, `creating_mr`,
`succeeded`, and `failed`.

## Identity And Authorization

### Feishu Runtime Identity

China Feishu messages and card callbacks carry app-scoped user and tenant
identifiers. The ingress stores `(tenant_key, open_id)` for delivery and uses
that pair for runtime authorization lookups. It records `union_id` when the
application has permission to receive it, but does not assume an `open_id` is
portable to another Feishu application.

Every accepted Feishu sender maps to a provider-scoped Centaur Principal using
`(tenant_key, open_id)`. This does not require the sender to have logged into
Console. Console OAuth can later associate that Principal with a Console User,
but it is not a prerequisite for approving from the originating Feishu card.

Only the following principals may approve publication:

- the persisted Principal that initiated the task; or
- a Centaur administrator.

A group member who can see the approval card is not thereby authorized.
`api-rs` makes the decision using the authenticated callback principal; hiding
or disabling a button is only a user-interface hint.

### Console Feishu OAuth

Console adds a `feishu` OAuth provider against the China Feishu authorization
and token endpoints. This is referred to as OAuth in this design, not OIDC.

The linked subject is `(tenant_key, union_id)`. The application is restricted
to configured tenant keys. Because the current Console user model requires an
email, first-release login also requires the Feishu user-info response to
contain a non-empty enterprise email; login fails closed when it is absent.
Centaur treats that email as provider-asserted only inside the configured
tenant, applies the existing email-domain rules, and never grants administrator
status automatically.

The identity link caches `(tenant_key, open_id)` for bot-to-Console continuity.
Account linking requires either an already authenticated Console user or an
exact allowed email match under existing account-linking policy. A matching
display name is never sufficient.

## Feishu Experience

### Transport

The bot uses the official shared Feishu/Lark Node SDK package,
`@larksuiteoapi/node-sdk`, with explicit China settings. The package name does
not select the international platform; `Domain.Feishu` does:

```ts
const clientConfig = {
  appId,
  appSecret,
  appType: Lark.AppType.SelfBuild,
  domain: Lark.Domain.Feishu,
};
```

The first implementation uses `WSClient` long connection for
`im.message.receive_v1` and supported card-action callbacks. The release test
must prove both message and button events against a China Feishu self-built
application. If a target tenant cannot deliver card callbacks over long
connection, only the Feishu transport adapter switches card actions to the
official encrypted webhook mode; the normalized callback and `api-rs`
contracts do not change.

Inbound Feishu event IDs and message IDs are deduplicated before creating a
durable message. Processing acknowledges within Feishu's deadline and performs
slow work asynchronously. Redeliveries replay or refresh the existing durable
result rather than creating a second Session or Execution.

### Direct Messages

The lifecycle is:

1. The first ordinary message creates a Session, Workspace, durable user
   message, and blocked first Execution.
2. The bot opens the project-selection card.
3. Selection and provisioning release the same Execution.
4. Later ordinary messages continue the active Session and reuse its Workspace.
5. `/projects` or **Add projects** opens an append-only picker when the Session
   is idle. Newly selected projects are available to later Executions.
6. `/new` or **New task** closes the active Channel Binding and creates the
   next Session generation. It does not delete the previous Workspace.

If no active binding exists, `/new` creates a blank draft and prompts for the
task. If an Execution is running, **New task** requires confirmation so a user
does not abandon work accidentally.

### Group Topics

The first group message that mentions the bot creates a Session keyed by its
root message ID. Replies in that topic continue the Session; another root
mention creates another Session. The bot replies in the same topic and does not
route unrelated group messages into the Session.

### Project Selection Card

The selection card contains:

- the task excerpt and current Session state;
- recent projects;
- keyword search;
- previous/next pagination;
- multi-select with selections retained across pages;
- selected-project count and removable selected items;
- **Confirm**, **No project**, and **Cancel** actions.

Search results expose only safe project summaries: opaque Repository ID,
display name, namespace, description excerpt, default branch, archive state,
and last activity. Clone URLs and credentials are never included.

Card state is durable and versioned. Every action includes an opaque selection
flow ID and expected version. Stale, replayed, and unauthorized actions return
the current card instead of applying a second mutation. **Confirm** is disabled
after acceptance. A later **Add projects** flow can append projects but cannot
remove or replace existing bindings.

### Progress And Result Cards

One message card is updated through these stages:

```text
Waiting for projects -> Preparing workspace -> Running
-> Tests complete -> Changes ready -> Publishing -> Complete
```

The changes-ready view contains the summary, changed repositories, commit
SHAs, test results, and a signed short-lived Console diff link. Authorized
users see **Create MR**. A partial publication card lists successful merge
requests and failed repositories separately and offers **Retry failed**.

No secrets, clone credentials, raw exception bodies, or unrestricted agent log
output are rendered into Feishu.

## Web Chat Experience

**New chat** presents a task composer and GitLab project picker on the same
screen. The user may search, page, select multiple projects, remove selections,
or choose **No project**, then submit once. The Session, Workspace, first user
message, repository bindings, and first Execution are created atomically from
the user's perspective.

After creation, the selected repositories are shown in the thread header.
Later messages reuse them without another selection step. An explicit **Add
projects** action can append repositories while the Session is idle; existing
repositories cannot be removed or replaced. The thread renders the same durable
execution, ChangeSet, and Publish Batch state as Feishu. The **Create MR**
control calls the same authorization and publication interface.

Console is also the detailed review surface. Its diff page is scoped to a
ChangeSet, not the mutable current Workspace. The link sent to Feishu requires
Console authentication and checks Session access server-side.

## Control-Plane Interfaces

Exact routes may follow existing `api-rs` naming conventions, but the logical
contracts are:

```text
RepositoryCatalog.search(query, cursor, principal)
RepositoryCatalog.resolve(repository_ids, principal)

SessionTask.accept(channel_key, platform_event_id, message, initiator)
SessionTask.select_repositories(selection_flow_id, version, repository_ids)
SessionTask.select_no_repository(selection_flow_id, version)
SessionTask.cancel_selection(selection_flow_id, version)
SessionTask.add_repositories(session_id, selection_flow_id, version, repository_ids)

WorkspaceManager.prepare(workspace_id)
WorkspaceManager.get_state(workspace_id)

ChangeSetCollector.collect(execution_id)
ChangeSetReview.get(changeset_id, principal)

Publication.approve(changeset_id, principal, idempotency_key)
Publication.retry_failed(publish_batch_id, principal, idempotency_key)
Publication.get(publish_batch_id, principal)
```

Ingresses pass authenticated platform evidence and opaque keys. They do not
pass trusted user roles, clone URLs, Workspace paths, target SHAs, or branch
names.

## End-To-End Flow

### Feishu: Task First, Projects Second

1. `feishubot` verifies and deduplicates the event, normalizes mentions, and
   resolves or creates the Channel Binding.
2. `api-rs` transactionally creates the Session and Workspace if needed,
   appends the user message, and creates the first queued Execution with the
   `awaiting_project_selection` blocker.
3. `feishubot` renders the selection card. No sandbox exists yet.
4. Search calls use Repository Catalog pagination against GitLab.
5. **Confirm** re-resolves every opaque Repository ID and transactionally
   creates the initial Workspace Repository set. **No project** creates an
   empty confirmed set. Later additions use the same resolution and
   provisioning path, are append-only, and never restart an earlier Execution.
6. `api-rs` moves the blocker to `workspace_provisioning` and asks Workspace
   Manager to prepare the Workspace.
7. A Provisioner Job clones each selected project into its deterministic path,
   resolves the exact default-branch SHA, creates the local working branch,
   removes transient credential helpers, writes `workspace.json`, and reports
   structured results.
8. When every binding is ready, recovery releases the existing first Execution
   for ordinary serialized sandbox dispatch.
9. The sandbox mounts the persistent Workspace, and `harness-server` starts
   Codex App Server with the generated Workspace instructions.
10. Durable events update the Feishu progress card. Restarts replay state rather
    than restarting completed Git or agent side effects.

### Codex Completion Contract

For an Execution to produce a publishable ChangeSet, Codex must:

- inspect the Workspace manifest and repository-specific `AGENTS.md` files;
- make only task-relevant edits;
- run appropriate tests in every affected repository;
- create ordinary local commits on each changed repository's Centaur branch;
- append commits without amending, resetting, or otherwise rewriting previously
  recorded Workspace history;
- leave no staged, unstaged, or untracked changes; and
- report changed repositories, commit subjects, and test evidence in structured
  completion metadata.

An Execution may still complete conversationally without a ChangeSet when no
files changed. If files changed but the working tree is dirty or commits are
missing, the collector marks the result `needs_agent_completion`; **Create MR**
is unavailable. Centaur may start a bounded continuation Execution asking
Codex to finish the commit contract, but Centaur itself does not decide which
files belong in the commit.

### ChangeSet Collection

For every Workspace Repository, the collector:

1. reads the recorded base SHA and current branch/head;
2. verifies the head descends from the recorded base;
3. verifies the working tree is clean;
4. identifies whether `base_sha..head_sha` contains changes;
5. renders bounded diff metadata and a content-addressed review artifact;
6. associates structured test evidence from the Execution; and
7. writes the ChangeSet and all repository entries in one durable transaction.

The collector has no GitLab credential. Reading committed Git objects is
sufficient because publication is allowed only for the recorded head SHA.

### Publication

When an authorized user clicks **Create MR**:

1. `api-rs` validates the principal, ChangeSet state, Session access, and a
   single-use idempotency key.
2. It acquires a publication lease for the Workspace and revalidates that every
   item has a clean working tree and the exact recorded head SHA.
3. It creates the Publish Batch and pending Publish Items before starting
   external side effects.
4. The Publisher processes items independently. For each repository it pushes
   the exact recorded head SHA to
   `refs/heads/centaur/<workspace-short-id>/<changeset-short-id>` using an
   explicit refspec.
5. It searches for an existing merge request with the same project, source
   branch, target branch, and ChangeSet marker before creating one.
6. It records the external branch and merge request identity immediately after
   each successful side effect.
7. The batch becomes `succeeded`, `partially_succeeded`, or `failed` from its
   item states, and the originating channel receives the durable result.

The Publisher never runs hooks, tests, builds, or repository code. It uses
non-interactive Git, disables hooks, pins the expected GitLab origin, and limits
network egress to that origin. It may read only the Git object database and
minimal repository metadata needed to push the recorded commit.

The lease blocks new Executions until the attempt reaches a durable terminal
state, then releases. **Retry failed** creates new attempts for failed items
only. Before retrying, the Publisher repeats remote branch and merge request
lookup so a crash after an external success cannot create a duplicate. A new
Publish Batch cannot be approved from a ChangeSet after the Workspace advances.
An already approved partial batch remains retryable: it publishes only its
original recorded SHA, never later commits, and requires that Git object still
exists and matches the stored metadata.

## Durable Data

The exact schema is an implementation-plan concern. These durable facts must be
represented, with foreign keys and state-transition constraints where
possible.

### Conversation And Selection

- Channel Binding: platform, tenant/app identity, conversation/root identity,
  active Session, generation, initiator, and timestamps.
- Selection flow: Session, version, query/cursor state if needed for recovery,
  selected Repository IDs, state, and deciding principal.
- Feishu delivery: event/message idempotency keys, rendered message/card ID,
  last durable event cursor, last rendered state version, and delivery error.

### Workspace

- Session Workspace: Session, storage reference, state, generation, and
  preparation attempt.
- Workspace Repository: the fields in the domain model plus uniqueness,
  append-only membership, and the selection flow that added it.

### Review And Publication

- ChangeSet: Session, Execution, collector status, summary, initiator, and
  immutable creation metadata.
- ChangeSet repository: Workspace Repository, base/head SHAs, patch hash,
  artifact reference, diff statistics, and test evidence.
- Publish Batch: ChangeSet, approver, idempotency key, aggregate state, and
  timestamps.
- Publish Item: repository entry, source/target branches, exact SHA, GitLab
  branch/MR identity, state, attempt count, and redacted failure.

### Console Identity

The existing identity store gains provider `feishu` with a structured subject
containing `tenant_key` and `union_id`. Feishu app-scoped `open_id` is stored as
a provider attribute for delivery correlation, not as the cross-app subject.
Slack-specific `team_id` fields must not be repurposed for Feishu tenants.

## Idempotency, Concurrency, And Recovery

The design uses durable state as the recovery source, not process memory.

- Feishu platform event ID uniquely identifies inbound receipt.
- Platform message ID plus Session uniquely identifies the durable user message
  and first Execution.
- Selection flow ID plus expected version serializes card mutations.
- Workspace preparation holds a Workspace-level lease; repository bindings use
  unique constraints and attempt records.
- Session execution serialization remains the one-Agent concurrency boundary.
- One publishable/current ChangeSet exists per completed code-producing
  Execution.
- Publication approval uses a unique `(changeset_id, idempotency_key)` and one
  active batch per ChangeSet.
- A Workspace publication lease prevents new Executions from changing
  repository state for the duration of each publication attempt. It is
  recoverable after process death.
- External push and merge request operations are reconciled by deterministic
  branch names and remote lookup before retry.
- Feishu rendering persists its consumed durable event cursor and desired card
  version. A renderer restart resumes from durable state.

Recovery workers handle stuck queued Executions, Workspace preparation leases,
ChangeSet collection, Publish Items, and delivery obligations. A process
restart must not require a user to resend the task or click approval again.

## Failure Semantics

### Selection And Provisioning

- GitLab search failure keeps the selection flow open and offers retry.
- A project that disappears or becomes invalid before confirmation is removed
  from the candidate selection with an explicit message.
- Provisioning is all-or-blocked for first execution: any failed repository
  keeps the Execution out of the sandbox.
- Retry skips repositories already verified ready and retries failed bindings.
- **Cancel** before confirmation ends the draft Session without a sandbox. It
  does not erase audit records.

### Execution And Collection

- Sandbox or harness failure uses the existing durable Execution recovery path.
- A failed test can still be recorded in a ChangeSet, but the result must make
  the failure prominent. Publication policy may block it if deployment policy
  requires passing tests.
- Dirty trees, missing commits, non-descendant heads, or missing Git objects
  make the ChangeSet non-publishable.
- Diff rendering failure does not lose the commits. It creates a collector
  failure that can be retried before review.

### Publication

- Authorization or stale-head failure occurs before any push.
- One repository failure does not roll back successful remote pushes or merge
  requests in other repositories.
- `partially_succeeded` is a terminal view of the current attempt but remains
  eligible for **Retry failed**.
- Existing matching remote branches and merge requests are adopted after
  verification; incompatible branch contents fail closed.
- Credential, authorization, and raw GitLab response details are logged only in
  redacted structured form and are never shown to the user.

## Security Model

### Shared GitLab Authority

One deployment-level token intentionally defines the visible project universe
for all first-release users. Therefore every authenticated Feishu/Web user who
may create a coding Session can discover and select every project that token can
read. This is an accepted product constraint, not per-user authorization.

The token should belong to a dedicated GitLab bot identity, not a personal or
administrator account. Its GitLab membership should be limited to the projects
Centaur is intended to expose, even though Centaur itself applies no project
allowlist. It needs only the API and repository write permissions required for
search, clone, push, and merge request creation; it must not be allowed to
merge, administer groups, manage users, or deploy.

### Credential Isolation

The shared token is available only through:

- the Repository Catalog credential path in `api-rs`;
- short-lived Provisioner Jobs; and
- short-lived Publisher Jobs.

It is not mounted or injected into `services/feishubot`, `services/console`, a
browser, `harness-server`, the sandbox, `iron-proxy` agent tools, or the
persistent Workspace. Provisioner and Publisher use separate process roles and
filesystem views even though they consume the same secret value.

Credential delivery uses a secret file or brokered request, never command-line
arguments, clone URL userinfo, environment logging, Git configuration persisted
in the Workspace, or remote URLs containing the token. Temporary helpers and
askpass files are removed before the job exits. Logs and errors redact headers,
URLs, and provider bodies.

### Network And Repository Validation

The configured internal GitLab HTTP origin is an accepted deployment risk for
the first release. The adapter accepts only that exact scheme, host, and port;
it rejects redirects or clone URLs to another origin. Egress policy limits
Catalog, Provisioner, and Publisher traffic to GitLab. Sandbox workloads do not
inherit this Git credential path.

### Human Authorization And Data

Feishu event verification, tenant allowlisting, Console authentication,
Session access, and publication approval are independent checks. All mutation
endpoints require CSRF protection where browser cookies are used. Signed links
are short lived and still require an authenticated authorized principal.

Diffs and test output may contain proprietary code or accidental secrets.
Artifacts are encrypted by the deployment's storage mechanism, access-checked,
bounded, excluded from routine logs, and retained no longer than the associated
Workspace/review policy.

## Deployment Configuration

New examples remain deployment-neutral. A representative configuration shape
is:

```yaml
workspaceRepositories:
  enabled: true
  gitlab:
    baseUrl: http://git.example.internal:82
    secretRef: centaur-gitlab
    pageSize: 20
  storage:
    className: example-storage-class
    size: 20Gi

gitlabPublishing:
  enabled: true
  secretRef: centaur-gitlab
  branchPrefix: centaur

feishubot:
  enabled: true
  secretRef: centaur-feishu
  allowedTenantKeys: []

console:
  feishuLogin:
    enabled: true
    secretRef: centaur-feishu-oauth
    allowedTenantKeys: []
```

Separate feature gates cover repository discovery, Workspace provisioning,
Feishu ingress, Feishu login, ChangeSet review, and GitLab publication. An
operator can disable publication without disabling read-only review or ongoing
Sessions.

## Observability And Audit

Metrics and structured logs use bounded identifiers, never task text, source
code, token values, or full provider responses. Required signals include:

- Feishu event verification, deduplication, acknowledgement, and render latency;
- Repository Catalog latency, result count, pagination, and failure category;
- Workspace preparation duration and per-repository result;
- time queued for selection, provisioning, sandbox, and publication;
- ChangeSet count, collector failures, changed repository count, and diff size
  buckets;
- publication item attempts, partial-success count, reconciliation outcome, and
  GitLab latency; and
- card delivery/update retries and stale-action count.

Audit events record initiator, Channel Binding, Session, Repository selections,
exact commit SHAs, approver, publication attempts, remote branch, and merge
request identity. Audit records contain Centaur user IDs and provider IDs needed
for accountability, but no credentials or raw diffs.

## Verification

Implementation validation starts narrow, then proves the full local boundary.

### Focused Tests

- Repository Catalog pagination, token-visible results, exact-origin checks,
  error redaction, and project re-resolution;
- Feishu signature/connection validation, event dedupe, mention normalization,
  direct/group binding, `/new`, card versioning, and unauthorized callbacks;
- Feishu China `Domain.Feishu` configuration for both REST and WS clients;
- Feishu OAuth state, one-time callback, exact redirect handling, tenant
  restriction, subject construction, missing-email denial, and account linking;
- atomic Session/Workspace/message/blocked-Execution creation;
- provisioning leases, deterministic paths, partial clone failure, retry, and
  credential cleanup;
- collector clean-tree and ancestry checks, multi-repository ChangeSets, empty
  changes, failed tests, and bounded artifacts;
- publication authorization, exact-SHA refspec, hook disabling, deterministic
  branch/MR reconciliation, crash recovery, and retry-failed-only behavior; and
- Web Chat picker accessibility, pagination, multi-select, no-project flow,
  append-only additions, diff access, and approval.

### Local End-To-End Proof

Before publication of the implementation:

1. run service format, type, lint, and unit checks;
2. build the affected runtime images with repository recipes;
3. verify the current Kubernetes context, deploy to the local stack, and inspect
   all changed component readiness;
4. from a real China Feishu self-built app, send a direct task, search/select
   multiple projects, complete an Execution, open the diff, approve, and observe
   one merge request per changed repository;
5. repeat from a Feishu group root topic and prove replies stay in the topic;
6. exercise **No project**, `/new`, stale card callback, unauthorized approval,
   append a project, exercise dirty-tree collection failure, and force one
   per-repository publish failure;
7. restart `api-rs` or the ingress during selection, execution rendering, and
   publication, then prove durable recovery without duplicate work; and
8. run the equivalent Web Chat workflow and inspect durable Session messages,
   Executions, ChangeSet, Publish Batch, and user-visible outcome.

Health checks alone are not acceptance evidence.

## Compatibility And Rollout

Existing Sessions keep their current repository/runtime behavior. New durable
fields and tables are additive. No migration synthesizes Workspaces or
ChangeSets for old Sessions.

Rollout order is:

1. additive schema and read-only Repository Catalog;
2. Workspace Manager and Web Chat project selection behind feature flags;
3. Feishu ingress and project cards with publication disabled;
4. ChangeSet collection and authenticated diff review;
5. Publisher enabled for an operator cohort;
6. Feishu OAuth enabled for allowed tenants; and
7. wider rollout after audit, recovery, and partial-failure evidence is clean.

Turning off a feature gate stops new operations without deleting Workspaces or
invalidating existing review/audit records. Disabling publication never causes
an implicit push.

## Evolution Beyond The First Release

### Slack Adoption

Slack can later map channel/thread identity to Channel Binding, use Block Kit
for the same Repository selection and approval state, and consume the same
ChangeSet and Publication APIs. Its existing event normalization and rendering
remain useful; it does not require copying Feishu transport code.

### Assistants And Enterprise Connections

Centaur can introduce a versioned Assistant product object containing role,
instructions, allowed tools, knowledge sources, workflow entry points, and
publication channels. Connections represent external services and work
identities, with credentials resolved only at execution time through controlled
proxies or internal gateways. This extends the product beyond coding to logs,
service checks, approval workflows, incident handling, and knowledge work on
remote systems.

### Multi-Agent Execution

Multi-Agent means explicit orchestration, delegation, permissions, budgets,
result synthesis, and auditability; it is not merely multiple prompts to Codex.
Every concurrently writing Agent receives an independent Git worktree or
Workspace branch. Agents never share one mutable working tree. An orchestrator
collects their outputs and applies or merges approved commits into the primary
Workspace before creating a new reviewable ChangeSet.

The initial user experience should still expose one real `@Centaur` bot and let
the user choose an Assistant or workflow in Centaur. Creating a separate
Feishu bot for every virtual role is optional presentation, not the Agent
architecture.

### Governance And Evaluation

Later releases add Assistant versioning, test conversations, evaluation sets,
cost/latency policies, connector health, approval matrices, retention rules,
and organization-wide audit search. These capabilities turn the durable
execution core into an enterprise AI platform rather than only a chat wrapper
around Codex.

## First-Release Acceptance Boundary

The release is complete only when a user can perform this closed loop in both
China Feishu and Web Chat:

```text
send task
-> select zero/one/many token-visible GitLab projects
-> reuse one durable Session and persistent Workspace
-> let Codex edit, test, and create ordinary local commits
-> review an immutable multi-repository ChangeSet
-> authorize Create MR as initiator or Centaur admin
-> push exact approved SHAs and create one MR per changed repository
-> retain successes and retry failed repositories only
```

It is not sufficient for the bot to answer, for the sandbox to report success,
or for a branch to exist locally. Durable state, exact review artifacts,
authorization, remote GitLab outcome, and final channel delivery must all agree.

## Primary References

- [China Feishu self-built application development process](https://open.feishu.cn/document/home/introduction-to-custom-app-development/self-built-application-development-process)
- [China Feishu long-connection event subscription](https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/event-subscription-guide/long-connection-mode)
- [China Feishu receive-message event](https://open.feishu.cn/document/server-docs/im-v1/message/events/receive)
- [China Feishu send message API](https://open.feishu.cn/document/server-docs/im-v1/message/create)
- [China Feishu reply message API](https://open.feishu.cn/document/server-docs/im-v1/message/reply)
- [China Feishu update message card API](https://open.feishu.cn/document/server-docs/im-v1/message-card/patch)
- [Official Feishu/Lark Node SDK Chinese README](https://github.com/larksuite/node-sdk/blob/main/README.zh.md)
