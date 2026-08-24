from __future__ import annotations

import asyncio

from workflows import console_workflow


class FakeContext:
    run_id = "run-123"
    task_id = "task-456"

    def __init__(
        self,
        result_text: str = "Daily summary",
        output_lines=None,
        slack_response_channel=None,
    ) -> None:
        self.result_text = result_text
        self.output_lines = output_lines or []
        self.slack_response_channel = slack_response_channel
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
        return {
            "channel": self.slack_response_channel or channel,
            "ts": f"123.{len(self.slack_calls)}",
        }


def scheduled_task_blocks(body: str, footer: str):
    blocks = []
    if body:
        blocks.append(
            {
                "type": "section",
                "text": {"type": "mrkdwn", "text": body},
            }
        )
    blocks.append(
        {
            "type": "context",
            "elements": [{"type": "mrkdwn", "text": footer}],
        }
    )
    return blocks


def test_handler_runs_one_scoped_agent_turn_and_delivers_its_text():
    context = FakeContext()
    footer = "Sent by <@U0123456789>'s scheduled task"

    result = asyncio.run(
        console_workflow.handler(
            {
                "prompt": "Summarize open incidents",
                "principal": "console-user-author",
                "channel": "C0123456789",
                "slack_user_id": "U0123456789",
                "scheduled_task_id": "tsk_123",
                "scheduled_task_name": "Incident summary",
            },
            context,
        )
    )

    assert len(context.agent_calls) == 1
    prompt, kwargs = context.agent_calls[0]
    assert prompt == (
        f"{console_workflow.SCHEDULED_TASK_EXECUTION_INSTRUCTIONS}\n\n"
        "Task to execute:\nSummarize open incidents\n\n"
        f"{console_workflow.SLACK_MRKDWN_INSTRUCTIONS}"
    )
    assert kwargs["principal"] == "console-user-author"
    assert "thread_key" not in kwargs
    assert kwargs["metadata"] == {
        "scheduled_task_id": "tsk_123",
        "scheduled_task_name": "Incident summary",
    }
    assert context.step_calls == ["post_result"]
    assert context.slack_calls == [
        (
            "C0123456789",
            f"Daily summary\n\n{footer}",
            {
                "mrkdwn": True,
                "blocks": scheduled_task_blocks("Daily summary", footer),
            },
        )
    ]
    assert result["delivery"]["ts"] == "123.1"


def test_handler_treats_recurring_language_as_an_instruction_to_execute_now():
    context = FakeContext()
    task = (
        "Each Monday, review my Google Calendar for the upcoming "
        "Monday-through-Sunday week and my recent Slack conversations."
    )

    asyncio.run(
        console_workflow.handler(
            {
                "prompt": task,
                "principal": "console-user-author",
                "channel": "C0123456789",
                "slack_user_id": "U0123456789",
                "scheduled_task_id": "tsk_123",
            },
            context,
        )
    )

    prompt, _kwargs = context.agent_calls[0]
    assert prompt.startswith(
        "This is a run of an existing scheduled task. Execute the task now.\n"
        "NEVER create or update a scheduled task"
    )
    assert f"Task to execute:\n{task}\n\n" in prompt


def test_handler_threads_and_truncates_long_channel_results():
    response_text = "x" * (console_workflow.SLACK_MESSAGE_MAX_LENGTH + 25)
    context = FakeContext(result_text=response_text)

    result = asyncio.run(
        console_workflow.handler(
            {
                "prompt": "Summarize open incidents",
                "principal": "console-user-author",
                "channel": "C0123456789",
                "slack_user_id": "U0123456789",
                "scheduled_task_id": "tsk_123",
            },
            context,
        )
    )

    expected_chunks = (
        console_workflow.SLACK_MESSAGE_MAX_LENGTH
        + console_workflow.SLACK_MESSAGE_CHUNK_MAX_LENGTH
        - 1
    ) // console_workflow.SLACK_MESSAGE_CHUNK_MAX_LENGTH
    assert context.step_calls == ["post_result"] + [
        f"post_result_reply_{index}" for index in range(1, expected_chunks)
    ]
    assert len(context.slack_calls) == expected_chunks
    footer = "Sent by <@U0123456789>'s scheduled task"
    body_limit = console_workflow.SLACK_MESSAGE_MAX_LENGTH - len(footer) - 2
    assert "".join(call[1] for call in context.slack_calls) == (
        f"{response_text[:body_limit]}\n\n{footer}"
    )
    assert all(
        len(call[1]) <= console_workflow.SLACK_MESSAGE_CHUNK_MAX_LENGTH
        for call in context.slack_calls
    )
    assert context.slack_calls[0][2] == {"mrkdwn": True}
    assert all(
        call[2] == {"mrkdwn": True, "thread_ts": "123.1"}
        for call in context.slack_calls[1:-1]
    )
    final_body = context.slack_calls[-1][1].removesuffix(f"\n\n{footer}")
    assert context.slack_calls[-1][2] == {
        "thread_ts": "123.1",
        "mrkdwn": True,
        "blocks": scheduled_task_blocks(final_body, footer),
    }
    assert result["delivery"]["ts"] == "123.1"


def test_handler_posts_long_dm_results_as_replies_to_the_first_message():
    response_text = "a" * (console_workflow.SLACK_MESSAGE_CHUNK_MAX_LENGTH * 2 + 25)
    context = FakeContext(result_text=response_text, slack_response_channel="D0123456789")
    params = {
        "prompt": "Summarize open incidents",
        "principal": "console-user-author",
        "channel": "U0123456789",
        "slack_user_id": "U0123456789",
        "scheduled_task_id": "tsk_123",
    }

    result = asyncio.run(console_workflow.handler(params, context))

    assert context.step_calls == [
        "post_result",
        "post_result_reply_1",
        "post_result_reply_2",
    ]
    assert "".join(call[1] for call in context.slack_calls) == (
        f"{response_text}\n\nSent by <@U0123456789>'s scheduled task"
    )
    assert all(
        len(call[1]) <= console_workflow.SLACK_MESSAGE_CHUNK_MAX_LENGTH
        for call in context.slack_calls
    )
    assert context.slack_calls[0] == (
        "U0123456789",
        "a" * console_workflow.SLACK_MESSAGE_CHUNK_MAX_LENGTH,
        {"mrkdwn": True},
    )
    assert all(
        call[0] == "D0123456789"
        and call[2] == {"mrkdwn": True, "thread_ts": "123.1"}
        for call in context.slack_calls[1:-1]
    )
    footer = "Sent by <@U0123456789>'s scheduled task"
    final_body = context.slack_calls[-1][1].removesuffix(f"\n\n{footer}")
    assert context.slack_calls[-1] == (
        "D0123456789",
        f"{final_body}\n\n{footer}",
        {
            "thread_ts": "123.1",
            "mrkdwn": True,
            "blocks": scheduled_task_blocks(final_body, footer),
        },
    )
    assert len(result["delivery"]["replies"]) == 2

    asyncio.run(console_workflow.handler(params, context))

    assert context.step_calls == [
        "post_result",
        "post_result_reply_1",
        "post_result_reply_2",
    ] * 2
    assert len(context.slack_calls) == 3


def test_handler_delivers_canonical_result_text_instead_of_output_lines():
    body = (
        "Cold scoops kiss the cone\n"
        "Summer sunlight melts to cream\n"
        "Sweet stars on my tongue"
    )
    footer = "Sent by <@U0123456789>'s scheduled task"
    context = FakeContext(
        result_text=body,
        output_lines=["Commentary...", "Downloading packages...", "Traceback..."],
    )

    asyncio.run(
        console_workflow.handler(
            {
                "prompt": "Write a haiku about ice cream",
                "principal": "console-user-author",
                "channel": "C0123456789",
                "slack_user_id": "U0123456789",
                "scheduled_task_id": "tsk_123",
            },
            context,
        )
    )

    assert context.slack_calls == [
        (
            "C0123456789",
            f"{body}\n\n{footer}",
            {
                "mrkdwn": True,
                "blocks": scheduled_task_blocks(body, footer),
            },
        )
    ]


def test_handler_does_not_repeat_checkpointed_slack_posts():
    context = FakeContext()
    footer = "Sent by <@U0123456789>'s scheduled task"
    params = {
        "prompt": "Summarize open incidents",
        "principal": "console-user-author",
        "channel": "C0123456789",
        "slack_user_id": "U0123456789",
        "scheduled_task_id": "tsk_123",
    }

    asyncio.run(console_workflow.handler(params, context))
    asyncio.run(console_workflow.handler(params, context))

    assert context.step_calls == ["post_result", "post_result"]
    assert context.slack_calls == [
        (
            "C0123456789",
            f"Daily summary\n\n{footer}",
            {
                "mrkdwn": True,
                "blocks": scheduled_task_blocks("Daily summary", footer),
            },
        )
    ]


def test_handler_uses_a_generic_footer_for_an_in_flight_run_without_an_author():
    context = FakeContext()
    footer = "Sent by a scheduled task"

    asyncio.run(
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

    assert context.slack_calls == [
        (
            "C0123456789",
            f"Daily summary\n\n{footer}",
            {
                "mrkdwn": True,
                "blocks": scheduled_task_blocks("Daily summary", footer),
            },
        )
    ]


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
