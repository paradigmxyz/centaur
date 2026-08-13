# Sandbox Workspace Root Design

## Problem

Development workspaces are provisioned and mounted at `/workspace`, with repositories
under `/workspace/repos/<repository>`. The sandbox image entrypoint independently
selects `$HOME/workspace` (or the persistent state workspace) as its working directory.
As a result, `harness-server` starts from `/home/agent/workspace` even though the
prepared repository is available at `/workspace`. Agents then incorrectly report that
the repository or its `README.md` is missing.

## Design

Use `CENTAUR_WORKSPACE_ROOT` as the explicit contract between the control plane and
the sandbox image:

- When a session has a prepared development workspace, the sandbox specification sets
  `CENTAUR_WORKSPACE_ROOT=/workspace` alongside the existing workspace mount and
  `working_dir`.
- The sandbox entrypoint uses `CENTAUR_WORKSPACE_ROOT` as `WORKSPACE_DIR` when it is
  set. It writes the composed runtime prompt there and changes into that directory
  before starting `harness-server`.
- When the variable is absent, the entrypoint preserves the existing behavior for
  persistent-state sandboxes and legacy `AGENT_REPO` sandboxes.

The control plane remains responsible for deciding which workspace is mounted. The
image only consumes the explicit path and does not infer workspace ownership from the
current process directory or inspect Kubernetes-specific state.

## Error Handling

The existing entrypoint behavior remains fail-fast: if the selected root cannot be
created or entered, sandbox startup fails instead of starting a harness against an
unrelated empty directory. No repository credentials or clone URLs are added to the
new environment contract.

## Testing

Regression coverage will verify:

1. A development workspace sandbox spec contains both the `/workspace` mount and
   `CENTAUR_WORKSPACE_ROOT=/workspace`.
2. The entrypoint selects the explicit workspace root when provided.
3. Existing persistent-state and legacy fallback selection remains unchanged when the
   variable is absent.
4. A rebuilt and locally deployed sandbox starts its Codex thread from `/workspace` and
   a real Feishu development session can read the selected repository's `README.md`
   without the user supplying an absolute path.

## Scope

This change fixes workspace selection only. It does not change GitLab authentication,
repository provisioning, project selection, changeset collection, or chat rendering.
