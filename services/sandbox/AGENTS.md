# Sandbox Guide

## Role

This directory builds the agent runtime image and composes its startup
environment. It installs harness CLIs and tool shims, prepares persistent state,
consumes repo-cache and overlay mounts supplied by the control plane, writes
harness configuration, and launches `harness-server`.

`SYSTEM_PROMPT.md` is runtime product behavior: the image bakes it into the
agent home and startup copies/composes it into the workspace. It is distinct
from repository `AGENTS.md` files, which guide contributors editing this repo.

## Invariants

- Sandboxes receive placeholder credentials only. Real secret values must not
  appear in image layers, environment dumps, generated auth files, logs, or
  tests; outbound substitution belongs to `iron-proxy`.
- Preserve the non-root runtime user and the boundary between the reusable
  toolchain stage and frequently changing final layers.
- `entrypoint.sh` must be deterministic and safe to rerun against persistent
  state. Preserve permissions, symlink targets, prompt composition order, and
  configured overlay precedence.
- Tool discovery comes from ordered `TOOL_DIRS` entries and each tool's
  `[project.scripts]`. Keep `centaur-tools` catalog, direct CLI execution, and
  workflow compatibility behavior aligned.
- Repo-cache readiness must describe the exact repositories, refs, visibility,
  and completed sync. Never report ready for stale or partial content.
- `crates/harness-server/` owns app-server protocol normalization and is built
  into this image. Keep harness-specific quirks there rather than in image
  startup code or chat clients.
- Pin runtime/tool versions deliberately. A Dockerfile version bump requires a
  startup or functional smoke test, not just a successful image build.

## Validation

From the repository root:

```bash
uv run python -m unittest discover -s services/sandbox -p 'test_*.py'
```

Use focused tests for prompt, tool-shim, and repo-cache changes. After building,
deploy to the local stack and verify from a real sandbox:

```bash
centaur-tools list
<tool> --help
```

Also run one harness turn that exercises the changed startup or protocol path.
For repo-cache changes, verify both the readiness sentinel and the checked-out
commit inside the sandbox; the existence of a directory alone is insufficient.
