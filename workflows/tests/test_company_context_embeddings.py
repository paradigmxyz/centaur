from __future__ import annotations

import asyncio
import importlib
import sys
import types
from pathlib import Path

sys.path.insert(
    0,
    str(Path(__file__).resolve().parents[2] / "services" / "workflow-python"),
)


def _load():
    return importlib.import_module("workflows.company_context_embeddings")


class FakePool:
    def __init__(self, rows):
        self.rows = rows
        self.fetch_call = None
        self.executemany_calls = []

    async def fetch(self, query, *args):
        self.fetch_call = (query, args)
        return self.rows

    async def executemany(self, query, values):
        self.executemany_calls.append((query, values))


class FakeEmbeddings:
    def __init__(self, data=None):
        self.call = None
        self.data = data or [
            types.SimpleNamespace(index=1, embedding=[0.3, 0.4]),
            types.SimpleNamespace(index=0, embedding=[0.1, 0.2]),
        ]

    async def create(self, **kwargs):
        self.call = kwargs
        return types.SimpleNamespace(data=self.data)


def test_handler_embeds_and_stores_one_batch(monkeypatch):
    embeddings = _load()
    monkeypatch.setenv("COMPANY_CONTEXT_EMBEDDINGS_ENABLED", "true")
    rows = [
        {
            "source_kind": "company_context",
            "document_id": "doc-1",
            "title": "First",
            "body": "First body",
            "content_hash": "hash-1",
        },
        {
            "source_kind": "google_docs",
            "document_id": "doc-2",
            "title": "Second",
            "body": "Second body",
            "content_hash": "hash-2",
        },
    ]
    pool = FakePool(rows)
    fake_embeddings = FakeEmbeddings()
    monkeypatch.setattr(
        embeddings,
        "_client",
        lambda: types.SimpleNamespace(embeddings=fake_embeddings),
    )
    logs = []
    starts = []

    async def start_workflow(workflow_name, workflow_input, *, idempotency_key):
        starts.append((workflow_name, workflow_input, idempotency_key))
        return {"run_id": "run-2", "task_id": "task-2"}

    context = types.SimpleNamespace(
        run_id="run-1",
        _pool=pool,
        log=lambda event, **fields: logs.append((event, fields)),
        start_workflow=start_workflow,
    )

    result = asyncio.run(
        embeddings.handler(
            embeddings.Input(batch_size=2, max_input_chars=100),
            context,
        )
    )

    assert result == {
        "status": "completed",
        "embedded": 2,
        "model": "text-embedding-3-small",
        "requeued": True,
        "next_run": {"run_id": "run-2", "task_id": "task-2"},
    }
    assert pool.fetch_call is not None
    assert "e.embedding_id IS NULL" in pool.fetch_call[0]
    assert "e.content_hash IS DISTINCT FROM d.content_hash" in pool.fetch_call[0]
    assert "FROM google_docs_context_documents" in pool.fetch_call[0]
    assert "FROM granola_context_documents" in pool.fetch_call[0]
    assert pool.fetch_call[1] == ("text-embedding-3-small", 2)
    assert fake_embeddings.call == {
        "model": "text-embedding-3-small",
        "input": ["First\n\nFirst body", "Second\n\nSecond body"],
        "dimensions": 1536,
        "encoding_format": "float",
    }
    assert len(pool.executemany_calls) == 2
    assert "company_context_document_id" in pool.executemany_calls[0][0]
    assert pool.executemany_calls[0][1] == [
        ("doc-1", "text-embedding-3-small", "hash-1", "[0.1,0.2]")
    ]
    assert "google_docs_context_document_id" in pool.executemany_calls[1][0]
    assert pool.executemany_calls[1][1] == [
        ("doc-2", "text-embedding-3-small", "hash-2", "[0.3,0.4]")
    ]
    assert starts == [
        (
            "company_context_embeddings",
            {
                "batch_size": 2,
                "model": "text-embedding-3-small",
                "max_input_chars": 100,
                "metadata": {"source": "company_context_embeddings_requeue"},
            },
            "company_context_embeddings:run-1:next",
        )
    ]
    assert logs[-1][0] == "company_context_embeddings_completed"


def test_openai_batches_stay_below_the_aggregate_token_limit():
    embeddings = _load()

    batches = embeddings._batches(list(range(250)), embeddings.OPENAI_BATCH_SIZE)

    assert [len(batch) for batch in batches] == [25] * 10
    assert (
        embeddings.OPENAI_BATCH_SIZE * embeddings.OPENAI_MAX_INPUT_TOKENS
        < embeddings.OPENAI_MAX_BATCH_TOKENS
    )


def test_handler_does_not_call_openai_when_batch_is_empty(monkeypatch):
    embeddings = _load()
    monkeypatch.setenv("COMPANY_CONTEXT_EMBEDDINGS_ENABLED", "true")
    pool = FakePool([])
    monkeypatch.setattr(
        embeddings,
        "_client",
        lambda: (_ for _ in ()).throw(AssertionError("client should not be created")),
    )
    context = types.SimpleNamespace(_pool=pool, log=lambda *_args, **_kwargs: None)

    result = asyncio.run(embeddings.handler(embeddings.Input(), context))

    assert result == {
        "status": "completed",
        "embedded": 0,
        "model": "text-embedding-3-small",
        "requeued": False,
    }
    assert pool.executemany_calls == []


def test_handler_does_not_requeue_a_partial_batch(monkeypatch):
    embeddings = _load()
    monkeypatch.setenv("COMPANY_CONTEXT_EMBEDDINGS_ENABLED", "true")
    pool = FakePool(
        [
            {
                "source_kind": "granola",
                "document_id": "doc-1",
                "title": "Only document",
                "body": "Body",
                "content_hash": "hash-1",
            }
        ]
    )
    fake_embeddings = FakeEmbeddings(
        [types.SimpleNamespace(index=0, embedding=[0.1, 0.2])]
    )
    monkeypatch.setattr(
        embeddings,
        "_client",
        lambda: types.SimpleNamespace(embeddings=fake_embeddings),
    )

    async def unexpected_start(*_args, **_kwargs):
        raise AssertionError("partial batches should not requeue")

    context = types.SimpleNamespace(
        run_id="run-1",
        _pool=pool,
        log=lambda *_args, **_kwargs: None,
        start_workflow=unexpected_start,
    )

    result = asyncio.run(embeddings.handler(embeddings.Input(batch_size=2), context))

    assert result == {
        "status": "completed",
        "embedded": 1,
        "model": "text-embedding-3-small",
        "requeued": False,
    }


def test_handler_is_disabled_by_default(monkeypatch):
    embeddings = _load()
    monkeypatch.delenv("COMPANY_CONTEXT_EMBEDDINGS_ENABLED", raising=False)
    context = types.SimpleNamespace(
        _pool=object(),
        log=lambda *_args, **_kwargs: None,
    )

    result = asyncio.run(embeddings.handler(embeddings.Input(), context))

    assert result == {
        "status": "skipped",
        "reason": "company_context_embeddings_disabled",
    }
