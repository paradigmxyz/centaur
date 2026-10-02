"""Preview retrieval over the company-context service's Google Drive index.

The legacy Google Docs index is served by ``client.py``. This module reads only
the tables owned by the company-context service, which exist only where that
service is deployed; row-level security limits rows to the requester's files.
"""

from __future__ import annotations

import asyncio
from datetime import datetime
from typing import Any

import asyncpg

from .client import (
    COMPANY_CONTEXT_EMBEDDINGS_ENABLED_ENV,
    DEFAULT_SEARCH_LIMIT,
    MAX_SEARCH_LIMIT,
    CompanyContextClient,
    _as_dict,
    _body_preview,
    _clamp,
    _env_flag_enabled,
    _hybrid_candidate_limit,
    _isoformat,
    _parse_datetime_filter,
    _reciprocal_rank_fusion,
    _row_value,
    _search_terms,
    _search_where_clause,
    _validate_date_window,
)

DOCUMENTS_TABLE = "company_context_data.google_drive_documents"
EMBEDDINGS_TABLE = "company_context_data.google_drive_document_embeddings"
SOURCE = "docs"
SOURCE_TYPES = ("google_doc", "pdf")
DOCUMENT_COLUMNS = """
    document_id,
    file_id,
    chunk_id,
    document_type,
    title,
    body,
    url,
    mime_type,
    drive_id,
    page_start,
    page_end,
    source_created_at,
    source_modified_at,
    metadata
"""


def _document_summary(row: Any) -> dict[str, Any]:
    """Return the tool's common document shape for one Drive chunk."""
    metadata = _as_dict(_row_value(row, "metadata", {}))
    metadata.update(
        {
            "file_id": str(_row_value(row, "file_id", "")),
            "chunk_id": str(_row_value(row, "chunk_id", "")),
            "drive_id": str(_row_value(row, "drive_id", "")),
            "mime_type": str(_row_value(row, "mime_type", "")),
            "page_start": _row_value(row, "page_start"),
            "page_end": _row_value(row, "page_end"),
        }
    )
    return {
        "document_id": str(_row_value(row, "document_id", "")),
        "source": SOURCE,
        "source_type": str(_row_value(row, "document_type", "")),
        "source_document_id": str(_row_value(row, "file_id", "")),
        "source_chunk_id": str(_row_value(row, "chunk_id", "")),
        "title": str(_row_value(row, "title", "")),
        "url": str(_row_value(row, "url", "")),
        "occurred_at": _isoformat(
            _row_value(row, "source_created_at") or _row_value(row, "source_modified_at")
        ),
        "source_updated_at": _isoformat(_row_value(row, "source_modified_at")),
        "metadata": metadata,
    }


def _result(row: Any, *, query: str) -> dict[str, Any]:
    result = _document_summary(row)
    result["preview"] = _body_preview(str(_row_value(row, "body", "") or ""), query=query)
    return result


def _normalize_source_type(source_type: str | None) -> str | None:
    normalized = source_type.strip() if source_type else None
    if normalized is not None and normalized not in SOURCE_TYPES:
        raise ValueError(f"source_type must be one of {', '.join(SOURCE_TYPES)}")
    return normalized


def _parse_window(
    occurred_after: str | datetime | None,
    occurred_before: str | datetime | None,
) -> tuple[datetime | None, datetime | None]:
    after = _parse_datetime_filter(occurred_after, name="occurred_after")
    before = _parse_datetime_filter(occurred_before, name="occurred_before")
    _validate_date_window(after, before)
    return after, before


class DriveV2Client:
    """Query the company-context service's Drive documents."""

    def __init__(
        self,
        database_url: str | None = None,
        *,
        embeddings_client: Any | None = None,
    ) -> None:
        # Reuse the scoped connection and query-embedding configuration.
        self._client = CompanyContextClient(database_url, embeddings_client=embeddings_client)

    async def _status_async(self) -> dict[str, Any]:
        conn = await self._client._connect()
        try:
            await conn.fetchval(f"SELECT 1 FROM {DOCUMENTS_TABLE} LIMIT 1")
        except (asyncpg.UndefinedTableError, asyncpg.InsufficientPrivilegeError) as exc:
            return {"status": "ok", "active": False, "table": DOCUMENTS_TABLE, "reason": str(exc)}
        finally:
            await conn.close()
        return {"status": "ok", "active": True, "table": DOCUMENTS_TABLE}

    def status(self) -> dict:
        """Report whether the Drive documents table is readable."""
        try:
            return asyncio.run(self._status_async())
        except Exception as exc:
            return {"status": "error", "error": str(exc)}

    async def _search_async(
        self,
        *,
        query: str,
        limit: int,
        hybrid: bool,
        source_type: str | None,
        occurred_after: datetime | None,
        occurred_before: datetime | None,
    ) -> dict[str, Any]:
        embeddings_available = hybrid and _env_flag_enabled(
            COMPANY_CONTEXT_EMBEDDINGS_ENABLED_ENV, default=False
        )
        candidate_limit = _hybrid_candidate_limit(limit) if embeddings_available else limit
        terms = _search_terms(query)
        search_terms = [query, *terms]
        source_type_param = len(search_terms) + 1
        after_param = len(search_terms) + 2
        before_param = len(search_terms) + 3
        limit_param = len(search_terms) + 4
        conn = await self._client._connect()
        try:
            rows = await conn.fetch(
                f"""
                SELECT
                    {DOCUMENT_COLUMNS},
                    paradedb.score(document_id) AS score
                FROM {DOCUMENTS_TABLE}
                WHERE {_search_where_clause(len(terms))}
                  AND (${source_type_param}::text IS NULL
                       OR document_type = ${source_type_param})
                  AND (${after_param}::timestamptz IS NULL
                       OR source_modified_at >= ${after_param})
                  AND (${before_param}::timestamptz IS NULL
                       OR source_modified_at < ${before_param})
                ORDER BY paradedb.score(document_id) DESC,
                         source_modified_at DESC NULLS LAST,
                         document_id ASC
                LIMIT ${limit_param}
                """,
                *search_terms,
                source_type,
                occurred_after,
                occurred_before,
                candidate_limit,
            )
            keyword_results = []
            for row in rows:
                result = _result(row, query=query)
                result["score"] = float(_row_value(row, "score", 0.0) or 0.0)
                result["lane"] = "indexed"
                keyword_results.append(result)

            vector_results: list[dict[str, Any]] = []
            if embeddings_available:
                try:
                    vector_results = await self._search_vectors_async(
                        conn,
                        query=query,
                        limit=candidate_limit,
                        source_type=source_type,
                        occurred_after=occurred_after,
                        occurred_before=occurred_before,
                    )
                except Exception:
                    # Vector search is optional; fall back to keyword results.
                    vector_results = []
        finally:
            await conn.close()

        if vector_results:
            results = _reciprocal_rank_fusion(keyword_results, vector_results, limit=limit)
            search_mode = "hybrid"
        else:
            results = keyword_results[:limit]
            search_mode = "keyword"
        return {
            "status": "ok",
            "query": query,
            "source": SOURCE,
            "source_type": source_type,
            "occurred_after": _isoformat(occurred_after),
            "occurred_before": _isoformat(occurred_before),
            "search_mode": search_mode,
            "count": len(results),
            "results": results,
        }

    async def _search_vectors_async(
        self,
        conn: asyncpg.Connection,
        *,
        query: str,
        limit: int,
        source_type: str | None,
        occurred_after: datetime | None,
        occurred_before: datetime | None,
    ) -> list[dict[str, Any]]:
        query_embedding = await self._client._query_embedding_async(query)
        rows = await conn.fetch(
            f"""
            SELECT
                d.document_id,
                d.file_id,
                d.chunk_id,
                d.document_type,
                d.title,
                d.body,
                d.url,
                d.mime_type,
                d.drive_id,
                d.page_start,
                d.page_end,
                d.source_created_at,
                d.source_modified_at,
                d.metadata,
                1 - (e.embedding <=> $1::vector) AS vector_similarity
            FROM {EMBEDDINGS_TABLE} e
            JOIN {DOCUMENTS_TABLE} d
              ON d.document_id = e.document_id
            WHERE e.model = $2
              AND ($3::text IS NULL OR d.document_type = $3)
              AND ($4::timestamptz IS NULL OR d.source_modified_at >= $4)
              AND ($5::timestamptz IS NULL OR d.source_modified_at < $5)
            ORDER BY e.embedding <=> $1::vector,
                     d.source_modified_at DESC NULLS LAST,
                     d.document_id ASC
            LIMIT $6
            """,
            query_embedding,
            self._client._embeddings_model(),
            source_type,
            occurred_after,
            occurred_before,
            limit,
        )
        results = []
        for row in rows:
            result = _result(row, query=query)
            result["vector_similarity"] = float(_row_value(row, "vector_similarity", 0.0) or 0.0)
            result["score"] = result["vector_similarity"]
            result["lane"] = "vector"
            results.append(result)
        return results

    def search(
        self,
        query: str,
        limit: int = DEFAULT_SEARCH_LIMIT,
        source_type: str | None = None,
        occurred_after: str | datetime | None = None,
        occurred_before: str | datetime | None = None,
        hybrid: bool = True,
    ) -> dict:
        """Search Drive documents visible to the requester."""
        normalized_query = query.strip()
        if not normalized_query:
            return {"status": "error", "error": "query cannot be empty"}
        try:
            after, before = _parse_window(occurred_after, occurred_before)
            return asyncio.run(
                self._search_async(
                    query=normalized_query,
                    limit=_clamp(limit, minimum=1, maximum=MAX_SEARCH_LIMIT),
                    hybrid=hybrid,
                    source_type=_normalize_source_type(source_type),
                    occurred_after=after,
                    occurred_before=before,
                )
            )
        except Exception as exc:
            return {"status": "error", "error": str(exc)}

    async def _list_documents_async(
        self,
        *,
        limit: int,
        source_type: str | None,
        occurred_after: datetime | None,
        occurred_before: datetime | None,
    ) -> dict[str, Any]:
        conn = await self._client._connect()
        try:
            rows = await conn.fetch(
                f"""
                SELECT {DOCUMENT_COLUMNS}
                FROM {DOCUMENTS_TABLE}
                WHERE ($1::text IS NULL OR document_type = $1)
                  AND ($2::timestamptz IS NULL OR source_modified_at >= $2)
                  AND ($3::timestamptz IS NULL OR source_modified_at < $3)
                ORDER BY source_modified_at DESC NULLS LAST,
                         source_created_at DESC NULLS LAST,
                         document_id ASC
                LIMIT $4
                """,
                source_type,
                occurred_after,
                occurred_before,
                limit,
            )
        finally:
            await conn.close()
        results = [_result(row, query="") for row in rows]
        return {
            "status": "ok",
            "source": SOURCE,
            "source_type": source_type,
            "occurred_after": _isoformat(occurred_after),
            "occurred_before": _isoformat(occurred_before),
            "count": len(results),
            "results": results,
        }

    def list_documents(
        self,
        limit: int = DEFAULT_SEARCH_LIMIT,
        source_type: str | None = None,
        occurred_after: str | datetime | None = None,
        occurred_before: str | datetime | None = None,
    ) -> dict:
        """List recently modified Drive documents visible to the requester."""
        try:
            after, before = _parse_window(occurred_after, occurred_before)
            return asyncio.run(
                self._list_documents_async(
                    limit=_clamp(limit, minimum=1, maximum=MAX_SEARCH_LIMIT),
                    source_type=_normalize_source_type(source_type),
                    occurred_after=after,
                    occurred_before=before,
                )
            )
        except Exception as exc:
            return {"status": "error", "error": str(exc)}

    async def _read_document_async(self, document_id: str, max_chars: int | None) -> dict:
        conn = await self._client._connect()
        try:
            row = await conn.fetchrow(
                f"""
                SELECT {DOCUMENT_COLUMNS}
                FROM {DOCUMENTS_TABLE}
                WHERE document_id = $1
                """,
                document_id,
            )
        finally:
            await conn.close()
        if not row:
            return {"status": "error", "error": f"document not found: {document_id}"}
        body = str(row["body"] or "")
        content = body if max_chars is None else body[:max_chars]
        return {
            "status": "ok",
            **_document_summary(row),
            "chars": len(content),
            "total_chars": len(body),
            "truncated": max_chars is not None and len(body) > max_chars,
            "content": content,
        }

    def read_document(self, document_id: str, max_chars: int = 0) -> dict:
        """Read one Drive document chunk, returning full content by default."""
        normalized_document_id = document_id.strip()
        if not normalized_document_id:
            return {"status": "error", "error": "document_id cannot be empty"}
        try:
            return asyncio.run(
                self._read_document_async(
                    normalized_document_id,
                    max_chars if max_chars > 0 else None,
                )
            )
        except Exception as exc:
            return {"status": "error", "error": str(exc)}

    async def _latest_date_async(self, source_type: str | None) -> dict[str, Any]:
        conn = await self._client._connect()
        try:
            row = await conn.fetchrow(
                f"""
                SELECT
                    MAX(COALESCE(source_modified_at, source_created_at)) AS latest_date,
                    MAX(source_modified_at) AS latest_source_updated_at,
                    MAX(source_created_at) AS latest_occurred_at,
                    COUNT(*)::bigint AS document_count
                FROM {DOCUMENTS_TABLE}
                WHERE ($1::text IS NULL OR document_type = $1)
                """,
                source_type,
            )
        finally:
            await conn.close()
        return {
            "status": "ok",
            "source": SOURCE,
            "source_type": source_type,
            "document_count": int(_row_value(row, "document_count", 0) or 0),
            "latest_date": _isoformat(_row_value(row, "latest_date")),
            "latest_source_updated_at": _isoformat(_row_value(row, "latest_source_updated_at")),
            "latest_occurred_at": _isoformat(_row_value(row, "latest_occurred_at")),
        }

    def latest_date(self, source_type: str | None = None) -> dict:
        """Return the latest indexed timestamp for visible Drive documents."""
        try:
            return asyncio.run(self._latest_date_async(_normalize_source_type(source_type)))
        except Exception as exc:
            return {"status": "error", "error": str(exc)}
