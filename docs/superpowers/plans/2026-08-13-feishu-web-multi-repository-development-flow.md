# Feishu And Web Multi-Repository Development Flow Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver the approved China Feishu and Web Chat workflow from task intake through multi-repository Codex execution, immutable review, approval, GitLab push, and one merge request per changed repository.

**Architecture:** `api-rs` remains the durable control plane. Platform-neutral domain types live in `centaur-session-core`, transactional state lives in `centaur-session-sqlx`, API orchestration lives in focused `centaur-api-server` modules, and Git/Kubernetes side effects sit behind Workspace and Publisher traits. `services/feishubot` and Console are thin transport/rendering clients of the same contracts.

**Tech Stack:** Rust 2024/Axum/SQLx/Postgres/Kube, Git and GitLab v4 API, TypeScript/Bun with `@larksuiteoapi/node-sdk`, Rails 8, Helm/Kubernetes, persistent volumes.

## Global Constraints

- China Feishu uses an enterprise self-built app and explicit `Lark.Domain.Feishu`; do not call international Lark endpoints.
- The first Feishu task message, Workspace, selection flow, and blocked Execution are one durable intake; no sandbox starts before project confirmation and Workspace readiness.
- One Session owns one persistent Workspace. Repository membership is append-only and only users can add repositories.
- Users select opaque `gitlab:<numeric-project-id>` IDs; clients never submit clone URLs, paths, branch names, SHAs, or authorization roles.
- One deployment token defines all visible projects. The token is absent from browsers, Console, feishubot, sandboxes, harnesses, and Workspace files.
- Codex creates ordinary append-only local commits and a clean tree. The collector never stages or commits.
- No push happens before task-initiator or Centaur-admin approval. Publisher pushes the exact reviewed SHA with hooks disabled.
- A partially successful Publish Batch preserves successful MRs and retries failed items only.
- Console Feishu login uses China Feishu OAuth. Stable identity is encoded from `(tenant_key, union_id)`; `open_id` is delivery identity, not a cross-app subject.
- Existing Slack behavior and legacy Session creation remain compatible throughout rollout.
- Every production behavior is added test-first and every external side effect has an idempotent recovery test.

---

### Task 1: Development Domain Types And Durable Schema

**Files:**
- Create: `services/api-rs/crates/centaur-session-core/src/development.rs`
- Modify: `services/api-rs/crates/centaur-session-core/src/lib.rs`
- Create: `services/api-rs/crates/centaur-session-sqlx/migrations/0052_development_workspaces.sql`
- Create: `services/api-rs/crates/centaur-session-sqlx/src/development.rs`
- Modify: `services/api-rs/crates/centaur-session-sqlx/src/lib.rs`

**Interfaces:**
- Produces `RepositoryId`, `WorkspaceState`, `WorkspaceRepositoryState`, `SelectionFlowState`, `ExecutionBlocker`, `ChangeSetState`, `PublishBatchState`, and `PublishItemState`.
- Produces durable rows for channel bindings, Workspaces, selection flows, repositories, ChangeSets, Publish Batches/Items, and Feishu delivery.
- Adds nullable `blocking_reason` to `session_executions`; execution claim queries select queued rows only when it is null.

- [x] **Step 1: Write failing domain tests**

Add tests in `development.rs` that require:

```rust
#[test]
fn repository_id_accepts_only_numeric_gitlab_projects() {
    assert_eq!(RepositoryId::parse("gitlab:42").unwrap().project_id(), 42);
    for invalid in ["42", "github:42", "gitlab:name", "gitlab:0"] {
        assert!(RepositoryId::parse(invalid).is_err(), "accepted {invalid}");
    }
}

#[test]
fn publish_batch_reduces_item_states_without_losing_partial_success() {
    assert_eq!(
        PublishBatchState::from_items([PublishItemState::Succeeded, PublishItemState::Failed]),
        PublishBatchState::PartiallySucceeded
    );
}
```

- [x] **Step 2: Run the focused test and verify RED**

Run:

```bash
cd services/api-rs
cargo test -p centaur-session-core development
```

Expected: compile failure because `development` and its types do not exist.

- [x] **Step 3: Implement the minimal typed state machines**

Use private string fields for parsed IDs, `serde` snake-case enums, `strum` database serialization, and transition methods returning typed errors. Export the module from `lib.rs`; do not add repository behavior to the existing monolithic file.

- [x] **Step 4: Add the schema migration**

Create `0052_development_workspaces.sql` with:

```sql
alter table session_executions add column if not exists blocking_reason text;

create table session_workspaces (
  workspace_id text primary key,
  thread_key text not null unique references sessions(thread_key) on delete cascade,
  state text not null,
  storage_ref text,
  preparation_attempt integer not null default 0,
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now(),
  constraint session_workspaces_state_check check
    (state in ('awaiting_selection','provisioning','ready','failed'))
);
```

The same migration creates the remaining tables and unique constraints named in the design: `(workspace_id, repository_id)`, `(workspace_id, relative_path)`, platform event/message idempotency, one active selection flow, `(changeset_id, repository_id)`, `(changeset_id, idempotency_key)`, and `(publish_batch_id, repository_id)`.

- [x] **Step 5: Add SQLx row conversions and schema tests**

`development.rs` in SQLx owns row structs and conversion errors. Add a database test that runs migrations, creates a legacy Session, attaches an empty Workspace, inserts a blocked Execution, and proves a duplicate repository binding and duplicate platform event are rejected.

- [x] **Step 6: Make focused checks GREEN**

Run:

```bash
cargo +nightly fmt --all --check
cargo test -p centaur-session-core development
SESSION_RUNTIME_TEST_DATABASE_URL="$TEST_DATABASE_URL" cargo test -p centaur-session-sqlx development
```

Expected: domain tests pass; database test either passes against disposable Postgres or explicitly reports it was skipped when no URL exists.

- [x] **Step 7: Commit**

```bash
git add services/api-rs/crates/centaur-session-core services/api-rs/crates/centaur-session-sqlx
git commit -m "feat: add durable development workspace model"
```

### Task 2: Atomic Task Intake And Repository Selection

**Files:**
- Modify: `services/api-rs/crates/centaur-session-sqlx/src/development.rs`
- Modify: `services/api-rs/crates/centaur-session-runtime/src/lib.rs`
- Create: `services/api-rs/crates/centaur-api-server/src/development.rs`
- Modify: `services/api-rs/crates/centaur-api-server/src/{lib,routes,types,client}.rs`

**Interfaces:**
- Consumes `AcceptDevelopmentTaskRequest { channel, platform_event_id, platform_message_id, harness_type, initiator, message }`.
- Produces `AcceptedDevelopmentTask { thread_key, workspace_id, selection_flow_id, execution_id, created }`.
- Produces versioned confirm/no-project/cancel/add-project mutations.

- [x] **Step 1: Write failing transactional tests**

Add SQLx tests proving one call creates Session, Workspace, message, selection flow, and queued Execution with `awaiting_project_selection`; replaying the same platform event returns the same IDs and no duplicate rows. Add a claim test proving blocked queued Executions cannot transition to running.

- [x] **Step 2: Verify RED**

Run:

```bash
SESSION_RUNTIME_TEST_DATABASE_URL="$TEST_DATABASE_URL" cargo test -p centaur-session-sqlx accept_development_task
cargo test -p centaur-session-runtime blocked_execution
```

Expected: missing method/type failures.

- [x] **Step 3: Implement one SQL transaction**

Implement:

```rust
pub async fn accept_development_task(
    &self,
    request: &AcceptDevelopmentTask,
) -> Result<AcceptedDevelopmentTask, SessionStoreError>;
```

It locks the channel binding, resolves or creates the Session generation, performs all inserts, and returns an existing result on idempotent replay. Existing legacy `create_or_get_session`, `append_messages`, and `create_execution` remain unchanged.

- [x] **Step 4: Implement selection state transitions**

Confirm re-resolves opaque IDs through a caller-supplied resolved snapshot list, inserts bindings append-only, increments flow version, and changes the Execution blocker to `workspace_provisioning`. No-project confirms an empty set. Cancel marks the draft cancelled and cancels its blocked Execution. Add-project rejects active Execution/publication and existing Repository IDs.

- [x] **Step 5: Add route tests before routes**

Use Axum `oneshot` tests for:

```text
POST /api/development/tasks
POST /api/development/selections/:id/confirm
POST /api/development/selections/:id/no-project
POST /api/development/selections/:id/cancel
POST /api/development/sessions/:thread_key/repositories
```

Require malformed IDs and caller-supplied clone URLs/roles to receive `400`, stale versions `409`, and replayed event keys `200` with `created: false`.

- [x] **Step 6: Implement thin handlers and client methods**

Handlers parse platform-neutral requests and call runtime/store services. Do not include Feishu card or Console HTML knowledge. Add matching typed methods to the Rust client for service integration tests.

- [x] **Step 7: Verify and commit**

```bash
cargo +nightly fmt --all --check
cargo test -p centaur-session-sqlx development
cargo test -p centaur-session-runtime blocked_execution
cargo test -p centaur-api-server development
git add services/api-rs
git commit -m "feat: add durable development task intake"
```

### Task 3: GitLab Repository Catalog

**Files:**
- Create: `services/api-rs/crates/centaur-api-server/src/gitlab.rs`
- Modify: `services/api-rs/crates/centaur-api-server/src/{development,routes,args,error}.rs`
- Modify: `services/api-rs/Cargo.toml`

**Interfaces:**
- Produces `GitLabCatalog::search(query, cursor) -> RepositoryPage` and `resolve(ids) -> Vec<ResolvedRepository>`.
- Configuration: exact GitLab base URL, token file path, page size, request timeout.

- [x] **Step 1: Write failing adapter tests**

Use a local HTTP test server to require `/api/v4/projects?membership=true&simple=true`, keyword search, GitLab `X-Next-Page` cursor conversion, `PRIVATE-TOKEN` header presence without logging it, archived/default-branch rejection, and exact scheme/host/port clone URL validation.

- [x] **Step 2: Verify RED**

```bash
cargo test -p centaur-api-server gitlab_catalog
```

Expected: missing `GitLabCatalog` module.

- [x] **Step 3: Implement the adapter and routes**

Read the token from its file for each request or a redaction-safe secret wrapper. Return only opaque ID, name, namespace, bounded description, default branch, archived, and last activity. Never serialize clone URLs.

- [x] **Step 4: Verify errors and pagination**

Run the focused tests and add route coverage for disabled catalog (`404`), timeout/upstream (`503` with sanitized code), invalid cursor (`400`), and full page traversal.

- [x] **Step 5: Commit**

```bash
git add services/api-rs
git commit -m "feat: add GitLab repository catalog"
```

### Task 4: Persistent Workspace Provisioning And Sandbox Attachment

**Files:**
- Create: `services/api-rs/crates/centaur-sandbox-core/src/workspace.rs`
- Modify: `services/api-rs/crates/centaur-sandbox-core/src/{lib,spec}.rs`
- Create: `services/api-rs/crates/centaur-sandbox-agent-k8s/src/workspace.rs`
- Modify: `services/api-rs/crates/centaur-sandbox-agent-k8s/src/lib.rs`
- Create: `services/api-rs/crates/centaur-session-runtime/src/development.rs`
- Modify: `services/api-rs/crates/centaur-session-runtime/src/lib.rs`

**Interfaces:**
- Produces `WorkspaceManager::prepare(workspace) -> WorkspacePreparation` and `WorkspaceMount`.
- Kubernetes implementation creates one PVC per Session and one short-lived Provisioner Job per attempt.

- [ ] **Step 1: Write failing backend-neutral tests**

Require deterministic repository paths (`repos/42-project`), path collision rejection, append-only plans, sanitized `workspace.json`, and `SandboxSpec` mounting the Workspace at `/workspace` without any Git credential reference.

- [ ] **Step 2: Verify RED**

```bash
cargo test -p centaur-sandbox-core workspace
cargo test -p centaur-session-runtime workspace_preparation
```

- [ ] **Step 3: Define the Workspace trait and local fake**

The trait consumes resolved server-side repository snapshots and an opaque credential reference. `SessionRuntime` depends on the trait, persists `provisioning` before calling it, persists each structured result, and clears `workspace_provisioning` only when all bindings are ready.

- [ ] **Step 4: Write failing Kubernetes object tests**

Assert PVC owner labels, Job lease/attempt labels, init-only token Secret, noninteractive `GIT_ASKPASS`, exact-origin clone commands, hooks disabled, local branch creation, manifest write, and no credential volume on the eventual sandbox.

- [ ] **Step 5: Implement Kube Workspace Manager**

Build Kubernetes objects with typed APIs. The Provisioner reports machine-readable JSON through Job termination/output; reconciliation adopts existing PVCs and finished Jobs by Workspace/attempt labels before creating anything.

- [ ] **Step 6: Attach Workspace and release Execution**

Extend sandbox preparation so a ready Session Workspace supplies a named-volume/PVC mount at `/workspace`. Legacy Sessions retain the current EmptyDir/repo-cache behavior. Trigger the existing execution driver only after the persisted blocker is null.

- [ ] **Step 7: Verify and commit**

```bash
cargo +nightly fmt --all --check
cargo test -p centaur-sandbox-core workspace
cargo test -p centaur-sandbox-agent-k8s workspace
cargo test -p centaur-session-runtime workspace_preparation
git add services/api-rs
git commit -m "feat: provision persistent session workspaces"
```

### Task 5: ChangeSet Collection

**Files:**
- Create: `services/api-rs/crates/centaur-session-runtime/src/changeset.rs`
- Modify: `services/api-rs/crates/centaur-session-runtime/src/{development,lib}.rs`
- Modify: `services/api-rs/crates/centaur-session-sqlx/src/development.rs`
- Modify: `services/api-rs/crates/centaur-api-server/src/{development,routes,types}.rs`

**Interfaces:**
- Produces `ChangeSetCollector::collect(execution, workspace) -> ChangeSet`.
- Produces authenticated ChangeSet summary/artifact endpoints.

- [ ] **Step 1: Write failing real-Git tests**

Create temporary repositories and prove clean committed changes yield exact base/head SHAs and stable patch hashes; empty changes yield no ChangeSet; dirty/untracked/staged trees, non-descendant heads, missing objects, and rewritten recorded history are non-publishable.

- [ ] **Step 2: Verify RED**

```bash
cargo test -p centaur-session-runtime changeset
```

- [ ] **Step 3: Implement a read-only Git command runner**

Use explicit `git -C <validated-path>` arguments, bounded stdout/stderr, `--no-ext-diff`, and no shell interpolation. Collect `status --porcelain=v1 -z`, `merge-base --is-ancestor`, commit metadata, `diff --binary`, and SHA-256 artifact hashes. The module exposes no stage/commit/reset methods.

- [ ] **Step 4: Persist immutable review state**

Write the ChangeSet and per-repository entries transactionally. Link structured test evidence from Execution metadata/events and store bounded artifacts through an `ArtifactStore` abstraction. Mark incomplete commit contracts `needs_agent_completion`.

- [ ] **Step 5: Add review route authorization tests**

Require Session owner/shared access for summary and artifact reads. Verify a signed link still returns `403` for an authenticated user without Session access and never follows mutable Workspace HEAD.

- [ ] **Step 6: Verify and commit**

```bash
cargo +nightly fmt --all --check
cargo test -p centaur-session-runtime changeset
cargo test -p centaur-api-server changeset
git add services/api-rs
git commit -m "feat: collect immutable workspace changesets"
```

### Task 6: Approval, Publisher, And Partial Retry

**Files:**
- Create: `services/api-rs/crates/centaur-session-runtime/src/publisher.rs`
- Modify: `services/api-rs/crates/centaur-session-runtime/src/{development,lib}.rs`
- Modify: `services/api-rs/crates/centaur-session-sqlx/src/development.rs`
- Modify: `services/api-rs/crates/centaur-api-server/src/{development,routes,types,args,error}.rs`

**Interfaces:**
- Produces `PublicationService::approve`, `retry_failed`, and `reconcile`.
- Produces one deterministic source branch and one GitLab MR per changed repository.

- [ ] **Step 1: Write failing authorization and state tests**

Prove initiator and Centaur admin may approve; visible group members and shared-thread readers may not. Require clean exact HEAD for a new batch, one active batch per ChangeSet, immutable approver/idempotency data, and retry selection containing failed items only.

- [ ] **Step 2: Verify RED**

```bash
cargo test -p centaur-session-runtime publication
```

- [ ] **Step 3: Implement durable approval and leases**

Persist the batch/items before external work. Acquire a Workspace publication lease that blocks new Executions for the attempt. Release it only after every started item has a durable terminal or retryable state.

- [ ] **Step 4: Write failing Publisher integration tests**

Against a temporary bare Git remote and fake GitLab API, require exact-SHA refspec, deterministic `centaur/<workspace-short>/<changeset-short>` branch, `core.hooksPath` disabled, remote-branch content verification, MR lookup before create, no agent code execution, and adoption after simulated crashes following push and MR creation.

- [ ] **Step 5: Implement Publisher and retry reconciliation**

Publisher reads the token only from its injected token-file reference. It pushes the stored SHA, verifies the remote branch, creates/finds the MR with a ChangeSet marker, and persists each external identity immediately. Retry accepts approved partial batches after Workspace advances but can publish only the originally recorded object.

- [ ] **Step 6: Add API tests and delivery events**

Add approve/retry/get endpoints with one-time idempotency keys and durable `development.publish_*` events for platform renderers.

- [ ] **Step 7: Verify and commit**

```bash
cargo +nightly fmt --all --check
cargo test -p centaur-session-runtime publication
cargo test -p centaur-api-server publication
git add services/api-rs
git commit -m "feat: publish approved GitLab merge requests"
```

### Task 7: China Feishu Ingress And Cards

**Files:**
- Create: `services/feishubot/AGENTS.md`
- Create: `services/feishubot/package.json`
- Create: `services/feishubot/tsconfig.json`
- Create: `services/feishubot/Dockerfile`
- Create: `services/feishubot/src/{config,feishu-events,session-api,selection-cards,render-recovery,server}.ts`
- Create: `services/feishubot/test/*.test.ts`
- Modify: `pnpm-workspace.yaml`

**Interfaces:**
- Consumes Feishu `im.message.receive_v1`, card actions, and durable API events.
- Produces normalized development-task intake, selection mutations, approval, retry, and same-topic delivery.

- [ ] **Step 1: Add the package with failing pure event tests**

Fixtures must prove `Domain.Feishu`, self-built app config, mention removal, bot/self rejection, DM binding, group `chat_id + root_id/message_id` binding, `/new`, `/projects`, event/message dedupe keys, and bounded normalized attachments/text.

- [ ] **Step 2: Verify RED**

```bash
pnpm --filter feishubot test
```

Expected: missing service/modules.

- [ ] **Step 3: Implement transport and API client**

Use `@larksuiteoapi/node-sdk` `Client` and `WSClient` with explicit `AppType.SelfBuild` and `Domain.Feishu`. Acknowledge before slow work, send opaque identifiers to `api-rs`, and never import GitLab configuration.

- [ ] **Step 4: Write failing card state tests**

Cover recent results, search, cursor pagination, cross-page selection, removal, Confirm/No project/Cancel, `/projects` append flow, disabled stale versions, unauthorized approval, partial result display, and retry-failed action. Validate all visible fallback text and card size limits.

- [ ] **Step 5: Implement cards and render recovery**

Render desired state from durable API reads. Store Feishu message/card ID, last event cursor, and rendered version through `api-rs`; restart replay must update an existing card rather than post a duplicate.

- [ ] **Step 6: Prove long-connection callbacks**

Add an SDK boundary contract test for `im.message.receive_v1` and `card.action.trigger`. In the real China tenant smoke test, prove both over WSClient; if the tenant rejects card callbacks, configure only the card-action adapter for encrypted webhook delivery without changing normalized handlers.

- [ ] **Step 7: Verify and commit**

```bash
pnpm --filter feishubot run check:types
pnpm --filter feishubot test
git add services/feishubot pnpm-workspace.yaml pnpm-lock.yaml
git commit -m "feat: add China Feishu development bot"
```

### Task 8: Web Chat Project Picker, Review, And Publication

**Files:**
- Modify: `services/console/app/services/centaur_api_client.rb`
- Modify: `services/console/app/controllers/console/threads_controller.rb`
- Create: `services/console/app/javascript/controllers/repository_picker_controller.js`
- Modify: `services/console/app/views/console/threads/{_composer,_thread_panel,_transcript}.html.erb`
- Modify: `services/console/config/routes.rb`
- Modify: `services/console/app/assets/stylesheets/application.css`
- Modify: `services/console/test/{services/centaur_api_client_test.rb,controllers/console/threads_controller_test.rb}`

**Interfaces:**
- Consumes Repository Catalog, development task intake, ChangeSet review, approve, and retry APIs.
- Produces one New Chat submission with task plus zero/one/many Repository IDs and authenticated review controls.

- [ ] **Step 1: Write failing API-client tests**

Require exact URL encoding and JSON bodies for catalog search, task intake, append repositories, ChangeSet read, publication approval, and retry. Ensure clone URL, branch, SHA, and role fields cannot be passed through client method signatures.

- [ ] **Step 2: Verify RED**

```bash
cd services/console
bin/rails test test/services/centaur_api_client_test.rb
```

- [ ] **Step 3: Implement thin client methods**

Reuse existing authenticated `CentaurApiClient` transport and error mapping. Do not query or mutate the new api-rs tables through ActiveRecord.

- [ ] **Step 4: Write failing controller/view tests**

Require New Chat search/pagination/multi-select, preserved selections, explicit No project, one task-intake call instead of create/append/execute, repository chips in thread header, idle-only Add projects, immutable ChangeSet diff link, owner/admin Create MR, unauthorized absence/server rejection, and partial retry UI.

- [ ] **Step 5: Implement the accessible picker and controls**

Use a compact unframed picker in the composer: search input, result checkboxes, selected list, pagination icon buttons with tooltips, and No project checkbox. Keep selected IDs in hidden form fields and re-resolve server-side. No token or provider URL reaches HTML.

- [ ] **Step 6: Implement controller mutations through API only**

New Chat calls atomic task intake with confirmed repositories. Follow-ups retain existing append/execute behavior. Add-project, approval, and retry actions check owned/admin UI policy and rely on `api-rs` for authoritative authorization.

- [ ] **Step 7: Verify and commit**

```bash
bin/rails test test/services/centaur_api_client_test.rb test/controllers/console/threads_controller_test.rb
bin/rubocop app/services/centaur_api_client.rb app/controllers/console/threads_controller.rb
git add services/console
git commit -m "feat: add multi-repository Web Chat workflow"
```

### Task 9: China Feishu OAuth Login

**Files:**
- Create: `services/console/lib/login/providers/feishu.rb`
- Modify: `services/console/lib/login/providers.rb`
- Modify: `services/console/lib/console_auth.rb`
- Modify: `services/console/app/models/{user,user_identity}.rb`
- Generate: `services/console/db/migrate/*_add_feishu_identity_attributes.rb`
- Modify: `services/console/db/schema.rb`
- Modify: `services/console/app/controllers/session_oauth_controller.rb`
- Modify: `services/console/app/views/sessions/new.html.erb`
- Create/Modify: `services/console/test/lib/login/providers/feishu_test.rb`
- Modify: `services/console/test/{controllers/session_oauth_controller_test.rb,models/user_identity_test.rb,models/user_test.rb}`

**Interfaces:**
- Produces provider key `feishu` and stable encoded subject from tenant key plus union ID.
- Caches tenant/open IDs as provider attributes without using Slack `team_id`.

- [ ] **Step 1: Write failing provider tests**

Use a fake token/user-info server to require China Feishu endpoints, signed state and single-use encrypted flow cookie, exact redirect URI, tenant allowlist, non-empty union/open IDs, enterprise email, sanitized provider errors, and no token persistence. Assert the same union ID in two tenants creates two subjects.

- [ ] **Step 2: Verify RED**

```bash
bin/rails test test/lib/login/providers/feishu_test.rb test/controllers/session_oauth_controller_test.rb
```

- [ ] **Step 3: Generate the identity migration**

Run Rails migration generation, add neutral `tenant_key` and `open_id` columns to `user_identities`, and retain the global `(provider, subject)` unique index. Add provider `feishu` without weakening existing Google/Slack validation.

- [ ] **Step 4: Implement the OAuth strategy**

Exchange the authorization code against China Feishu, call official user-info, validate the configured tenant, encode a length-delimited or canonical JSON subject from `(tenant_key, union_id)`, and return provider-asserted email only after those checks. Missing email fails closed and Feishu login never bootstrap-promotes an administrator.

- [ ] **Step 5: Link runtime Principal and Console User**

On successful login, reconcile the stored `(tenant_key, open_id)` attribute to the existing provider-scoped bot Principal. Exact allowed email linking follows current verified-identity policy; display names never link accounts.

- [ ] **Step 6: Run full Console security checks and commit**

```bash
bin/rails test
bin/rubocop
bin/brakeman --quiet --no-pager --exit-on-warn --exit-on-error
git add services/console
git commit -m "feat: add China Feishu console login"
```

### Task 10: Helm Wiring And Credential Isolation

**Files:**
- Modify: `contrib/chart/values.yaml`
- Modify: `contrib/chart/values.schema.json`
- Create: `contrib/chart/templates/feishubot.yaml`
- Create: `contrib/chart/templates/development-workspaces.yaml`
- Modify: `contrib/chart/templates/{apirs,console,networkpolicy,secrets,_helpers}.yaml`
- Create: `contrib/chart/tests/test_development_workflow.sh`
- Modify: image/build recipes under the existing root `Justfile` or service recipes.

**Interfaces:**
- Adds `workspaceRepositories`, `gitlabPublishing`, `feishubot`, and `console.feishuLogin` values from the design.
- Mounts GitLab Secret only into api-rs Catalog credential path and short-lived Provisioner/Publisher jobs.

- [ ] **Step 1: Write failing Helm render assertions**

Render neutral GitLab/Feishu values and assert: feishubot gets Feishu secrets but no GitLab secret; Console gets OAuth secrets but no GitLab token; sandbox specs get Workspace PVC but no GitLab secret; api-rs gets token file references; provisioner/publisher service accounts are distinct; egress allows exact GitLab scheme port; disabled features render no new workloads.

- [ ] **Step 2: Verify RED**

```bash
bash contrib/chart/tests/test_development_workflow.sh
```

- [ ] **Step 3: Implement values, schema, resources, and policies**

Use explicit Secret key refs rather than broad `envFrom`. Add least-privilege RBAC for PVC/Job reconciliation. Keep examples deployment-neutral and require allowed Feishu tenant keys when Feishu ingress/login is enabled.

- [ ] **Step 4: Add metrics, probes, and build wiring**

Expose bounded metrics and health/readiness endpoints for feishubot; add its image to existing build/deploy recipes. api-rs readiness must report initialized even when optional GitLab is disabled, and report configuration failure before accepting development routes when enabled but invalid.

- [ ] **Step 5: Verify and commit**

```bash
bash contrib/chart/tests/test_development_workflow.sh
helm lint contrib/chart
helm template centaur contrib/chart >/tmp/centaur-default.yaml
git diff --check
git add contrib/chart Justfile services/feishubot/Dockerfile
git commit -m "feat: deploy Feishu development workflow"
```

### Task 11: Integrated Recovery And Acceptance Proof

**Files:**
- Modify tests and operator documentation only where verification exposes gaps.
- Modify: `docs/pages/architecture.mdx`
- Modify: `docs/pages/reference/configuration.mdx`
- Modify: `docs/pages/quickstart.mdx`

**Interfaces:**
- Consumes every prior deliverable.
- Produces fresh evidence for the complete first-release acceptance boundary.

- [ ] **Step 1: Run all affected static/unit suites**

```bash
cd services/api-rs
cargo +nightly fmt --all --check
cargo +nightly clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd ../..
pnpm --filter feishubot run check:types
pnpm --filter feishubot test
cd services/console
bin/ci
cd ../..
helm lint contrib/chart
git diff --check
```

- [ ] **Step 2: Deploy to the verified local context**

Run `kubectl config current-context`, require the documented local context, build affected images, run `just deploy`, and inspect readiness and migration output. Never use an unverified ambient non-local context.

- [ ] **Step 3: Prove the Web Chat closed loop**

Using disposable GitLab projects, select two repositories, let Codex modify/test/commit, inspect exact ChangeSet diffs, approve as initiator, and verify two remote branches/MRs contain exactly the approved heads. Repeat No project then Add projects.

- [ ] **Step 4: Prove the China Feishu closed loop**

From a real China Feishu self-built app, exercise DM task-first selection, follow-up reuse, `/new`, group root-topic isolation, card search/pagination/multi-select, ChangeSet link, authorized approval, unauthorized callback, and final MR card delivery.

- [ ] **Step 5: Prove crash and partial-failure recovery**

Restart ingress/api-rs during selection and rendering; kill Publisher after push and after MR create; force one repository to fail. Verify no duplicate Session/Execution/branch/MR, successes remain visible, and Retry failed processes only the failed repository at the original SHA.

- [ ] **Step 6: Document verified operations and commit**

Document configuration, permissions, rollback flags, Workspace lifecycle, audit fields, and the plaintext HTTP GitLab risk without private addresses or tokens.

```bash
git add docs/pages
git commit -m "docs: operate the Feishu development workflow"
```

- [ ] **Step 7: Final requirement-by-requirement audit**

Re-read the approved design and map every first-release acceptance item to a passing test, rendered manifest, durable row, remote GitLab artifact, and Feishu/Web user-visible result. Do not mark the goal complete while any evidence is missing.
