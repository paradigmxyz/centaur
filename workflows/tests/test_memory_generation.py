from __future__ import annotations

import asyncio
import importlib
import json
import sys
import types
from pathlib import Path

import pytest

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPOSITORY_ROOT))
sys.path.insert(0, str(REPOSITORY_ROOT / "services" / "workflow-python"))


def _load():
    api_module = sys.modules.get("api")
    if api_module is not None and not hasattr(api_module, "__path__"):
        for module_name in (
            "api.metrics",
            "api.runtime_control",
            "api.workflow_engine",
            "api",
        ):
            sys.modules.pop(module_name, None)
    return importlib.import_module("workflows.memory_generation")


def _thread(**overrides):
    value = {
        "execution_id": "exe-1",
        "thread_key": "slack:T1:D1:123.456",
        "session_metadata": {
            "platform": "slack",
            "source": "slackbotv2",
            "slack_channel_id": "D1",
            "slack_team_id": "T1",
            "slack_home_team_id": "T1",
            "slack_user_id": "U1",
        },
        "iron_control_principal": "prn-1",
        "conversation_type": "im",
    }
    value.update(overrides)
    return value


def test_workflow_is_manual_and_batches_250_threads():
    memory = _load()

    assert not hasattr(memory, "SCHEDULE")
    assert memory.DEFAULT_GENERATION_BATCH_SIZE == 250


def test_uses_v1_memory_prompt():
    memory = _load()
    normalized = " ".join(memory.SYSTEM_PROMPT.split())

    assert memory.DEFAULT_GENERATION_MODEL == "gpt-5.6-luna"
    assert "Generate zero or more memories" in normalized
    assert "weeks or months later" in normalized
    assert "ordinary inventories" in normalized
    assert "generate only the latest state" in normalized


def test_generation_input_is_bounded_and_preserves_recent_user_context():
    memory = _load()
    material = {
        "executions": [
            {
                "source_execution_id": "exe-1",
                "creator_user_id": "U1",
                "assistant_final": "a" * 1_000,
            }
        ],
        "preceding_user_messages": [
            {"message_id": "old", "text": "b" * 1_000},
            {"message_id": "recent", "text": "keep this recent context"},
        ],
    }

    encoded = memory._generation_input(material, max_chars=512)
    payload = json.loads(encoded)

    assert len(encoded) <= 512
    assert payload["preceding_user_messages"][-1]["text"] == (
        "keep this recent context"
    )
    assert len(payload["executions"][0]["assistant_final"]) < 1_000
    assert material["executions"][0]["assistant_final"] == "a" * 1_000


def test_owner_uses_user_scope_for_verified_dm():
    memory = _load()

    owner = memory._owner_for_thread(_thread())

    assert owner == memory.MemoryOwner("user", "U1")


def test_owner_uses_channel_scope_for_public_and_synced_private_channels():
    memory = _load()
    channel_thread = _thread(
        thread_key="slack:T1:C1:123.456",
        session_metadata={
            "platform": "slack",
            "source": "slackbotv2",
            "slack_channel_id": "C1",
            "slack_team_id": "T_EXTERNAL",
            "slack_home_team_id": "T1",
            "slack_user_id": "U1",
        },
        conversation_type="",
    )

    assert memory._owner_for_thread(channel_thread) == memory.MemoryOwner(
        "channel", "C1"
    )
    assert memory._owner_for_thread(
        _thread(
            thread_key="slack:T1:G1:123.456",
            session_metadata={
                "platform": "slack",
                "source": "slackbotv2",
                "slack_channel_id": "G1",
                "slack_team_id": "T1",
                "slack_home_team_id": "T1",
                "slack_user_id": "U1",
            },
            conversation_type="private_channel",
        )
    ) == memory.MemoryOwner("channel", "G1")


def test_owner_skips_group_dm_and_unknown_g_conversation():
    memory = _load()
    group_thread = _thread(
        thread_key="slack:T1:G1:123.456",
        session_metadata={
            "platform": "slack",
            "source": "slackbotv2",
            "slack_channel_id": "G1",
            "slack_team_id": "T1",
            "slack_home_team_id": "T1",
            "slack_user_id": "U1",
        },
    )

    assert (
        memory._owner_for_thread({**group_thread, "conversation_type": "mpim"}) is None
    )
    assert memory._owner_for_thread({**group_thread, "conversation_type": ""}) is None

    assert (
        memory._owner_for_thread(
            _thread(
                thread_key="slack:T1:C1:123.456",
                session_metadata={
                    "platform": "slack",
                    "source": "slackbotv2",
                    "slack_channel_id": "C1",
                    "slack_team_id": "T1",
                    "slack_home_team_id": "T1",
                    "slack_user_id": "U1",
                },
                conversation_type="mpim",
            )
        )
        is None
    )


def test_owner_rejects_unverified_or_mismatched_session():
    memory = _load()

    assert memory._owner_for_thread(_thread(iron_control_principal=None)) is None
    assert (
        memory._owner_for_thread(_thread(thread_key="slack:T1:D_OTHER:123.456")) is None
    )


def test_candidate_validation_rejects_oversized_content():
    memory = _load()
    execution = {"creator_user_id": "U1"}
    common = {
        "content": "Use short status updates in project channels.",
        "source_execution_id": "exe-1",
    }

    accepted, reason = memory._validate_candidate(
        common,
        executions_by_id={"exe-1": execution},
        seen_hashes=set(),
    )
    assert reason == "accepted"
    assert accepted["source_execution_id"] == "exe-1"
    assert accepted["creator_user_id"] == "U1"

    rejected, reason = memory._validate_candidate(
        {**common, "content": "x" * 1_501},
        executions_by_id={"exe-1": execution},
        seen_hashes=set(),
    )
    assert rejected is None
    assert reason == "invalid_length"

    rejected, reason = memory._validate_candidate(
        common,
        executions_by_id={"exe-1": {}},
        seen_hashes=set(),
    )
    assert rejected is None
    assert reason == "invalid_source"


def test_candidate_validation_rejects_duplicate_content():
    memory = _load()
    content = "Use short status updates."

    candidate, reason = memory._validate_candidate(
        {
            "content": content,
            "source_execution_id": "exe-1",
        },
        executions_by_id={"exe-1": {"creator_user_id": "U1"}},
        seen_hashes={memory._content_hash(content)},
    )

    assert candidate is None
    assert reason == "duplicate"


class FakeEmbeddingPool:
    def __init__(self, rows):
        self.rows = rows
        self.fetch_query = ""
        self.execute_calls = []

    async def fetch(self, query, *_args):
        self.fetch_query = query
        return self.rows

    async def execute(self, query, *args):
        self.execute_calls.append((query, args))
        return "UPDATE 1"


def test_pending_embeddings_are_selected_only_by_null_embedding(monkeypatch):
    memory = _load()
    pool = FakeEmbeddingPool([])
    monkeypatch.setattr(memory, "increment_metric", lambda *_args, **_kwargs: None)

    result = asyncio.run(
        memory._embed_pending(
            pool,
            batch_size=25,
            model="text-embedding-3-small",
            client=types.SimpleNamespace(),
            ctx=types.SimpleNamespace(log=lambda *_args, **_kwargs: None),
        )
    )

    assert result == {"embedded": 0, "embedding_failed": 0}
    assert "embedding IS NULL" in pool.fetch_query
    assert "embedding_status" not in pool.fetch_query


def test_embedding_failure_leaves_memory_null_for_next_run(monkeypatch):
    memory = _load()
    pool = FakeEmbeddingPool(
        [
            {
                "id": "00000000-0000-0000-0000-000000000001",
                "content": "Fact",
                "content_hash": "h1",
            }
        ]
    )

    class BrokenEmbeddings:
        async def create(self, **_kwargs):
            raise RuntimeError("temporary upstream failure")

    monkeypatch.setattr(memory, "increment_metric", lambda *_args, **_kwargs: None)
    result = asyncio.run(
        memory._embed_pending(
            pool,
            batch_size=25,
            model="text-embedding-3-small",
            client=types.SimpleNamespace(embeddings=BrokenEmbeddings()),
            ctx=types.SimpleNamespace(log=lambda *_args, **_kwargs: None),
        )
    )

    assert result == {"embedded": 0, "embedding_failed": 1}
    assert pool.execute_calls == []


def test_successful_embedding_updates_only_a_null_row(monkeypatch):
    memory = _load()
    row = {
        "id": "00000000-0000-0000-0000-000000000001",
        "content": "Fact",
        "content_hash": "h1",
    }
    pool = FakeEmbeddingPool([row])

    class FakeEmbeddings:
        async def create(self, **_kwargs):
            return types.SimpleNamespace(
                data=[types.SimpleNamespace(index=0, embedding=[0.1, 0.2])]
            )

    monkeypatch.setattr(memory, "increment_metric", lambda *_args, **_kwargs: None)

    result = asyncio.run(
        memory._embed_pending(
            pool,
            batch_size=25,
            model="text-embedding-3-small",
            client=types.SimpleNamespace(embeddings=FakeEmbeddings()),
            ctx=types.SimpleNamespace(log=lambda *_args, **_kwargs: None),
        )
    )

    assert result == {"embedded": 1, "embedding_failed": 0}
    query, args = pool.execute_calls[0]
    assert "id = $1::uuid" in query
    assert "embedding IS NULL" in query
    assert args == (
        row["id"],
        "[0.1,0.2]",
        "text-embedding-3-small",
    )


class ImmediateContext:
    def __init__(self, pool):
        self._pool = pool
        self.run_id = "run-1"
        self.steps = []
        self.started = []

    async def step(self, name, fn):
        self.steps.append(name)
        return await fn()

    async def start_workflow(
        self, workflow_name, workflow_input, *, idempotency_key=None
    ):
        self.started.append((workflow_name, workflow_input, idempotency_key))
        return {"run_id": "run-next", "task_id": "task-next"}

    def log(self, *_args, **_kwargs):
        return None


def test_handler_groups_executions_by_thread_and_keeps_embedding_pass_independent(
    monkeypatch,
):
    memory = _load()
    executions = [_thread(execution_id="exe-1"), _thread(execution_id="exe-2")]
    process_calls = []

    async def load_executions(*_args, **_kwargs):
        return executions

    async def process_thread(*_args, **kwargs):
        process_calls.append(kwargs["executions"])
        return {"created": 1, "rejected": 0, "skipped": 0}

    async def advance_cursor(*_args, **_kwargs):
        return None

    async def embed_pending(*_args, **_kwargs):
        return {"embedded": 0, "embedding_failed": 2}

    async def emit_age(_pool):
        return None

    monkeypatch.setattr(memory, "_load_executions", load_executions)
    monkeypatch.setattr(memory, "_process_thread", process_thread)
    monkeypatch.setattr(memory, "_advance_cursor", advance_cursor)
    monkeypatch.setattr(memory, "_embed_pending", embed_pending)
    monkeypatch.setattr(memory, "_emit_pending_embedding_age", emit_age)
    monkeypatch.setattr(memory, "_client", lambda: types.SimpleNamespace())
    monkeypatch.setattr(memory, "increment_metric", lambda *_args, **_kwargs: None)
    context = ImmediateContext(object())

    result = asyncio.run(memory.handler({}, context))

    assert len(process_calls) == 1
    assert [execution["execution_id"] for execution in process_calls[0]] == [
        "exe-1",
        "exe-2",
    ]
    assert context.steps == [
        "load_completed_slack_executions",
        memory._thread_step_name("slack:T1:D1:123.456"),
        "advance_memory_generation_cursor",
        "embed_null_memories",
    ]
    assert result["created"] == 1
    assert result["failed"] == 0
    assert result["processed"] == 2
    assert result["threads"] == 1
    assert result["embedding_failed"] == 2
    assert result["requeued"] is False


def test_handler_advances_cursor_and_embeds_after_thread_failure(monkeypatch):
    memory = _load()
    executions = [
        _thread(execution_id="exe-bad", thread_key="slack:T1:D1:1"),
        _thread(execution_id="exe-good", thread_key="slack:T1:D2:2"),
    ]
    advanced = []
    metrics = []

    async def load_executions(*_args, **_kwargs):
        return executions

    async def process_thread(*_args, **kwargs):
        if kwargs["executions"][0]["execution_id"] == "exe-bad":
            raise memory.GenerationInputTooLargeError("poison thread")
        return {"created": 1, "rejected": 0, "skipped": 0}

    async def advance_cursor(_pool, execution_id):
        advanced.append(execution_id)

    async def embed_pending(*_args, **_kwargs):
        return {"embedded": 1, "embedding_failed": 0}

    async def emit_age(_pool):
        return None

    monkeypatch.setattr(memory, "_load_executions", load_executions)
    monkeypatch.setattr(memory, "_process_thread", process_thread)
    monkeypatch.setattr(memory, "_advance_cursor", advance_cursor)
    monkeypatch.setattr(memory, "_embed_pending", embed_pending)
    monkeypatch.setattr(memory, "_emit_pending_embedding_age", emit_age)
    monkeypatch.setattr(memory, "_client", lambda: types.SimpleNamespace())
    monkeypatch.setattr(
        memory,
        "increment_metric",
        lambda name, value, **fields: metrics.append((name, value, fields)),
    )
    context = ImmediateContext(object())

    result = asyncio.run(memory.handler({}, context))

    assert result["created"] == 1
    assert result["failed"] == 1
    assert result["embedded"] == 1
    assert advanced == ["exe-good"]
    assert metrics == [
        (
            "memory_generation_threads_failed_total",
            1,
            {"error_type": "GenerationInputTooLargeError"},
        )
    ]


def test_handler_keeps_cursor_on_transient_thread_failure(monkeypatch):
    memory = _load()
    executions = [_thread(execution_id="exe-1")]
    advanced = []
    embedded = []

    async def load_executions(*_args, **_kwargs):
        return executions

    async def process_thread(*_args, **_kwargs):
        raise RuntimeError("OpenAI unavailable")

    async def advance_cursor(_pool, execution_id):
        advanced.append(execution_id)

    async def embed_pending(*_args, **_kwargs):
        embedded.append(True)
        return {"embedded": 0, "embedding_failed": 0}

    monkeypatch.setattr(memory, "_load_executions", load_executions)
    monkeypatch.setattr(memory, "_process_thread", process_thread)
    monkeypatch.setattr(memory, "_advance_cursor", advance_cursor)
    monkeypatch.setattr(memory, "_embed_pending", embed_pending)
    monkeypatch.setattr(memory, "_client", lambda: types.SimpleNamespace())
    context = ImmediateContext(object())

    with pytest.raises(RuntimeError, match="OpenAI unavailable"):
        asyncio.run(memory.handler({}, context))

    assert advanced == []
    assert embedded == []


def test_handler_requeues_when_thread_batch_is_full(monkeypatch):
    memory = _load()
    monkeypatch.setattr(memory, "DEFAULT_GENERATION_BATCH_SIZE", 2)
    executions = [
        _thread(execution_id="exe-1", thread_key="slack:T1:D1:1"),
        _thread(execution_id="exe-2", thread_key="slack:T1:D2:2"),
    ]

    async def load_executions(*_args, **_kwargs):
        return executions

    async def process_thread(*_args, **_kwargs):
        return {"created": 0, "rejected": 0, "skipped": 0}

    async def no_op(*_args, **_kwargs):
        return None

    async def embed_pending(*_args, **_kwargs):
        return {"embedded": 0, "embedding_failed": 0}

    monkeypatch.setattr(memory, "_load_executions", load_executions)
    monkeypatch.setattr(memory, "_process_thread", process_thread)
    monkeypatch.setattr(memory, "_advance_cursor", no_op)
    monkeypatch.setattr(memory, "_embed_pending", embed_pending)
    monkeypatch.setattr(memory, "_emit_pending_embedding_age", no_op)
    monkeypatch.setattr(memory, "_client", lambda: types.SimpleNamespace())
    context = ImmediateContext(object())

    result = asyncio.run(memory.handler({}, context))

    assert result["threads"] == 2
    assert result["requeued"] is True
    assert result["next_run"] == {"run_id": "run-next", "task_id": "task-next"}
    assert context.started == [
        (
            "memory_generation",
            {"source": "memory_generation_continuation"},
            "memory_generation:run-1:next",
        )
    ]
