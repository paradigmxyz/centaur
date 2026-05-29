from __future__ import annotations

import uuid

import pytest

from workflows.github_issue_triage import handler


class FakeContext:
    def __init__(self) -> None:
        self.agent_turn_calls = []

    async def agent_turn(self, prompt, **kwargs):
        self.agent_turn_calls.append((prompt, kwargs))
        return {"result_text": "comment posted"}


def _input(*, event: str = "issues", action: str = "opened") -> dict:
    return {
        "webhook": {
            "headers": {
                "x-github-event": event,
                "x-github-delivery": f"delivery-{uuid.uuid4().hex}",
            },
            "body": {
                "action": action,
                "repository": {
                    "full_name": "acme/widgets",
                    "name": "widgets",
                    "default_branch": "main",
                    "owner": {"login": "acme"},
                },
                "issue": {
                    "number": 42,
                    "title": "Widget crashes on startup",
                    "body": "The app exits immediately after launch.",
                    "html_url": "https://github.com/acme/widgets/issues/42",
                    "url": "https://api.github.com/repos/acme/widgets/issues/42",
                    "user": {"login": "octo-user"},
                    "labels": [{"name": "bug"}],
                },
            },
        },
    }


@pytest.mark.asyncio
async def test_github_issue_triage_dispatches_agent_for_opened_issue():
    ctx = FakeContext()

    result = await handler(_input(), ctx)

    assert result["triaged"] is True
    assert result["repository"] == "acme/widgets"
    assert result["issue_number"] == 42
    assert len(ctx.agent_turn_calls) == 1
    prompt, kwargs = ctx.agent_turn_calls[0]
    assert "Widget crashes on startup" in prompt
    assert "https://api.github.com/repos/acme/widgets/issues/42/comments" in prompt
    assert kwargs["thread_key"] == "github:acme/widgets:42"
    assert kwargs["metadata"]["github_repository"] == "acme/widgets"


@pytest.mark.asyncio
async def test_github_issue_triage_skips_non_issue_event():
    ctx = FakeContext()

    result = await handler(_input(event="ping"), ctx)

    assert result == {
        "skipped": True,
        "reason": "unsupported_github_event",
        "event_type": "ping",
    }
    assert ctx.agent_turn_calls == []


@pytest.mark.asyncio
async def test_github_issue_triage_skips_unsupported_issue_action():
    ctx = FakeContext()

    result = await handler(_input(action="edited"), ctx)

    assert result == {
        "skipped": True,
        "reason": "unsupported_issue_action",
        "action": "edited",
    }
    assert ctx.agent_turn_calls == []
