from __future__ import annotations

import asyncio

from workflows import console_workflow


class FakeContext:
    run_id = "run-123"
    task_id = "task-456"

    def __init__(self, result_text: str = "Daily summary", output_lines=None) -> None:
        self.result_text = result_text
        self.output_lines = output_lines or []
        self.agent_calls = []
        self.step_calls = []
        self.step_results = {}
        self.slack_calls = []

    async def agent_turn(self, prompt, **kwargs):
        self.agent_calls.append((prompt, kwargs))
        return {
            "result_text": self.result_text,
            "output_lines": self.output_lines,
            "execution_id": "exec-123",
        }

    async def step(self, name, fn):
        self.step_calls.append(name)
        if name not in self.step_results:
            self.step_results[name] = await fn()
        return self.step_results[name]

    async def post_to_slack(self, channel, text, **kwargs):
        self.slack_calls.append((channel, text, kwargs))
        return {"channel": channel, "ts": f"123.{len(self.slack_calls)}"}


def test_handler_runs_one_scoped_agent_turn_and_delivers_its_text():
    context = FakeContext()

    result = asyncio.run(
        console_workflow.handler(
            {
                "prompt": "Summarize open incidents",
                "principal": "console-user-author",
                "channel": "C0123456789",
                "scheduled_task_id": "tsk_123",
                "scheduled_task_name": "Incident summary",
            },
            context,
        )
    )

    assert len(context.agent_calls) == 1
    prompt, kwargs = context.agent_calls[0]
    assert prompt == "Summarize open incidents"
    assert kwargs["principal"] == "console-user-author"
    assert "thread_key" not in kwargs
    assert kwargs["metadata"] == {
        "scheduled_task_id": "tsk_123",
        "scheduled_task_name": "Incident summary",
    }
    assert context.step_calls == ["post_result"]
    assert context.slack_calls == [("C0123456789", "Daily summary", {})]
    assert result["delivery"]["ts"] == "123.1"


def test_handler_truncates_long_slack_results():
    response_text = "x" * (console_workflow.SLACK_MESSAGE_MAX_LENGTH + 25)
    context = FakeContext(result_text=response_text)

    result = asyncio.run(
        console_workflow.handler(
            {
                "prompt": "Summarize open incidents",
                "principal": "console-user-author",
                "channel": "C0123456789",
                "scheduled_task_id": "tsk_123",
            },
            context,
        )
    )

    assert context.step_calls == ["post_result"]
    assert len(context.slack_calls) == 1
    assert len(context.slack_calls[0][1]) == console_workflow.SLACK_MESSAGE_MAX_LENGTH
    assert context.slack_calls[0][2] == {}
    assert result["delivery"]["ts"] == "123.1"


def test_handler_delivers_canonical_result_text_instead_of_output_lines():
    context = FakeContext(
        result_text=(
            "Cold scoops kiss the cone\n"
            "Summer sunlight melts to cream\n"
            "Sweet stars on my tongue"
        ),
        output_lines=["Commentary...", "Downloading packages...", "Traceback..."],
    )

    asyncio.run(
        console_workflow.handler(
            {
                "prompt": "Write a haiku about ice cream",
                "principal": "console-user-author",
                "channel": "C0123456789",
                "scheduled_task_id": "tsk_123",
            },
            context,
        )
    )

    assert context.slack_calls == [
        (
            "C0123456789",
            "Cold scoops kiss the cone\nSummer sunlight melts to cream\nSweet stars on my tongue",
            {},
        )
    ]


def test_handler_does_not_repeat_checkpointed_slack_posts():
    context = FakeContext()
    params = {
        "prompt": "Summarize open incidents",
        "principal": "console-user-author",
        "channel": "C0123456789",
        "scheduled_task_id": "tsk_123",
    }

    asyncio.run(console_workflow.handler(params, context))
    asyncio.run(console_workflow.handler(params, context))

    assert context.step_calls == ["post_result", "post_result"]
    assert context.slack_calls == [("C0123456789", "Daily summary", {})]


def test_handler_rejects_missing_required_input_before_starting_an_agent():
    context = FakeContext()

    try:
        asyncio.run(console_workflow.handler({}, context))
    except ValueError as error:
        assert str(error) == "console_workflow requires prompt"
    else:
        raise AssertionError("expected invalid input to fail")

    assert context.agent_calls == []
    assert context.step_calls == []
    assert context.slack_calls == []
