# Sandbox Workspace Root Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Start development-session harnesses at the prepared `/workspace` root so agents can discover selected repositories without an absolute path.

**Architecture:** `WorkspaceMount` will own both halves of the control-plane contract: mounting the named volume at `/workspace` and injecting `CENTAUR_WORKSPACE_ROOT=/workspace`. The sandbox entrypoint will delegate path selection to a small shell helper that honors this explicit root first and preserves both existing fallback modes.

**Tech Stack:** Rust (`centaur-sandbox-core`, `centaur-session-runtime`), Bash, Python `unittest`, Docker, Helm, Kind, Kubernetes.

## Global Constraints

- Keep `/workspace` as the development Workspace root and repository parent.
- Preserve persistent-state and legacy `AGENT_REPO` behavior when `CENTAUR_WORKSPACE_ROOT` is absent.
- Do not expose GitLab credentials, clone URLs, or tokens in sandbox environment changes.
- Fail sandbox startup if the selected workspace root cannot be created or entered.
- Validate through a fresh local sandbox and a real development execution that reads `README.md` without an absolute path.

---

### Task 1: Publish the workspace-root sandbox contract

**Files:**
- Modify: `services/api-rs/crates/centaur-sandbox-core/src/workspace.rs`
- Modify: `services/api-rs/crates/centaur-session-runtime/src/development.rs`

**Interfaces:**
- Produces: `WORKSPACE_ROOT_ENV: &str = "CENTAUR_WORKSPACE_ROOT"` and a `SandboxSpec` with one `CENTAUR_WORKSPACE_ROOT=/workspace` entry whenever `WorkspaceMount::apply_to` is used.
- Consumes: existing `SandboxSpec.env`, `EnvVar`, `WORKSPACE_MOUNT_PATH`, and `WorkspaceMount::apply_to`.

- [ ] **Step 1: Write the failing core and runtime assertions**

Add assertions that `WorkspaceMount::apply_to` and the ready-workspace runtime path produce exactly one environment entry named `CENTAUR_WORKSPACE_ROOT` with value `/workspace`. Apply the mount twice in the core test to prove the environment contract is idempotent.

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```bash
cd services/api-rs
cargo test -p centaur-sandbox-core workspace_manifest_and_mount_expose_no_git_credentials
SESSION_RUNTIME_TEST_DATABASE_URL="$LOCAL_TEST_DATABASE_URL" cargo test -p centaur-session-runtime ready_workspace_mounts_pvc_without_gitlab_credentials
```

Expected: at least the core assertion fails because `CENTAUR_WORKSPACE_ROOT` is absent from `SandboxSpec.env`.

- [ ] **Step 3: Implement the minimal contract**

Define `WORKSPACE_ROOT_ENV` next to `WORKSPACE_MOUNT_PATH`. In `WorkspaceMount::apply_to`, remove any existing entry with that name and append:

```rust
EnvVar::new(WORKSPACE_ROOT_ENV, WORKSPACE_MOUNT_PATH)
```

Keep the existing named-volume mount and `working_dir` assignment unchanged.

- [ ] **Step 4: Run the focused tests and verify GREEN**

Run the two commands from Step 2 again. Expected: both selected tests pass with no failures.

- [ ] **Step 5: Commit the control-plane contract**

```bash
git add services/api-rs/crates/centaur-sandbox-core/src/workspace.rs services/api-rs/crates/centaur-session-runtime/src/development.rs
git commit -m "fix: publish sandbox workspace root"
```

### Task 2: Consume the explicit root in the sandbox image

**Files:**
- Create: `services/sandbox/select_workspace_root.sh`
- Create: `services/sandbox/test_select_workspace_root.py`
- Modify: `services/sandbox/entrypoint.sh`
- Modify: `services/sandbox/Dockerfile`

**Interfaces:**
- Produces: executable `select-workspace-root <home-dir> <state-dir> <persistent-state>` that prints one selected absolute path.
- Consumes: `CENTAUR_WORKSPACE_ROOT`, the entrypoint's computed `HOME_DIR`, `STATE_DIR`, and `CENTAUR_PERSISTENT_STATE`.

- [ ] **Step 1: Write the failing selector tests**

Create Python subprocess tests for these exact cases:

```text
CENTAUR_WORKSPACE_ROOT=/workspace, persistent=1 -> /workspace
variable absent, persistent=1 -> <state-dir>/workspace
variable absent, persistent=0 -> <home-dir>/workspace
```

Also assert a relative explicit root exits non-zero with `CENTAUR_WORKSPACE_ROOT must be absolute`.

- [ ] **Step 2: Run the selector tests and verify RED**

Run:

```bash
uv run python -m unittest services.sandbox.test_select_workspace_root -v
```

Expected: ERROR because `services/sandbox/select_workspace_root.sh` does not exist.

- [ ] **Step 3: Implement the selector and wire the entrypoint**

Implement the helper with `set -eu`, explicit-root precedence, absolute-path validation, and the two existing fallbacks. Replace the entrypoint's inline `WORKSPACE_DIR` conditional with:

```bash
WORKSPACE_DIR="$(select-workspace-root "$HOME_DIR" "$STATE_DIR" "${CENTAUR_PERSISTENT_STATE:-0}")"
```

Copy the helper to `/usr/local/bin/select-workspace-root` in the sandbox image with mode `0755`.

- [ ] **Step 4: Run sandbox tests and verify GREEN**

Run:

```bash
uv run python -m unittest services.sandbox.test_select_workspace_root -v
uv run python -m unittest discover -s services/sandbox -p 'test_*.py'
```

Expected: all selector tests and the complete sandbox suite pass.

- [ ] **Step 5: Commit the sandbox consumer**

```bash
git add services/sandbox/select_workspace_root.sh services/sandbox/test_select_workspace_root.py services/sandbox/entrypoint.sh services/sandbox/Dockerfile
git commit -m "fix: honor prepared sandbox workspace root"
```

### Task 3: Verify and deploy the complete path

**Files:**
- Verify only: all Task 1 and Task 2 files.

**Interfaces:**
- Consumes: `centaur-api-rs:latest`, `centaur-agent:latest`, the `kind-centaur` context, and the local Helm release.
- Produces: a fresh sandbox whose PID 1 cwd and Codex `thread/started.cwd` are `/workspace`, plus a completed development execution that reads the selected repository's root `README.md`.

- [ ] **Step 1: Run static and package verification**

```bash
cd services/api-rs
cargo fmt --all --check
cargo clippy -p centaur-sandbox-core -p centaur-session-runtime --all-targets -- -D warnings
cargo test -p centaur-sandbox-core
SESSION_RUNTIME_TEST_DATABASE_URL="$LOCAL_TEST_DATABASE_URL" cargo test -p centaur-session-runtime
cd ../..
git diff --check
```

Expected: every command exits zero.

- [ ] **Step 2: Build affected runtime images**

```bash
just build-one api-rs
just build-one sandbox
```

Expected: both Docker builds finish successfully.

- [ ] **Step 3: Load and deploy to the verified local context**

```bash
kubectl config current-context
kind load docker-image --name centaur centaur-api-rs:latest centaur-agent:latest
just deploy
kubectl --context kind-centaur -n centaur rollout status deploy/centaur-centaur-api-rs --timeout=180s
```

Expected: the current context is `kind-centaur` and the API rollout succeeds.

- [ ] **Step 4: Exercise a fresh development sandbox**

Start a fresh local development execution against the already verified GitLab test repository, using the normal development API path. Ask it `读取 README.md，返回第一行和当前工作目录，不要修改文件。` without an absolute repository path.

Expected runtime evidence:

```text
/proc/1/cwd -> /workspace
CENTAUR_WORKSPACE_ROOT=/workspace
thread/started.params.thread.cwd == /workspace
agent reads repos/<selected-repository>/README.md
session execution status == completed
development.changeset_empty is recorded
```

- [ ] **Step 5: Verify the Feishu delivery boundary**

Inspect the durable session events, `feishu_deliveries`, and Feishu bot logs for the execution. Expected: the final answer is delivered, the delivery reaches its desired revision, and there are no terminal render errors.
