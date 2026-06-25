---
name: tool-health-smoke
description: "Run a focused Centaur tool health smoke test across every live tool CLI. Use when asked to smoke test tools, check all tool auth/connectivity, validate brokered credential injection, or produce a Slack-ready health report for the current deployment."
---

# Tool Health Smoke

## Overview

Use this skill for broad tool-by-tool smoke checks. It is narrower than the full `qa` skill: it only verifies that each live tool CLI is installed and that its `health` command succeeds through the normal sandbox credential path.

Do not invent one-off probes for each tool. The canonical smoke surface is:

```bash
<tool> health
```

## Workflow

1. Run the bundled runner from a sandbox or environment where `centaur-tools` and tool shims are installed:

   ```bash
   uv run .agents/skills/tool-health-smoke/scripts/run_tool_health_smoke.py
   ```

2. The runner discovers tools with `centaur-tools json`, then executes each `<tool> health` command with a bounded timeout.

3. Review the generated Slack-friendly report. The first line is the overall result:

   ```text
   Overall: PASS|FAIL|PARTIAL - <reason>
   ```

4. Return the report in the Slack thread as the assistant response. Do not call the Slack posting API unless the user explicitly asks you to post through the Slack tool.

## Report Rules

- Treat `returncode != 0`, invalid JSON, missing `ok`, or `ok: false` as a failed tool health check.
- Treat a missing `health` command as a failure. Tools are expected to expose `health`.
- Report the runner output directly. Health commands are responsible for returning compact, safe JSON.
- Keep evidence compact: include the tool name, status, and one short detail or error.
- Use Slack mrkdwn bullets, not Markdown tables.
- Include all failed rows. If all tools pass, include a compact pass list or pass count.

## Useful Options

```bash
uv run .agents/skills/tool-health-smoke/scripts/run_tool_health_smoke.py --timeout 45
uv run .agents/skills/tool-health-smoke/scripts/run_tool_health_smoke.py --concurrency 4
uv run .agents/skills/tool-health-smoke/scripts/run_tool_health_smoke.py --only slack,websearch,vlogs
uv run .agents/skills/tool-health-smoke/scripts/run_tool_health_smoke.py --json
```

Use `--json` only when you need machine-readable results for follow-up analysis. Use the default text output for Slack.

## Interpreting Results

- `PASS`: every discovered tool health command returned valid JSON with `ok: true`.
- `FAIL`: one or more tools failed, timed out, returned malformed output, or omitted the required health result.
- `PARTIAL`: discovery succeeded but no tools were found, or the run was intentionally filtered.

When a tool fails because a credential is missing, check whether the client incorrectly requires an environment variable instead of using `secret()` or proxy-injected auth.
