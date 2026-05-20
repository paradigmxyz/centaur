"""Tests for Slack egress sanitization in slackbot_client."""

from __future__ import annotations

import pytest

from api import slackbot_client


@pytest.fixture
def posted(monkeypatch):
    calls: list[tuple[str, dict]] = []

    async def fake_post(path: str, body: dict, **_kwargs):
        calls.append((path, body))
        return {"ok": True}

    monkeypatch.setattr(slackbot_client, "post", fake_post)
    return calls


@pytest.mark.asyncio
async def test_session_text_sanitizes_markdown(posted):
    await slackbot_client.session_text(
        "sess",
        'Done. {"kind":"Status","status":"Failure","reason":"AlreadyExists"} Codex thread `019e3c91-4030-7910`',
    )

    body = posted[0][1]
    assert body["markdown"] == "Done. [k8s status omitted]"


@pytest.mark.asyncio
async def test_session_step_sanitizes_visible_fields(posted):
    await slackbot_client.session_step(
        "sess",
        step_id="s1",
        title="Run command: curl (28): Operation timed out",
        details='{"error_type":"InternalServerError","detail":"bad gateway"}',
        output="Execution: `exe_e77594af2e0b4893`",
    )

    body = posted[0][1]
    assert body["title"] == "Run command: transport_error(28)"
    assert body["details"] == "[tool error omitted]"
    assert body["output"] == "[execution id omitted]"


@pytest.mark.asyncio
async def test_harness_event_sanitizes_nested_text_fields(posted):
    await slackbot_client.harness_event(
        "sess",
        {
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "text",
                        "text": "Body. Codex thread `019e3c91-4030-7910`, with interactive elements",
                    }
                ]
            },
            "metadata": {"thread_id": "019e3c91-4030-7910"},
        },
    )

    event = posted[0][1]["event"]
    assert event["message"]["content"][0]["text"] == "Body."
    assert event["metadata"]["thread_id"] == "019e3c91-4030-7910"
