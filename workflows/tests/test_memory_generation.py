from __future__ import annotations

import asyncio
import importlib
import sys
import types
from pathlib import Path

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


def test_schedule_is_opt_in(monkeypatch):
    monkeypatch.delenv("MEMORY_GENERATION_ENABLED", raising=False)
    memory = _load()

    assert memory.SCHEDULE["enabled"] is False
    assert memory.SCHEDULE["interval_seconds"] == 900
    assert memory.SCHEDULE["no_delivery"] is True


def test_uses_v1_memory_prompt():
    memory = _load()
    normalized = " ".join(memory.SYSTEM_PROMPT.split())

    assert memory.DEFAULT_GENERATION_MODEL == "gpt-5.6-luna"
    assert "Generate zero or more memories" in normalized
    assert "weeks or months later" in normalized
    assert "ordinary inventories" in normalized
    assert "generate only the latest state" in normalized


def test_owner_uses_user_scope_for_verified_dm():
    memory = _load()

    owner = memory._owner_for_thread(_thread())

    assert owner == memory.MemoryOwner("user", "U1")


def test_owner_uses_channel_scope_and_skips_group_dm():
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
    assert (
        memory._owner_for_thread(
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

    async def step(self, name, fn):
        self.steps.append(name)
        return await fn()

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
    assert result["processed"] == 2
    assert result["embedding_failed"] == 2
