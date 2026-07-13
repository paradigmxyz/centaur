---
title: Using an overlay
description: Package and mount organization-specific Centaur tools, workflows, skills, personas, and prompts without forking the base repo.
---

# Using an overlay

Use an overlay when your deployment needs organization-specific tools,
workflows, skills, personas, prompts, or sandbox files without turning the base
Centaur repo into a fork.

An overlay is a separate Git repo listed in Helm values under
`overlays.sources`. The repo-cache DaemonSet checks out each repo on every node;
the API pod reads those checkouts from `/var/lib/centaur/repos`, and sandbox
pods read the same revisions from `/home/agent/github`.

Later overlay sources shadow earlier ones when a tool, workflow, or skill name
collides. This lets the base Centaur repo stay generic while each deployment
layers in reviewed organization behavior.

## Overlay layout

```text
centaur-overlay/
├── tools/
│   └── warehouse/
│       ├── client.py
│       └── pyproject.toml
├── workflows/
│   └── nightly_report.py
├── .agents/
│   └── skills/
│       └── incident-response/
│           └── SKILL.md
└── services/
    └── sandbox/
        └── SYSTEM_PROMPT.md
```

Only include the directories your deployment needs.

## Configure ordered sources

Declare every repo that contributes runtime extension points:

```yaml
overlays:
  sources:
    - repo: paradigmxyz/centaur
      ref: main
      visibility: public

    - repo: your-org/centaur-overlay
      ref: main
      visibility: private
```

`repo` is `owner/name` on GitHub. `ref` can be a branch, tag, or commit SHA;
omit it, set it to `""`, or set it to `main` to track the repo's default
branch. Pinning a SHA is recommended when you need a fully reproducible
production rollout, but many overlay repos intentionally track `main` so a
reviewed merge is enough for new sandboxes to pick up the change after
repo-cache refreshes.

`visibility` controls which sandboxes may receive the repo-cache checkout.
It defaults to `private`. Set `visibility: public` only for repos whose full
contents are safe to expose to principals configured with
`sandbox_repo_cache=public`; invalid or missing values are treated as `private`.

Each source defaults to the conventional layout — `toolsSubdir: tools`,
`workflowsSubdir: workflows`, `skillsSubdir: .agents/skills` — and directories
a repo does not contain are skipped at runtime, so a skills-only overlay needs
no extra configuration. Set a subdir to a non-default path to relocate it, or
to `""` to explicitly disable that surface for a source:

```yaml
    - repo: your-org/workflows-only
      ref: main
      workflowsSubdir: flows
      toolsSubdir: ""
      skillsSubdir: ""
```

For compatibility, when `overlays.sources` is empty the chart maps
`toolServer.repo`, `toolServer.ref`, `toolServer.subdir`, and
`toolServer.extraSources[]` into the same ordered overlay list.

## Mount paths

Repo-cache-backed overlays appear under different prefixes depending on where
you are debugging:

| Runtime | Mount | Used for |
|---------|-------|----------|
| API | `/var/lib/centaur/repos/<owner>/<repo>` | Tool-secret discovery and workflow discovery. |
| Sandbox | `/home/agent/github/<owner>/<repo>` | Workflow-host execution, skills, persona files, prompt fragments, and runtime files available to agents. |

Do not use the sandbox path when debugging API discovery. If a tool or workflow
is missing from API discovery, inspect `/var/lib/centaur/repos/...` in the API
container. If a skill or workflow-host import is missing, inspect
`/home/agent/github/...` in the sandbox.

## Discovery paths

The chart renders API discovery paths from the ordered overlay list:

```text
TOOL_DIRS=/var/lib/centaur/repos/paradigmxyz/centaur/tools:/var/lib/centaur/repos/your-org/centaur-overlay/tools
WORKFLOW_DIRS=/var/lib/centaur/repos/paradigmxyz/centaur/workflows:/var/lib/centaur/repos/your-org/centaur-overlay/workflows
```

The same ordered workflow list is translated for workflow-host sandboxes:

```text
WORKFLOW_DIRS=/home/agent/github/paradigmxyz/centaur/workflows:/home/agent/github/your-org/centaur-overlay/workflows
```

Agent sandboxes receive overlay skills through:

```text
CENTAUR_SKILL_DIRS=/home/agent/github/paradigmxyz/centaur/.agents/skills:/home/agent/github/your-org/centaur-overlay/.agents/skills
```

The sandbox entrypoint copies each existing directory from `CENTAUR_SKILL_DIRS`
into the agent workspace in order, so later overlay skill directories can
replace earlier skill names.

## Prompt overlays

For small prompt additions, keep using the chart-level escape hatch:

```yaml
overlay:
  systemPrompt: |
    Add deployment-specific agent guidance here.
```

For larger prompt/persona sets, keep files in an overlay repo and expose their
paths through `overlays.sources` as that surface is wired into your deployment.
Do not rely on `overlay.image.*`; repo-cache-backed overlays are the default
delivery path.

## Verify the overlay

Verify the API pod sees the ordered API-side paths:

```bash
kubectl exec -n centaur deploy/centaur-centaur-api-rs -- sh -lc '
  echo "$TOOL_DIRS"
  echo "$WORKFLOW_DIRS"
  for d in ${TOOL_DIRS//:/ }; do test -d "$d" && find "$d" -maxdepth 1 -mindepth 1 -type d | sort; done
  for d in ${WORKFLOW_DIRS//:/ }; do test -d "$d" && find "$d" -maxdepth 1 -name "*.py" | sort; done
'
```

Verify an agent sandbox sees merged tools and copied skills:

```bash
kubectl exec -n centaur <agent-sandbox-pod> -- sh -lc '
  echo "$TOOL_DIRS"
  echo "$CENTAUR_SKILL_DIRS"
  ls -la /app/tools
  find /workspace/.agents/skills -maxdepth 2 -type f -name SKILL.md | sort
'
```

Verify a workflow-host sandbox sees the sandbox-translated workflow list:

```bash
kubectl exec -n centaur <workflow-host-pod> -- sh -lc '
  echo "$WORKFLOW_DIRS"
  for d in ${WORKFLOW_DIRS//:/ }; do test -d "$d" && find "$d" -maxdepth 1 -name "*.py" | sort; done
'
```

If something is missing, check the configured repo/ref, repo-cache readiness,
the rendered env vars, and the API or sandbox mount prefix relevant to the
extension type.
