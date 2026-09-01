"""Workflow: embed company context documents with OpenAI."""

from __future__ import annotations

import json
import os
from dataclasses import dataclass, field
from typing import Any, Protocol

from api.workflow_engine import WorkflowContext
from openai import AsyncOpenAI, BadRequestError

WORKFLOW_NAME = "company_context_embeddings"

DEFAULT_BATCH_SIZE = 250
DEFAULT_INTERVAL_SECONDS = 5 * 60
DEFAULT_MAX_INPUT_CHARS = 8_192
DEFAULT_MODEL = "text-embedding-3-small"
EMBEDDING_DIMENSIONS = 1_536
OPENAI_BATCH_SIZE = 25
FALSE_ENV_VALUES = {"0", "false", "no", "off"}
EMBEDDING_UPSERTS = {
    "company_context": (
        "INSERT INTO company_context_document_embeddings "
        "  (company_context_document_id, model, content_hash, embedding) "
        "VALUES ($1, $2, $3, $4::vector) "
        "ON CONFLICT (company_context_document_id) DO UPDATE SET "
        "  model = EXCLUDED.model, "
        "  content_hash = EXCLUDED.content_hash, "
        "  embedding = EXCLUDED.embedding, "
        "  embedding_failed = FALSE, "
        "  failure_reason = NULL, "
        "  updated_at = NOW()"
    ),
    "google_docs": (
        "INSERT INTO company_context_document_embeddings "
        "  (google_docs_context_document_id, model, content_hash, embedding) "
        "VALUES ($1, $2, $3, $4::vector) "
        "ON CONFLICT (google_docs_context_document_id) DO UPDATE SET "
        "  model = EXCLUDED.model, "
        "  content_hash = EXCLUDED.content_hash, "
        "  embedding = EXCLUDED.embedding, "
        "  embedding_failed = FALSE, "
        "  failure_reason = NULL, "
        "  updated_at = NOW()"
    ),
    "granola": (
        "INSERT INTO company_context_document_embeddings "
        "  (granola_context_document_id, model, content_hash, embedding) "
        "VALUES ($1, $2, $3, $4::vector) "
        "ON CONFLICT (granola_context_document_id) DO UPDATE SET "
        "  model = EXCLUDED.model, "
        "  content_hash = EXCLUDED.content_hash, "
        "  embedding = EXCLUDED.embedding, "
        "  embedding_failed = FALSE, "
        "  failure_reason = NULL, "
        "  updated_at = NOW()"
    ),
}
EMBEDDING_FAILURE_UPSERTS = {
    "company_context": (
        "INSERT INTO company_context_document_embeddings "
        "  (company_context_document_id, model, content_hash, "
        "   embedding_failed, failure_reason) "
        "VALUES ($1, $2, $3, TRUE, $4) "
        "ON CONFLICT (company_context_document_id) DO UPDATE SET "
        "  model = EXCLUDED.model, "
        "  content_hash = EXCLUDED.content_hash, "
        "  embedding = NULL, "
        "  embedding_failed = TRUE, "
        "  failure_reason = EXCLUDED.failure_reason, "
        "  updated_at = NOW()"
    ),
    "google_docs": (
        "INSERT INTO company_context_document_embeddings "
        "  (google_docs_context_document_id, model, content_hash, "
        "   embedding_failed, failure_reason) "
        "VALUES ($1, $2, $3, TRUE, $4) "
        "ON CONFLICT (google_docs_context_document_id) DO UPDATE SET "
        "  model = EXCLUDED.model, "
        "  content_hash = EXCLUDED.content_hash, "
        "  embedding = NULL, "
        "  embedding_failed = TRUE, "
        "  failure_reason = EXCLUDED.failure_reason, "
        "  updated_at = NOW()"
    ),
    "granola": (
        "INSERT INTO company_context_document_embeddings "
        "  (granola_context_document_id, model, content_hash, "
        "   embedding_failed, failure_reason) "
        "VALUES ($1, $2, $3, TRUE, $4) "
        "ON CONFLICT (granola_context_document_id) DO UPDATE SET "
        "  model = EXCLUDED.model, "
        "  content_hash = EXCLUDED.content_hash, "
        "  embedding = NULL, "
        "  embedding_failed = TRUE, "
        "  failure_reason = EXCLUDED.failure_reason, "
        "  updated_at = NOW()"
    ),
}


def _positive_int(value: int | str | None, default: int) -> int:
    try:
        parsed = int(value) if value is not None else default
    except (TypeError, ValueError):
        return default
    return parsed if parsed > 0 else default


def _env_flag_enabled(name: str, default: bool = False) -> bool:
    value = os.getenv(name)
    if value is None:
        return default
    return value.strip().lower() not in FALSE_ENV_VALUES


SCHEDULE = {
    "schedule_id": WORKFLOW_NAME,
    "interval_seconds": _positive_int(
        os.getenv("COMPANY_CONTEXT_EMBEDDINGS_INTERVAL_SECONDS"),
        DEFAULT_INTERVAL_SECONDS,
    ),
    "enabled": _env_flag_enabled("COMPANY_CONTEXT_EMBEDDINGS_ENABLED"),
    "no_delivery": True,
}


@dataclass
class Input:
    """Runtime options for one embedding batch."""

    batch_size: int | None = None
    model: str | None = None
    max_input_chars: int | None = None
    metadata: dict[str, Any] = field(default_factory=dict)


class EmbeddingsClient(Protocol):
    embeddings: Any


def _client() -> EmbeddingsClient:
    return AsyncOpenAI()


def _model(value: str | None) -> str:
    configured = value or os.getenv("COMPANY_CONTEXT_EMBEDDINGS_MODEL") or DEFAULT_MODEL
    return configured.strip() or DEFAULT_MODEL


def _embedding_text(row: Any, max_chars: int) -> str:
    parts = [
        text
        for key in ("title", "body")
        if row[key] and (text := str(row[key]).strip())
    ]
    return "\n\n".join(parts)[: min(max_chars, DEFAULT_MAX_INPUT_CHARS)]


def _batches(rows: list[Any], size: int) -> list[list[Any]]:
    return [rows[start : start + size] for start in range(0, len(rows), size)]


async def _load_documents(connection, *, model: str, batch_size: int) -> list[Any]:
    return await connection.fetch(
        "WITH pending_documents AS ("
        "  SELECT 'company_context'::text AS source_kind, "
        "    d.document_id, d.title, d.body, d.content_hash, d.updated_at "
        "  FROM company_context_documents d "
        "  LEFT JOIN company_context_document_embeddings e "
        "    ON e.company_context_document_id = d.document_id "
        "  WHERE (btrim(d.title) <> '' OR btrim(d.body) <> '') "
        "    AND (e.embedding_id IS NULL OR e.model IS DISTINCT FROM $1 "
        "      OR e.content_hash IS DISTINCT FROM d.content_hash) "
        "  UNION ALL "
        "  SELECT 'google_docs'::text AS source_kind, "
        "    d.document_id, d.title, d.body, d.content_hash, d.updated_at "
        "  FROM google_docs_context_documents d "
        "  LEFT JOIN company_context_document_embeddings e "
        "    ON e.google_docs_context_document_id = d.document_id "
        "  WHERE (btrim(d.title) <> '' OR btrim(d.body) <> '') "
        "    AND (e.embedding_id IS NULL OR e.model IS DISTINCT FROM $1 "
        "      OR e.content_hash IS DISTINCT FROM d.content_hash) "
        "  UNION ALL "
        "  SELECT 'granola'::text AS source_kind, "
        "    d.document_id, d.title, d.body, d.content_hash, d.updated_at "
        "  FROM granola_context_documents d "
        "  LEFT JOIN company_context_document_embeddings e "
        "    ON e.granola_context_document_id = d.document_id "
        "  WHERE (btrim(d.title) <> '' OR btrim(d.body) <> '') "
        "    AND (e.embedding_id IS NULL OR e.model IS DISTINCT FROM $1 "
        "      OR e.content_hash IS DISTINCT FROM d.content_hash)"
        ") "
        "SELECT source_kind, document_id, title, body, content_hash "
        "FROM pending_documents "
        "ORDER BY updated_at, source_kind, document_id "
        "LIMIT $2",
        model,
        batch_size,
    )


async def _store_embeddings(
    connection,
    *,
    rows: list[Any],
    model: str,
    embeddings: list[list[float]],
) -> None:
    values_by_source: dict[str, list[tuple[str, str, str, str]]] = {}
    for row, embedding in zip(rows, embeddings, strict=True):
        source_kind = str(row["source_kind"])
        if source_kind not in EMBEDDING_UPSERTS:
            raise ValueError(f"unsupported embedding source {source_kind!r}")
        values_by_source.setdefault(source_kind, []).append(
            (
                str(row["document_id"]),
                model,
                str(row["content_hash"]),
                json.dumps(embedding, separators=(",", ":")),
            )
        )

    for source_kind, values in values_by_source.items():
        await connection.executemany(EMBEDDING_UPSERTS[source_kind], values)


async def _store_embedding_failure(
    connection,
    *,
    row: Any,
    model: str,
    error_type: str,
    error_message: str,
) -> None:
    source_kind = str(row["source_kind"])
    if source_kind not in EMBEDDING_FAILURE_UPSERTS:
        raise ValueError(f"unsupported embedding source {source_kind!r}")
    await connection.execute(
        EMBEDDING_FAILURE_UPSERTS[source_kind],
        str(row["document_id"]),
        model,
        str(row["content_hash"]),
        f"{error_type}: {error_message}"[:1_000],
    )


async def _generate_embeddings(
    client: EmbeddingsClient,
    *,
    model: str,
    inputs: list[str],
) -> list[list[float]]:
    response = await client.embeddings.create(
        model=model,
        input=inputs,
        dimensions=EMBEDDING_DIMENSIONS,
        encoding_format="float",
    )
    embeddings_by_index = {item.index: item.embedding for item in response.data}
    expected_indexes = set(range(len(inputs)))
    if set(embeddings_by_index) != expected_indexes:
        raise RuntimeError("OpenAI returned an incomplete embedding batch")
    return [embeddings_by_index[index] for index in range(len(inputs))]


async def handler(inp: Input, ctx: WorkflowContext) -> dict[str, Any]:
    """Generate and store one batch of missing or stale document embeddings."""
    model = _model(inp.model)
    batch_size = _positive_int(
        inp.batch_size or os.getenv("COMPANY_CONTEXT_EMBEDDINGS_BATCH_SIZE"),
        DEFAULT_BATCH_SIZE,
    )
    max_input_chars = _positive_int(
        inp.max_input_chars or os.getenv("COMPANY_CONTEXT_EMBEDDINGS_MAX_INPUT_CHARS"),
        DEFAULT_MAX_INPUT_CHARS,
    )
    connection = ctx._pool
    embedded_count = 0
    failed_count = 0
    rows = await _load_documents(connection, model=model, batch_size=batch_size)
    if rows:
        client = _client()
        prepared_rows: list[tuple[Any, str]] = []
        for row in rows:
            text = _embedding_text(row, max_input_chars)
            if text:
                prepared_rows.append((row, text))
                continue
            await _store_embedding_failure(
                connection,
                row=row,
                model=model,
                error_type="empty_input",
                error_message="document contains no non-whitespace text",
            )
            failed_count += 1
            ctx.log(
                "company_context_embedding_document_failed",
                source_kind=str(row["source_kind"]),
                document_id=str(row["document_id"]),
                error_type="empty_input",
            )

        for batch in _batches(prepared_rows, OPENAI_BATCH_SIZE):
            batch_rows = [row for row, _text in batch]
            batch_inputs = [text for _row, text in batch]
            try:
                embeddings = await _generate_embeddings(
                    client,
                    model=model,
                    inputs=batch_inputs,
                )
            except BadRequestError as batch_error:
                ctx.log(
                    "company_context_embedding_batch_rejected",
                    documents=len(batch),
                    error_type=type(batch_error).__name__,
                )
                for row, text in batch:
                    try:
                        document_embeddings = await _generate_embeddings(
                            client,
                            model=model,
                            inputs=[text],
                        )
                    except BadRequestError as document_error:
                        await _store_embedding_failure(
                            connection,
                            row=row,
                            model=model,
                            error_type=type(document_error).__name__,
                            error_message=str(document_error),
                        )
                        failed_count += 1
                        ctx.log(
                            "company_context_embedding_document_failed",
                            source_kind=str(row["source_kind"]),
                            document_id=str(row["document_id"]),
                            error_type=type(document_error).__name__,
                        )
                        continue
                    await _store_embeddings(
                        connection,
                        rows=[row],
                        model=model,
                        embeddings=document_embeddings,
                    )
                    embedded_count += 1
                continue

            await _store_embeddings(
                connection,
                rows=batch_rows,
                model=model,
                embeddings=embeddings,
            )
            embedded_count += len(batch_rows)

    if not rows:
        result = {
            "status": "completed",
            "embedded": 0,
            "failed": 0,
            "model": model,
            "requeued": False,
        }
        ctx.log("company_context_embeddings_completed", **result)
        return result

    next_run = None
    if len(rows) == batch_size:
        next_run = await ctx.start_workflow(
            WORKFLOW_NAME,
            {
                "batch_size": batch_size,
                "model": model,
                "max_input_chars": max_input_chars,
                "metadata": {
                    **inp.metadata,
                    "source": "company_context_embeddings_requeue",
                },
            },
            idempotency_key=f"{WORKFLOW_NAME}:{ctx.run_id}:next",
        )

    result: dict[str, Any] = {
        "status": "completed",
        "embedded": embedded_count,
        "failed": failed_count,
        "model": model,
        "requeued": next_run is not None,
    }
    if next_run is not None:
        result["next_run"] = next_run
    ctx.log("company_context_embeddings_completed", **result)
    return result
