# GitLab Workspace Publication Path Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep agent changes in the durable Workspace and publish approved commits through the configured GitLab publisher.

**Architecture:** Align Workspace lifecycle job ownership with the sandbox agent, then append a development-specific runtime prompt that overrides the generic repo-cache clone workflow. Enable the already-implemented publisher only in the explicit local deployment values.

**Tech Stack:** Rust, Kubernetes Jobs, Python prompt composition, Helm, Kind.

## Global Constraints

- Never expose or inject the real GitLab token into a normal sandbox.
- Use GitLab API v3 and the existing Secret reference.
- Keep the shared Helm chart write path opt-in.
- Preserve unrelated working-tree changes.

---

### Task 1: Make Development Workspace Guidance Authoritative

**Files:**
- Modify: `services/sandbox/compose_system_prompt.py`
- Modify: `services/sandbox/entrypoint.sh`
- Test: `services/sandbox/test_compose_system_prompt.py`

**Interfaces:**
- Consumes: `CENTAUR_WORKSPACE_ROOT` selected by the sandbox entrypoint.
- Produces: an appended prompt block that routes all selected-repository writes to `/workspace/repos` and publication to the approval flow.

- [ ] Add a failing prompt-composition test for development Workspace rules.
- [ ] Run the focused test and verify it fails because the rules are absent.
- [ ] Add the development prompt option and pass it from the entrypoint.
- [ ] Run the focused sandbox tests and verify they pass.

### Task 2: Align Workspace Lifecycle Ownership

**Files:**
- Modify: `services/api-rs/crates/centaur-sandbox-agent-k8s/src/workspace.rs`

**Interfaces:**
- Consumes: the default `centaur-agent` UID/GID contract of 1001.
- Produces: provisioner, collector, and publisher pod/container security contexts using UID/GID 1001.

- [ ] Add failing assertions for UID, GID, and fsGroup 1001 on every Workspace lifecycle job.
- [ ] Run the focused Rust tests and verify they fail against UID 1000.
- [ ] Update the shared Workspace security context values to 1001.
- [ ] Run the focused crate tests and verify they pass.

### Task 3: Enable And Prove Local Publication

**Files:**
- Modify: `contrib/chart/values.local.yaml` (ignored local deployment configuration)

**Interfaces:**
- Consumes: Secret `gitlab-repo-token`, key `token`.
- Produces: publisher RBAC, `GITLAB_PUBLISHING_ENABLED=true`, and approval-triggered publisher jobs.

- [ ] Enable `gitlabPublishing` with the existing Secret reference.
- [ ] Render the Chart and verify publisher RBAC and API environment wiring.
- [ ] Build and deploy the affected local images with explicit `kind-centaur` context checks.
- [ ] Repair the current Workspace file ownership and preserve commit `08b202d0` in the durable repository.
- [ ] Run a real ChangeSet and publication smoke test without exposing credentials.
