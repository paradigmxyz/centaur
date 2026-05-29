"""Example workflow: triage new GitHub issues from a webhook.

Configure a GitHub repository webhook to POST issue events to:

    /api/webhooks/github-issue-triage

Required GitHub webhook settings:

    Content type: application/json preferred; GitHub's default form payloads
    are also accepted.
    Secret: value stored as GITHUB_WEBHOOK_SECRET in the API environment
    Events: Issues

The workflow verifies GitHub's HMAC signature at the API edge, creates a
durable workflow run, then asks an agent sandbox to post one comment on the
issue with a concrete initial diagnosis and possible fix.
"""

from __future__ import annotations

import json
from typing import Any

from api.workflow_engine import WorkflowContext

WORKFLOW_NAME = "github_issue_triage"

WEBHOOKS = [
    {
        "slug": "github-issue-triage",
        "provider": "github",
        "auth": {"type": "github", "secret_ref": "GITHUB_WEBHOOK_SECRET"},
        "trigger_key": {"type": "header", "header": "X-GitHub-Delivery"},
        "allowed_methods": ["POST"],
        "allowed_content_types": ["application/json", "application/x-www-form-urlencoded"],
    }
]

_SUPPORTED_ACTIONS = {"opened", "reopened"}


def _webhook_payload(inp: dict[str, Any]) -> dict[str, Any]:
    webhook = inp.get("webhook")
    if not isinstance(webhook, dict):
        return {}
    body = webhook.get("body")
    return body if isinstance(body, dict) else {}


def _headers(inp: dict[str, Any]) -> dict[str, str]:
    webhook = inp.get("webhook")
    if not isinstance(webhook, dict):
        return {}
    headers = webhook.get("headers")
    return headers if isinstance(headers, dict) else {}


def _issue_summary(payload: dict[str, Any]) -> dict[str, Any]:
    issue = payload.get("issue") if isinstance(payload.get("issue"), dict) else {}
    repo = payload.get("repository") if isinstance(payload.get("repository"), dict) else {}
    owner = repo.get("owner") if isinstance(repo.get("owner"), dict) else {}
    labels = issue.get("labels") if isinstance(issue.get("labels"), list) else []
    return {
        "repo_full_name": repo.get("full_name") or "",
        "repo_default_branch": repo.get("default_branch") or "",
        "repo_description": repo.get("description") or "",
        "owner": owner.get("login") or "",
        "repo": repo.get("name") or "",
        "issue_number": issue.get("number"),
        "issue_title": issue.get("title") or "",
        "issue_body": issue.get("body") or "",
        "issue_url": issue.get("html_url") or "",
        "issue_api_url": issue.get("url") or "",
        "author": (issue.get("user") or {}).get("login") if isinstance(issue.get("user"), dict) else "",
        "labels": [
            label.get("name")
            for label in labels
            if isinstance(label, dict) and label.get("name")
        ],
    }


def _agent_prompt(summary: dict[str, Any]) -> str:
    repo_full_name = summary["repo_full_name"]
    issue_number = summary["issue_number"]
    issue_json = json.dumps(summary, indent=2, sort_keys=True)
    return f"""You are triaging a newly opened GitHub issue.

Repository: {repo_full_name}
Issue number: {issue_number}

Issue payload summary:

```json
{issue_json}
```

Your task:
1. Inspect the repository enough to form a plausible first diagnosis.
2. Identify the most likely relevant files, commands, or code paths.
3. Post exactly one GitHub issue comment with:
   - a short acknowledgement
   - the likely cause or first hypothesis
   - concrete next steps or a possible fix
   - any command or file references that would help a maintainer

Constraints:
- Do not close, label, assign, or edit the issue.
- Do not post more than one comment.
- Keep the comment concise and useful; avoid generic filler.
- If you cannot access the repository or GitHub API, return a clear failure
  explaining what blocked you.

You may use `gh` or `curl` to post the comment to:
https://api.github.com/repos/{repo_full_name}/issues/{issue_number}/comments
"""


async def handler(inp: dict[str, Any], ctx: WorkflowContext) -> dict[str, Any]:
    headers = _headers(inp)
    event_type = headers.get("x-github-event", "")
    payload = _webhook_payload(inp)
    action = str(payload.get("action") or "")
    if event_type != "issues":
        return {"skipped": True, "reason": "unsupported_github_event", "event_type": event_type}
    if action not in _SUPPORTED_ACTIONS:
        return {"skipped": True, "reason": "unsupported_issue_action", "action": action}

    summary = _issue_summary(payload)
    repo_full_name = str(summary.get("repo_full_name") or "")
    issue_number = summary.get("issue_number")
    if not repo_full_name or not issue_number:
        return {"skipped": True, "reason": "missing_repository_or_issue"}

    result = await ctx.agent_turn(
        _agent_prompt(summary),
        thread_key=f"github:{repo_full_name}:{issue_number}",
        message_id=f"github:{repo_full_name}:{issue_number}:{headers.get('x-github-delivery', '')}",
        metadata={
            "source": "github_webhook",
            "github_event": event_type,
            "github_action": action,
            "github_repository": repo_full_name,
            "github_issue_number": issue_number,
            "github_issue_url": summary.get("issue_url"),
        },
    )

    return {
        "triaged": True,
        "repository": repo_full_name,
        "issue_number": issue_number,
        "issue_url": summary.get("issue_url"),
        "agent_result": result,
    }
