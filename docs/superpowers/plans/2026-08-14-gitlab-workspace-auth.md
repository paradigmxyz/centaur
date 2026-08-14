# GitLab Workspace Authentication Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Clone repositories using the username owned by the configured GitLab token and terminate failed workspace executions visibly.

**Architecture:** Resolve the GitLab username inside the short-lived provisioner from the mounted token and the configured repository origin. Keep execution failure durability in the SQLx session store so every ingress observes the same terminal state.

**Tech Stack:** Rust, Python provisioner script embedded in Rust, PostgreSQL/SQLx, Bun/TypeScript, Kubernetes/Kind.

## Global Constraints

- Keep GitLab setup token-only; do not add a username setting.
- Keep credentials and upstream error bodies out of logs, persisted state, and Git URLs.
- Use GitLab API v3 only.
- Preserve unrelated working-tree files.

---

### Task 1: Resolve GitLab Username In The Provisioner

**Files:**
- Modify: `services/api-rs/crates/centaur-sandbox-agent-k8s/src/workspace.rs`

**Interfaces:**
- Consumes: mounted `CENTAUR_GIT_TOKEN_FILE` and repository `clone_url`.
- Produces: a validated username used only by the provisioner's `GIT_ASKPASS` script.

- [x] Add a generated-job regression assertion requiring `/api/v3/user` identity lookup and forbidding the fixed `oauth2` username.
- [x] Run the focused crate test and confirm it fails on the missing identity lookup.
- [x] Update the embedded provisioner script to resolve and validate the username before cloning.
- [x] Apply the same token-derived username to the publication push Job.
- [x] Run the focused crate tests and confirm they pass.

### Task 2: Terminate Failed Workspace Executions

**Files:**
- Modify: `services/api-rs/crates/centaur-session-sqlx/src/development.rs`

**Interfaces:**
- Consumes: `CompleteWorkspacePreparation` with at least one failed repository.
- Produces: failed `session_executions` state and a standard terminal session event.

- [x] Change the existing failure-state test to require a failed, unblocked execution and terminal failure event.
- [x] Run the focused SQLx test and confirm it fails with the existing queued execution.
- [x] Update workspace completion transactionally to fail the affected execution and append its terminal event.
- [x] Reconcile legacy failed-workspace/blocked-execution rows idempotently.
- [x] Run the focused SQLx tests and confirm they pass.

### Task 3: Validate And Deploy

**Files:**
- Modify: `services/feishubot/src/bot.ts`
- Modify: `services/feishubot/test/bot-metrics.test.ts`

**Interfaces:**
- Consumes: terminal `session.execution_failed` events.
- Produces: a terminal red failure card instead of a stale progress card.

- [x] Add a regression test for rendering `session.execution_failed`.
- [x] Treat that event as terminal and failed in the Feishu render loop.
- [x] Run the focused Feishubot tests and confirm they pass.

### Task 4: Validate And Deploy

**Files:**
- Verify all modified files above.

**Interfaces:**
- Consumes: the two fixed runtime behaviors.
- Produces: a locally deployed workflow proven through a real Feishu task.

- [x] Run formatting, focused Rust tests, Feishubot tests, type checks, and `git diff --check`.
- [x] Build `centaur-api-rs` and any affected runtime image, load them into `kind-centaur`, and roll out the deployments.
- [x] Start a fresh local smoke task through the supported development-task workflow.
- [x] Verify the repository reaches `ready`, execution leaves `workspace_provisioning`, and the previous Feishu delivery no longer remains on "工作区准备中".
