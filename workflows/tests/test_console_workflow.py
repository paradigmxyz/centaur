from __future__ import annotations

import asyncio

from workflows import console_workflow


class FakeContext:
    run_id = "run-123"

    def __init__(self, result_text: str = "Daily summary") -> None:
        self.result_text = result_text
        self.agent_calls = []
        self.slack_calls = []

    async def agent_turn(self, prompt, **kwargs):
        self.agent_calls.append((prompt, kwargs))
        return {"result_text": self.result_text, "execution_id": "exec-123"}

    async def post_to_slack(self, channel, text):
        self.slack_calls.append((channel, text))
        return {"channel": channel, "ts": "123.456"}


def test_handler_runs_one_scoped_agent_turn_and_delivers_its_text():
    context = FakeContext()

    result = asyncio.run(
        console_workflow.handler(
            {
                "prompt": "Summarize open incidents",
                "principal": "console-user-author",
                "channel": "C0123456789",
                "authored_workflow_id": "awf_123",
                "authored_workflow_name": "Incident summary",
            },
            context,
        )
    )

    assert len(context.agent_calls) == 1
    prompt, kwargs = context.agent_calls[0]
    assert prompt == "Summarize open incidents"
    assert kwargs["principal"] == "console-user-author"
    assert kwargs["thread_key"] == "console-workflow:awf_123:run-123"
    assert kwargs["metadata"]["authored_workflow_name"] == "Incident summary"
    assert context.slack_calls == [("C0123456789", "Daily summary")]
    assert result["delivery"]["ts"] == "123.456"


def test_handler_rejects_missing_required_input_before_starting_an_agent():
    context = FakeContext()

    try:
        asyncio.run(console_workflow.handler({}, context))
    except ValueError as error:
        assert str(error) == "console_workflow requires prompt"
    else:
        raise AssertionError("expected invalid input to fail")

    assert context.agent_calls == []
    assert context.slack_calls == []
