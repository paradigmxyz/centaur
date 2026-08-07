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
        self.fetch_args = None
        self.executemany_values = []
        self.execute_values = []
        self.closed = False

    async def fetch(self, _query, *args):
        self.fetch_args = args
        return self.rows

    async def executemany(self, _query, values):
        self.executemany_values.append(values)

    async def execute(self, _query, *values):
        self.execute_values.append(values)

    async def close(self):
        self.closed = True


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


def _use_database_pool(monkeypatch, embeddings, pool):
    monkeypatch.setenv(
        "CENTAUR_POSTGRES_DSN",
        "postgresql://workflow:secret@postgres-proxy:5432?sslmode=require",
    )
    database_urls = []

    async def create_pool(database_url, **options):
        database_urls.append((database_url, options))
        return pool

    monkeypatch.setattr(embeddings.asyncpg, "create_pool", create_pool)
    return database_urls


def test_workflow_uses_a_scoped_principal_and_ai_v2_database(monkeypatch):
    embeddings = _load()
    pool = FakePool([])
    database_urls = _use_database_pool(monkeypatch, embeddings, pool)

    asyncio.run(embeddings._create_database_pool())

    assert embeddings.WORKFLOW_PRINCIPAL is True
    assert database_urls == [
        (
            "postgresql://workflow:secret@postgres-proxy:5432/ai_v2?sslmode=require",
            {"min_size": 1, "max_size": 2},
        )
    ]


def test_handler_embeds_and_stores_one_batch(monkeypatch):
    embeddings = _load()
    monkeypatch.delenv("COMPANY_CONTEXT_EMBEDDINGS_ENABLED", raising=False)
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
    _use_database_pool(monkeypatch, embeddings, pool)
    fake_embeddings = FakeEmbeddings()
    monkeypatch.setattr(
        embeddings,
        "_client",
        lambda: types.SimpleNamespace(embeddings=fake_embeddings),
    )
    starts = []

    async def start_workflow(workflow_name, workflow_input, *, idempotency_key):
        starts.append((workflow_name, workflow_input, idempotency_key))
        return {"run_id": "run-2", "task_id": "task-2"}

    context = types.SimpleNamespace(
        run_id="run-1",
        log=lambda *_args, **_kwargs: None,
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
        "failed": 0,
        "model": "text-embedding-3-small",
        "requeued": True,
        "next_run": {"run_id": "run-2", "task_id": "task-2"},
    }
    assert pool.fetch_args == ("text-embedding-3-small", 2)
    assert pool.closed is True
    assert fake_embeddings.call == {
        "model": "text-embedding-3-small",
        "input": ["First\n\nFirst body", "Second\n\nSecond body"],
        "dimensions": 1536,
        "encoding_format": "float",
    }
    assert pool.executemany_values[0] == [
        ("doc-1", "text-embedding-3-small", "hash-1", "[0.1,0.2]")
    ]
    assert pool.executemany_values[1] == [
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


def test_embedding_text_enforces_the_character_limit():
    embeddings = _load()
    row = {"title": "密" * 24_000, "body": ""}

    text = embeddings._embedding_text(row, 24_000)

    assert text == "密" * 8_192


def test_handler_records_whitespace_only_documents_without_calling_openai(monkeypatch):
    embeddings = _load()
    pool = FakePool(
        [
            {
                "source_kind": "company_context",
                "document_id": "empty-doc",
                "title": " \t ",
                "body": "\n",
                "content_hash": "empty-hash",
            }
        ]
    )
    _use_database_pool(monkeypatch, embeddings, pool)

    class UnexpectedEmbeddings:
        async def create(self, **_kwargs):
            raise AssertionError("whitespace-only documents must not reach OpenAI")

    monkeypatch.setattr(
        embeddings,
        "_client",
        lambda: types.SimpleNamespace(embeddings=UnexpectedEmbeddings()),
    )
    context = types.SimpleNamespace(log=lambda *_args, **_kwargs: None)

    result = asyncio.run(embeddings.handler(embeddings.Input(), context))

    assert result == {
        "status": "completed",
        "embedded": 0,
        "failed": 1,
        "model": "text-embedding-3-small",
        "requeued": False,
    }
    assert pool.execute_values == [
        (
            "empty-doc",
            "text-embedding-3-small",
            "empty-hash",
            "empty_input: document contains no non-whitespace text",
        )
    ]


def test_handler_isolates_and_records_a_rejected_document(monkeypatch):
    embeddings = _load()
    rows = [
        {
            "source_kind": "company_context",
            "document_id": "good-doc",
            "title": "Good",
            "body": "Body",
            "content_hash": "good-hash",
        },
        {
            "source_kind": "company_context",
            "document_id": "bad-doc",
            "title": "Rejected",
            "body": "Body",
            "content_hash": "bad-hash",
        },
    ]
    pool = FakePool(rows)
    _use_database_pool(monkeypatch, embeddings, pool)

    class RejectedInput(Exception):
        pass

    class SelectiveEmbeddings:
        async def create(self, **kwargs):
            inputs = kwargs["input"]
            if len(inputs) > 1 or inputs[0].startswith("Rejected"):
                raise RejectedInput("input rejected")
            return types.SimpleNamespace(
                data=[types.SimpleNamespace(index=0, embedding=[0.1, 0.2])]
            )

    monkeypatch.setattr(embeddings, "BadRequestError", RejectedInput)
    monkeypatch.setattr(
        embeddings,
        "_client",
        lambda: types.SimpleNamespace(embeddings=SelectiveEmbeddings()),
    )
    context = types.SimpleNamespace(log=lambda *_args, **_kwargs: None)

    result = asyncio.run(embeddings.handler(embeddings.Input(batch_size=3), context))

    assert result == {
        "status": "completed",
        "embedded": 1,
        "failed": 1,
        "model": "text-embedding-3-small",
        "requeued": False,
    }
    assert pool.executemany_values == [
        [("good-doc", "text-embedding-3-small", "good-hash", "[0.1,0.2]")]
    ]
    assert pool.execute_values == [
        (
            "bad-doc",
            "text-embedding-3-small",
            "bad-hash",
            "RejectedInput: input rejected",
        )
    ]


def test_handler_does_not_call_openai_when_batch_is_empty(monkeypatch):
    embeddings = _load()
    pool = FakePool([])
    _use_database_pool(monkeypatch, embeddings, pool)
    monkeypatch.setattr(
        embeddings,
        "_client",
        lambda: (_ for _ in ()).throw(AssertionError("client should not be created")),
    )
    context = types.SimpleNamespace(log=lambda *_args, **_kwargs: None)

    result = asyncio.run(embeddings.handler(embeddings.Input(), context))

    assert result == {
        "status": "completed",
        "embedded": 0,
        "failed": 0,
        "model": "text-embedding-3-small",
        "requeued": False,
    }
    assert pool.closed is True


def test_handler_does_not_requeue_a_partial_batch(monkeypatch):
    embeddings = _load()
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
    _use_database_pool(monkeypatch, embeddings, pool)
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
        log=lambda *_args, **_kwargs: None,
        start_workflow=unexpected_start,
    )

    result = asyncio.run(embeddings.handler(embeddings.Input(batch_size=2), context))

    assert result == {
        "status": "completed",
        "embedded": 1,
        "failed": 0,
        "model": "text-embedding-3-small",
        "requeued": False,
    }
