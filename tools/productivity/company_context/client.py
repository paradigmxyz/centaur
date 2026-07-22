"""Fetch historical company context documents."""

from __future__ import annotations

import asyncio
import json
import os
import re
import time
import urllib.request
from collections import Counter
from contextlib import suppress
from datetime import UTC, datetime
from typing import Any
from urllib.parse import urlparse, urlunparse

import asyncpg

from centaur_sdk.tool_sdk import secret

DEFAULT_SEARCH_LIMIT = 10
MAX_SEARCH_LIMIT = 50
TITLE_MATCH_BOOST = 4
EXACT_QUERY_TITLE_BOOST = 8
EXACT_QUERY_BODY_BOOST = 2
THREAD_SCORE_MULTIPLIER = 1.25
CHANNEL_DAY_SCORE_MULTIPLIER = 0.75
DEFAULT_PREVIEW_CHARS = 280
MAX_RELATED_CHILDREN = 25
SLACK_DM_SOURCE = "slack_dm"
GRANOLA_SOURCE = "granola"
GRANOLA_SOURCE_TYPE = "granola_note"
DOCS_SOURCE = "docs"
LEGACY_GOOGLE_DRIVE_SOURCE = "google_drive"
GOOGLE_DOCS_SOURCE_TYPE = "google_doc"
COMPANY_CONTEXT_DSN_ENV = "CENTAUR_POSTGRES_DSN"
COMPANY_CONTEXT_DATABASE_ENV = "COMPANY_CONTEXT_POSTGRES_DATABASE"
DEFAULT_POSTGRES_DATABASE = "ai_v2"
COMPANY_CONTEXT_LOOKUP_METRICS_ENABLED_ENV = "COMPANY_CONTEXT_LOOKUP_METRICS_ENABLED"
VICTORIAMETRICS_PUSH_ENABLED_ENV = "VICTORIAMETRICS_PUSH_ENABLED"
VICTORIAMETRICS_URL_ENV = "VICTORIAMETRICS_URL"
DEFAULT_VICTORIAMETRICS_URL = "http://victoriametrics:8428"
METRICS_PUSH_TIMEOUT_SECONDS = 1.0
LOOKUP_REQUEST_METRIC = "company_context_lookup_requests"
LOOKUP_RESULT_METRIC = "company_context_lookup_results"
LOOKUP_ZERO_RESULT_METRIC = "company_context_lookup_zero_results"

_SEARCH_TERM_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.:/-]*")
_STOP_WORDS = {
    "a",
    "an",
    "and",
    "are",
    "as",
    "at",
    "be",
    "but",
    "by",
    "for",
    "from",
    "how",
    "i",
    "if",
    "in",
    "into",
    "is",
    "it",
    "of",
    "on",
    "or",
    "our",
    "that",
    "the",
    "their",
    "there",
    "these",
    "they",
    "this",
    "to",
    "was",
    "we",
    "were",
    "what",
    "when",
    "where",
    "which",
    "who",
    "why",
    "will",
    "with",
}


def _clamp(value: int, *, minimum: int, maximum: int) -> int:
    """Clamp integer tool inputs to predictable output bounds."""
    return max(minimum, min(int(value), maximum))


def _scoped_database_url() -> str:
    value = os.getenv(COMPANY_CONTEXT_DSN_ENV)  # noqa: TID251
    if value is None:
        value = secret(COMPANY_CONTEXT_DSN_ENV, default="")
    value = value.strip()
    if value == COMPANY_CONTEXT_DSN_ENV:
        return ""
    return value


def _database_url_with_name(value: str, database: str) -> str:
    parsed = urlparse(value)
    if parsed.scheme and parsed.netloc and parsed.path in ("", "/"):
        return urlunparse(parsed._replace(path=f"/{database}"))
    return value


def _postgres_database_name() -> str:
    value = os.getenv(COMPANY_CONTEXT_DATABASE_ENV, DEFAULT_POSTGRES_DATABASE)  # noqa: TID251
    return value.strip() or DEFAULT_POSTGRES_DATABASE


def _as_dict(value: Any) -> dict[str, Any]:
    """Decode asyncpg JSON/JSONB values into a dict."""
    if isinstance(value, dict):
        return value
    if isinstance(value, str):
        try:
            parsed = json.loads(value)
            if isinstance(parsed, dict):
                return parsed
        except Exception:
            return {}
    return {}


def _isoformat(value: Any) -> str | None:
    """Serialize datetimes while leaving absent values explicit."""
    if isinstance(value, datetime):
        return value.isoformat()
    return None


def _env_flag_enabled(name: str, *, default: bool) -> bool:
    value = os.getenv(name)  # noqa: TID251
    if value is None:
        return default
    return value.strip().lower() not in {"0", "false", "no", "off"}


def _first_nonempty_env(names: list[str]) -> str | None:
    for name in names:
        value = os.getenv(name, "").strip()  # noqa: TID251
        if value:
            return value
    return None


def _metric_runtime_labels() -> dict[str, str]:
    labels: dict[str, str] = {}
    environment = _first_nonempty_env(
        ["METRICS_ENVIRONMENT", "CENTAUR_ENVIRONMENT", "DEPLOY_ENV", "ENVIRONMENT"]
    )
    namespace = _first_nonempty_env(
        ["METRICS_NAMESPACE", "CENTAUR_NAMESPACE", "POD_NAMESPACE", "NAMESPACE"]
    )
    if environment:
        labels["environment"] = environment
    if namespace:
        labels["namespace"] = namespace
    return labels


def _metric_label_value(value: str | None, *, empty: str = "all") -> str:
    normalized = str(value or "").strip()
    return normalized if normalized else empty


def _format_metric_labels(labels: dict[str, str]) -> str:
    if not labels:
        return ""
    parts = []
    for key in sorted(labels):
        value = labels[key].replace("\\", "\\\\").replace("\n", "\\n").replace('"', '\\"')
        parts.append(f'{key}="{value}"')
    return "{" + ",".join(parts) + "}"


def _format_metric_sample(
    metric: str,
    value: int | float,
    labels: dict[str, str],
    timestamp_ms: int,
) -> str:
    return f"{metric}{_format_metric_labels(labels)} {value} {timestamp_ms}"


def _company_context_lookup_metrics_enabled() -> bool:
    return _env_flag_enabled(COMPANY_CONTEXT_LOOKUP_METRICS_ENABLED_ENV, default=True) and (
        _env_flag_enabled(VICTORIAMETRICS_PUSH_ENABLED_ENV, default=True)
    )


def _victoria_metrics_import_url() -> str:
    base_url = os.getenv(VICTORIAMETRICS_URL_ENV, DEFAULT_VICTORIAMETRICS_URL).strip()  # noqa: TID251
    if base_url and not base_url.startswith(("http://", "https://")):
        base_url = f"http://{base_url}"
    return f"{(base_url or DEFAULT_VICTORIAMETRICS_URL).rstrip('/')}/api/v1/import/prometheus"


def _push_company_context_lookup_metric_lines(lines: list[str]) -> None:
    if not lines or not _company_context_lookup_metrics_enabled():
        return
    try:
        body = ("\n".join(lines) + "\n").encode()
        request = urllib.request.Request(
            _victoria_metrics_import_url(),
            data=body,
            headers={"Content-Type": "text/plain; version=0.0.4"},
            method="POST",
        )
        with urllib.request.urlopen(request, timeout=METRICS_PUSH_TIMEOUT_SECONDS):
            pass
    except Exception:
        return


def _emit_company_context_lookup_metrics(
    *,
    status: str,
    requested_source: str | None,
    requested_source_type: str | None,
    occurred_after: datetime | None,
    occurred_before: datetime | None,
    results: list[dict[str, Any]],
) -> None:
    """Emit event-style lookup samples; dashboards should sum them over a range."""
    labels = {
        **_metric_runtime_labels(),
        "status": _metric_label_value(status, empty="unknown"),
        "requested_source": _metric_label_value(requested_source),
        "requested_source_type": _metric_label_value(requested_source_type),
        "time_window": str(occurred_after is not None or occurred_before is not None).lower(),
    }
    timestamp_ms = int(time.time() * 1000)
    lines = [_format_metric_sample(LOOKUP_REQUEST_METRIC, 1, labels, timestamp_ms)]

    if status == "ok" and not results:
        lines.append(_format_metric_sample(LOOKUP_ZERO_RESULT_METRIC, 1, labels, timestamp_ms))

    grouped_results = Counter(
        (
            _metric_label_value(str(result.get("source") or "")),
            _metric_label_value(str(result.get("source_type") or "")),
            _metric_label_value(str(result.get("lane") or "indexed"), empty="indexed"),
        )
        for result in results
    )
    for (source, source_type, lane), count in sorted(grouped_results.items()):
        lines.append(
            _format_metric_sample(
                LOOKUP_RESULT_METRIC,
                count,
                {
                    **_metric_runtime_labels(),
                    "source": source,
                    "source_type": source_type,
                    "lane": lane,
                },
                timestamp_ms,
            )
        )

    _push_company_context_lookup_metric_lines(lines)


def _parse_datetime_filter(value: str | datetime | None, *, name: str) -> datetime | None:
    """Parse optional date filters for tool calls."""
    if value is None:
        return None
    if isinstance(value, datetime):
        parsed = value
    else:
        text = str(value).strip()
        if not text:
            return None
        if text.endswith("Z"):
            text = f"{text[:-1]}+00:00"
        try:
            parsed = datetime.fromisoformat(text)
        except ValueError as exc:
            raise ValueError(f"{name} must be an ISO 8601 date or timestamp") from exc
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=UTC)
    return parsed


def _validate_date_window(start: datetime | None, end: datetime | None) -> None:
    """Reject inverted date ranges before hitting Postgres."""
    if start is not None and end is not None and start >= end:
        raise ValueError("occurred_after must be earlier than occurred_before")


def _normalize_text(value: str) -> str:
    """Collapse whitespace so previews stay compact and readable."""
    return re.sub(r"\s+", " ", value).strip()


def _search_terms(query: str) -> list[str]:
    """Extract unique content terms, falling back when filtering removes everything."""
    seen: set[str] = set()
    all_terms: list[str] = []
    filtered_terms: list[str] = []
    for match in _SEARCH_TERM_RE.finditer(query):
        term = match.group(0).strip()
        if len(term) < 2:
            continue
        key = term.lower()
        if key in seen:
            continue
        seen.add(key)
        all_terms.append(term)
        if key not in _STOP_WORDS:
            filtered_terms.append(term)
    return filtered_terms or all_terms or [query]


def _search_where_clause(term_count: int) -> str:
    """Build a ParadeDB query that boosts exact matches and falls back to OR term matching."""
    clauses = [
        "("
        f"title ||| $1::text::pdb.boost({EXACT_QUERY_TITLE_BOOST}) "
        f"OR body ||| $1::text::pdb.boost({EXACT_QUERY_BODY_BOOST})"
        ")"
    ]
    for index in range(2, term_count + 2):
        clauses.append(
            f"(title ||| ${index}::text::pdb.boost({TITLE_MATCH_BOOST}) OR body ||| ${index}::text)"
        )
    return " OR ".join(clauses)


def _body_preview(body: str, *, query: str, max_chars: int = DEFAULT_PREVIEW_CHARS) -> str:
    """Build a compact preview centered on the first query-term hit when possible."""
    normalized = _normalize_text(body)
    if not normalized:
        return ""
    if len(normalized) <= max_chars:
        return normalized

    terms = _search_terms(query)
    start = 0
    lowered = normalized.lower()
    for term in terms:
        index = lowered.find(term.lower())
        if index >= 0:
            start = max(0, index - max_chars // 3)
            break

    end = min(len(normalized), start + max_chars)
    snippet = normalized[start:end].strip()
    if start > 0:
        snippet = f"...{snippet}"
    if end < len(normalized):
        snippet = f"{snippet}..."
    return snippet


def _row_value(row: Any, key: str, default: Any = None) -> Any:
    """Read values from asyncpg rows while tolerating sparse test doubles."""
    try:
        value = row[key]
    except (KeyError, IndexError, TypeError):
        return default
    return default if value is None else value


def _document_summary(row: Any) -> dict[str, Any]:
    """Return the common metadata we expose for document records."""
    return {
        "document_id": str(_row_value(row, "document_id", "")),
        "source": str(_row_value(row, "source", "")),
        "source_type": str(_row_value(row, "source_type", "")),
        "source_document_id": str(_row_value(row, "source_document_id", "")),
        "source_chunk_id": str(_row_value(row, "source_chunk_id", "")),
        "parent_document_id": str(_row_value(row, "parent_document_id", "") or "") or None,
        "title": str(_row_value(row, "title", "")),
        "url": str(_row_value(row, "url", "")),
        "author_name": str(_row_value(row, "author_name", "")),
        "access_scope": str(_row_value(row, "access_scope", "")),
        "occurred_at": _isoformat(_row_value(row, "occurred_at")),
        "source_updated_at": _isoformat(_row_value(row, "source_updated_at")),
        "metadata": _as_dict(_row_value(row, "metadata", {})),
    }


def _google_doc_summary(row: Any) -> dict[str, Any]:
    """Return the common result shape for OAuth Google Docs context chunks."""
    metadata = _as_dict(_row_value(row, "metadata", {}))
    metadata.update(
        {
            "file_id": str(_row_value(row, "file_id", "")),
            "chunk_id": str(_row_value(row, "chunk_id", "")),
            "drive_id": str(_row_value(row, "drive_id", "")),
            "mime_type": str(_row_value(row, "mime_type", "")),
            "provider_author_id": str(_row_value(row, "provider_author_id", "")),
        }
    )
    return {
        "document_id": str(_row_value(row, "document_id", "")),
        "source": DOCS_SOURCE,
        "source_type": GOOGLE_DOCS_SOURCE_TYPE,
        "source_document_id": str(_row_value(row, "file_id", "")),
        "source_chunk_id": str(_row_value(row, "chunk_id", "")),
        "parent_document_id": None,
        "title": str(_row_value(row, "title", "")),
        "url": str(_row_value(row, "url", "")),
        "author_name": str(_row_value(row, "provider_author_name", "")),
        "access_scope": "",
        "occurred_at": _isoformat(
            _row_value(row, "source_created_at") or _row_value(row, "source_modified_at")
        ),
        "source_updated_at": _isoformat(_row_value(row, "source_modified_at")),
        "metadata": metadata,
    }


def _granola_doc_summary(row: Any) -> dict[str, Any]:
    """Return the common result shape for user-visible Granola notes."""
    metadata = _as_dict(_row_value(row, "metadata", {}))
    metadata.update(
        {
            "note_id": str(_row_value(row, "note_id", "")),
            "owner_id": str(_row_value(row, "owner_id", "")),
            "owner_email": str(_row_value(row, "owner_email", "")),
            "attendee_labels": list(_row_value(row, "attendee_labels", []) or []),
        }
    )
    return {
        "document_id": str(_row_value(row, "document_id", "")),
        "source": GRANOLA_SOURCE,
        "source_type": GRANOLA_SOURCE_TYPE,
        "source_document_id": str(_row_value(row, "note_id", "")),
        "source_chunk_id": "",
        "parent_document_id": None,
        "title": str(_row_value(row, "title", "")),
        "url": str(_row_value(row, "url", "")),
        "author_name": str(
            _row_value(row, "owner_name", "")
            or _row_value(row, "owner_email", "")
            or _row_value(row, "owner_id", "")
        ),
        "access_scope": "granola_note",
        "occurred_at": _isoformat(_row_value(row, "occurred_at")),
        "source_updated_at": _isoformat(_row_value(row, "source_updated_at")),
        "metadata": metadata,
    }


def _dm_document_summary(row: Any) -> dict[str, Any]:
    """Return metadata for user-scoped Slack conversation context records."""
    metadata = _as_dict(_row_value(row, "metadata", {}))
    conversation_type = str(_row_value(row, "conversation_type", ""))
    conversation_id = str(_row_value(row, "conversation_id", ""))
    message_ts = str(_row_value(row, "message_ts", ""))
    user_id = str(_row_value(row, "user_id", ""))
    bot_id = str(_row_value(row, "bot_id", ""))
    return {
        "document_id": str(_row_value(row, "document_id", "")),
        "source": SLACK_DM_SOURCE,
        "source_type": f"slack_{conversation_type}" if conversation_type else SLACK_DM_SOURCE,
        "source_document_id": conversation_id,
        "source_chunk_id": message_ts,
        "parent_document_id": None,
        "title": str(_row_value(row, "title", "")),
        "url": str(_row_value(row, "permalink", "")),
        "author_name": user_id or bot_id,
        "access_scope": (
            "slack_private_channel"
            if conversation_type == "private_channel"
            else "slack_dm"
        ),
        "occurred_at": _isoformat(_row_value(row, "occurred_at")),
        "source_updated_at": _isoformat(_row_value(row, "source_updated_at")),
        "conversation_id": conversation_id,
        "conversation_type": conversation_type,
        "message_ts": message_ts,
        "thread_ts": _row_value(row, "thread_ts"),
        "user_id": user_id,
        "bot_id": bot_id,
        "attachment_count": int(metadata.get("attachment_count") or 0),
        "metadata": metadata,
    }


def _dm_conversation_summary(row: Any) -> dict[str, Any]:
    """Return visible metadata for a Slack DM/MPIM conversation."""
    return {
        "document_id": str(_row_value(row, "document_id", "")),
        "source": SLACK_DM_SOURCE,
        "source_type": "slack_dm_conversation",
        "home_team_id": str(_row_value(row, "home_team_id", "")),
        "conversation_id": str(_row_value(row, "conversation_id", "")),
        "conversation_type": str(_row_value(row, "conversation_type", "")),
        "title": str(_row_value(row, "title", "")),
        "is_ext_shared": bool(_row_value(row, "is_ext_shared", False)),
        "last_seen_at": _isoformat(_row_value(row, "last_seen_at")),
        "source_updated_at": _isoformat(_row_value(row, "source_updated_at")),
        "participant_user_ids": list(_row_value(row, "participant_user_ids", []) or []),
        "participant_labels": list(_row_value(row, "participant_labels", []) or []),
        "participant_count": int(_row_value(row, "participant_count", 0) or 0),
        "matched_labels": list(_row_value(row, "matched_labels", []) or []),
        "metadata": _as_dict(_row_value(row, "metadata", {})),
    }


def _include_google_docs_source(source: str | None, source_type: str | None) -> bool:
    return (source is None or source == DOCS_SOURCE) and source_type in (
        None,
        GOOGLE_DOCS_SOURCE_TYPE,
    )


def _include_slack_dms_source(source: str | None, source_type: str | None) -> bool:
    return (source is None or source == SLACK_DM_SOURCE) and source_type in (
        None,
        SLACK_DM_SOURCE,
        "slack_im",
        "slack_mpim",
        "slack_private_channel",
        "slack_dm_conversation",
    )


def _include_granola_source(source: str | None, source_type: str | None) -> bool:
    return (source is None or source == GRANOLA_SOURCE) and source_type in (
        None,
        GRANOLA_SOURCE,
        GRANOLA_SOURCE_TYPE,
    )


def _company_context_filters_for_source(
    source: str | None,
    source_type: str | None,
) -> tuple[str | None, str | None]:
    """Map the public docs source to the legacy projected Google Drive rows."""
    if source == DOCS_SOURCE:
        return LEGACY_GOOGLE_DRIVE_SOURCE, source_type or GOOGLE_DOCS_SOURCE_TYPE
    return source, source_type


class CompanyContextClient:
    """Query the shared company context document table."""

    def __init__(self, database_url: str | None = None) -> None:
        self._database_url = (database_url or _scoped_database_url()).strip()

    def _require_database_url(self) -> str:
        if not self._database_url:
            raise RuntimeError(f"{COMPANY_CONTEXT_DSN_ENV} is required for company context search")
        return self._database_url

    async def _connect(self) -> asyncpg.Connection:
        return await asyncpg.connect(
            _database_url_with_name(self._require_database_url(), _postgres_database_name()),
            command_timeout=30,
        )

    async def _search_async(
        self,
        *,
        query: str,
        limit: int,
        source: str | None,
        source_type: str | None,
        occurred_after: datetime | None,
        occurred_before: datetime | None,
    ) -> dict[str, Any]:
        conn = await self._connect()
        try:
            terms = _search_terms(query)
            search_terms = [query, *terms]
            results = []
            google_docs_error = None
            granola_error = None
            company_source, company_source_type = _company_context_filters_for_source(
                source,
                source_type,
            )
            source_param = len(search_terms) + 1
            source_type_param = len(search_terms) + 2
            occurred_after_param = len(search_terms) + 3
            occurred_before_param = len(search_terms) + 4
            limit_param = len(search_terms) + 5
            rows = await conn.fetch(
                f"""
                SELECT
                    document_id,
                    source,
                    source_type,
                    source_document_id,
                    source_chunk_id,
                    parent_document_id,
                    title,
                    url,
                    author_name,
                    access_scope,
                    body,
                    occurred_at,
                    source_updated_at,
                    metadata,
                    paradedb.score(document_id) AS score
                FROM company_context_documents
                WHERE {_search_where_clause(len(terms))}
                  AND (${source_param}::text IS NULL OR source = ${source_param})
                  AND (${source_type_param}::text IS NULL OR source_type = ${source_type_param})
                  AND (${occurred_after_param}::timestamptz IS NULL
                       OR occurred_at >= ${occurred_after_param})
                  AND (${occurred_before_param}::timestamptz IS NULL
                       OR occurred_at < ${occurred_before_param})
                ORDER BY
                    paradedb.score(document_id)
                    * CASE source_type
                        WHEN 'slack_thread' THEN {THREAD_SCORE_MULTIPLIER}
                        WHEN 'slack_channel_day' THEN {CHANNEL_DAY_SCORE_MULTIPLIER}
                        ELSE 1.0
                    END DESC,
                    source_updated_at DESC NULLS LAST
                LIMIT ${limit_param}
                """,
                *search_terms,
                company_source,
                company_source_type,
                occurred_after,
                occurred_before,
                limit,
            )
            for row in rows:
                result = _document_summary(row)
                result["score"] = float(_row_value(row, "score", 0.0) or 0.0)
                result["preview"] = _body_preview(
                    str(_row_value(row, "body", "") or ""),
                    query=query,
                )
                result["lane"] = "indexed"
                result["result_type"] = str(result["source_type"] or "indexed_document")
                results.append(result)

            if _include_google_docs_source(source, source_type):
                try:
                    google_rows = await self._search_google_docs_async(
                        conn,
                        search_terms=search_terms,
                        term_count=len(terms),
                        limit=limit,
                        modified_after=occurred_after,
                        modified_before=occurred_before,
                    )
                    for row in google_rows:
                        result = _google_doc_summary(row)
                        result["score"] = float(_row_value(row, "score", 0.0) or 0.0)
                        result["preview"] = _body_preview(
                            str(_row_value(row, "body", "") or ""),
                            query=query,
                        )
                        result["lane"] = "indexed"
                        result["result_type"] = GOOGLE_DOCS_SOURCE_TYPE
                        results.append(result)
                except asyncpg.UndefinedTableError as exc:
                    google_docs_error = str(exc)

            if _include_granola_source(source, source_type):
                try:
                    granola_rows = await self._search_granola_async(
                        conn,
                        search_terms=search_terms,
                        term_count=len(terms),
                        limit=limit,
                        occurred_after=occurred_after,
                        occurred_before=occurred_before,
                    )
                    for row in granola_rows:
                        result = _granola_doc_summary(row)
                        result["score"] = float(_row_value(row, "score", 0.0) or 0.0)
                        result["preview"] = _body_preview(
                            str(_row_value(row, "body", "") or ""),
                            query=query,
                        )
                        result["lane"] = "indexed"
                        result["result_type"] = GRANOLA_SOURCE_TYPE
                        results.append(result)
                except asyncpg.UndefinedTableError as exc:
                    granola_error = str(exc)

            results.sort(
                key=lambda item: (
                    float(item.get("score") or 0.0),
                    str(item.get("source_updated_at") or ""),
                ),
                reverse=True,
            )
            results = results[:limit]

            _emit_company_context_lookup_metrics(
                status="ok",
                requested_source=source,
                requested_source_type=source_type,
                occurred_after=occurred_after,
                occurred_before=occurred_before,
                results=results,
            )

            response = {
                "status": "ok",
                "query": query,
                "source": source,
                "source_type": source_type,
                "occurred_after": _isoformat(occurred_after),
                "occurred_before": _isoformat(occurred_before),
                "count": len(results),
                "indexed_count": len(results),
                "results": results,
            }
            if google_docs_error:
                response["google_docs_error"] = google_docs_error
            if granola_error:
                response["granola_error"] = granola_error
            return response
        finally:
            await conn.close()

    async def _search_google_docs_async(
        self,
        conn: asyncpg.Connection,
        *,
        search_terms: list[str],
        term_count: int,
        limit: int,
        modified_after: datetime | None,
        modified_before: datetime | None,
    ) -> list[Any]:
        modified_after_param = len(search_terms) + 1
        modified_before_param = len(search_terms) + 2
        limit_param = len(search_terms) + 3
        return await conn.fetch(
            f"""
            SELECT
                document_id,
                file_id,
                chunk_id,
                title,
                body,
                url,
                provider_author_id,
                provider_author_name,
                mime_type,
                drive_id,
                source_created_at,
                source_modified_at,
                metadata,
                paradedb.score(document_id) AS score
            FROM google_docs_context_documents
            WHERE {_search_where_clause(term_count)}
              AND (${modified_after_param}::timestamptz IS NULL
                   OR source_modified_at >= ${modified_after_param})
              AND (${modified_before_param}::timestamptz IS NULL
                   OR source_modified_at < ${modified_before_param})
            ORDER BY paradedb.score(document_id) DESC,
                     source_modified_at DESC NULLS LAST,
                     document_id ASC
            LIMIT ${limit_param}
            """,
            *search_terms,
            modified_after,
            modified_before,
            limit,
        )

    async def _search_granola_async(
        self,
        conn: asyncpg.Connection,
        *,
        search_terms: list[str],
        term_count: int,
        limit: int,
        occurred_after: datetime | None,
        occurred_before: datetime | None,
    ) -> list[Any]:
        occurred_after_param = len(search_terms) + 1
        occurred_before_param = len(search_terms) + 2
        limit_param = len(search_terms) + 3
        return await conn.fetch(
            f"""
            SELECT
                document_id,
                note_id,
                title,
                body,
                url,
                owner_id,
                owner_email,
                owner_name,
                access_emails,
                attendee_labels,
                occurred_at,
                source_updated_at,
                metadata,
                paradedb.score(document_id) AS score
            FROM granola_context_documents
            WHERE {_search_where_clause(term_count)}
              AND (${occurred_after_param}::timestamptz IS NULL
                   OR occurred_at >= ${occurred_after_param})
              AND (${occurred_before_param}::timestamptz IS NULL
                   OR occurred_at < ${occurred_before_param})
            ORDER BY paradedb.score(document_id) DESC,
                     occurred_at DESC NULLS LAST,
                     source_updated_at DESC NULLS LAST,
                     document_id ASC
            LIMIT ${limit_param}
            """,
            *search_terms,
            occurred_after,
            occurred_before,
            limit,
        )

    async def _latest_date_for_connection(
        self,
        conn: asyncpg.Connection,
        *,
        source: str | None,
        source_type: str | None,
    ) -> dict[str, Any]:
        """Return latest indexed date using an existing DB connection."""
        if source == SLACK_DM_SOURCE:
            return self._empty_latest_date_result(source=source, source_type=source_type)
        company_source, company_source_type = _company_context_filters_for_source(
            source,
            source_type,
        )
        row = await conn.fetchrow(
            """
            SELECT
                MAX(COALESCE(source_updated_at, occurred_at)) AS latest_date,
                MAX(source_updated_at) AS latest_source_updated_at,
                MAX(occurred_at) AS latest_occurred_at,
                COUNT(*)::bigint AS document_count
            FROM company_context_documents
            WHERE ($1::text IS NULL OR source = $1)
              AND ($2::text IS NULL OR source_type = $2)
            """,
            company_source,
            company_source_type,
        )
        if not row or int(row["document_count"] or 0) == 0:
            return self._empty_latest_date_result(source=source, source_type=source_type)
        return {
            "status": "ok",
            "source": source,
            "source_type": source_type,
            "document_count": int(row["document_count"] or 0),
            "latest_date": _isoformat(row["latest_date"]),
            "latest_source_updated_at": _isoformat(row["latest_source_updated_at"]),
            "latest_occurred_at": _isoformat(row["latest_occurred_at"]),
        }

    async def _latest_google_docs_for_connection(
        self,
        conn: asyncpg.Connection,
        *,
        source: str | None,
        source_type: str | None,
    ) -> dict[str, Any]:
        if not _include_google_docs_source(source, source_type):
            return self._empty_latest_date_result(source=source, source_type=source_type)
        row = await conn.fetchrow(
            """
            SELECT
                MAX(COALESCE(source_modified_at, source_created_at)) AS latest_date,
                MAX(source_modified_at) AS latest_source_updated_at,
                MAX(source_created_at) AS latest_occurred_at,
                COUNT(*)::bigint AS document_count
            FROM google_docs_context_documents
            """
        )
        if not row or int(row["document_count"] or 0) == 0:
            return self._empty_latest_date_result(source=source, source_type=source_type)
        return {
            "status": "ok",
            "source": source,
            "source_type": source_type,
            "document_count": int(row["document_count"] or 0),
            "latest_date": _isoformat(row["latest_date"]),
            "latest_source_updated_at": _isoformat(row["latest_source_updated_at"]),
            "latest_occurred_at": _isoformat(row["latest_occurred_at"]),
        }

    async def _latest_granola_for_connection(
        self,
        conn: asyncpg.Connection,
        *,
        source: str | None,
        source_type: str | None,
    ) -> dict[str, Any]:
        if not _include_granola_source(source, source_type):
            return self._empty_latest_date_result(source=source, source_type=source_type)
        row = await conn.fetchrow(
            """
            SELECT
                MAX(COALESCE(source_updated_at, occurred_at)) AS latest_date,
                MAX(source_updated_at) AS latest_source_updated_at,
                MAX(occurred_at) AS latest_occurred_at,
                COUNT(*)::bigint AS document_count
            FROM granola_context_documents
            """
        )
        return self._latest_date_result_from_row(
            row,
            source=source,
            source_type=source_type,
        )

    async def _latest_slack_dms_for_connection(
        self,
        conn: asyncpg.Connection,
        *,
        source: str | None,
        source_type: str | None,
    ) -> dict[str, Any]:
        if not _include_slack_dms_source(source, source_type):
            return self._empty_latest_date_result(source=source, source_type=source_type)

        message_conversation_type = None
        include_messages = source_type in (
            None,
            SLACK_DM_SOURCE,
            "slack_im",
            "slack_mpim",
            "slack_private_channel",
        )
        if source_type in ("slack_im", "slack_mpim", "slack_private_channel"):
            message_conversation_type = source_type.removeprefix("slack_")
        include_conversations = source_type in (None, SLACK_DM_SOURCE, "slack_dm_conversation")

        messages = self._empty_latest_date_result(source=source, source_type=source_type)
        conversations = self._empty_latest_date_result(source=source, source_type=source_type)

        if include_messages:
            with suppress(asyncpg.UndefinedTableError):
                row = await conn.fetchrow(
                    """
                    SELECT
                        MAX(COALESCE(source_updated_at, occurred_at)) AS latest_date,
                        MAX(source_updated_at) AS latest_source_updated_at,
                        MAX(occurred_at) AS latest_occurred_at,
                        COUNT(*)::bigint AS document_count
                    FROM slack_private_context_documents
                    WHERE ($1::text IS NULL OR conversation_type = $1)
                    """,
                    message_conversation_type,
                )
                messages = self._latest_date_result_from_row(
                    row,
                    source=source,
                    source_type=source_type,
                )

        if include_conversations:
            with suppress(asyncpg.UndefinedTableError):
                row = await conn.fetchrow(
                    """
                    SELECT
                        MAX(COALESCE(source_updated_at, last_seen_at)) AS latest_date,
                        MAX(source_updated_at) AS latest_source_updated_at,
                        MAX(last_seen_at) AS latest_occurred_at,
                        COUNT(*)::bigint AS document_count
                    FROM slack_private_conversation_context_documents
                    """,
                )
                conversations = self._latest_date_result_from_row(
                    row,
                    source=source,
                    source_type=source_type,
                )

        return self._merge_latest_dates(
            source=source,
            source_type=source_type,
            indexed=messages,
            google_docs=conversations,
        )

    def _empty_latest_date_result(
        self,
        *,
        source: str | None,
        source_type: str | None,
    ) -> dict[str, Any]:
        return {
            "status": "ok",
            "source": source,
            "source_type": source_type,
            "document_count": 0,
            "latest_date": None,
            "latest_source_updated_at": None,
            "latest_occurred_at": None,
        }

    def _latest_date_result_from_row(
        self,
        row: Any,
        *,
        source: str | None,
        source_type: str | None,
    ) -> dict[str, Any]:
        if not row or int(row["document_count"] or 0) == 0:
            return self._empty_latest_date_result(source=source, source_type=source_type)
        return {
            "status": "ok",
            "source": source,
            "source_type": source_type,
            "document_count": int(row["document_count"] or 0),
            "latest_date": _isoformat(row["latest_date"]),
            "latest_source_updated_at": _isoformat(row["latest_source_updated_at"]),
            "latest_occurred_at": _isoformat(row["latest_occurred_at"]),
        }

    def _merge_latest_dates(
        self,
        *,
        source: str | None,
        source_type: str | None,
        indexed: dict[str, Any],
        google_docs: dict[str, Any],
        slack_dms: dict[str, Any] | None = None,
        granola: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        def latest(values: list[str | None]) -> str | None:
            present = [value for value in values if value]
            return max(present) if present else None

        latest_results = [indexed, google_docs]
        if slack_dms is not None:
            latest_results.append(slack_dms)
        if granola is not None:
            latest_results.append(granola)

        return {
            "status": "ok",
            "source": source,
            "source_type": source_type,
            "document_count": sum(
                int(result.get("document_count") or 0) for result in latest_results
            ),
            "latest_date": latest([result.get("latest_date") for result in latest_results]),
            "latest_source_updated_at": latest(
                [result.get("latest_source_updated_at") for result in latest_results]
            ),
            "latest_occurred_at": latest(
                [result.get("latest_occurred_at") for result in latest_results]
            ),
        }

    def search(
        self,
        query: str,
        limit: int = DEFAULT_SEARCH_LIMIT,
        source: str | None = None,
        source_type: str | None = None,
        occurred_after: str | datetime | None = None,
        occurred_before: str | datetime | None = None,
    ) -> dict:
        """Search indexed company context documents and return candidate document ids."""
        normalized_query = query.strip()
        if not normalized_query:
            return {"status": "error", "error": "query cannot be empty"}

        normalized_source = source.strip() if source else None
        normalized_source_type = source_type.strip() if source_type else None
        try:
            parsed_occurred_after = _parse_datetime_filter(
                occurred_after,
                name="occurred_after",
            )
            parsed_occurred_before = _parse_datetime_filter(
                occurred_before,
                name="occurred_before",
            )
            _validate_date_window(parsed_occurred_after, parsed_occurred_before)
            return asyncio.run(
                self._search_async(
                    query=normalized_query,
                    limit=_clamp(limit, minimum=1, maximum=MAX_SEARCH_LIMIT),
                    source=normalized_source,
                    source_type=normalized_source_type,
                    occurred_after=parsed_occurred_after,
                    occurred_before=parsed_occurred_before,
                )
            )
        except Exception as exc:
            _emit_company_context_lookup_metrics(
                status="error",
                requested_source=normalized_source,
                requested_source_type=normalized_source_type,
                occurred_after=None,
                occurred_before=None,
                results=[],
            )
            return {"status": "error", "error": str(exc)}

    async def _search_dm_conversations_async(
        self,
        *,
        query: str,
        limit: int,
    ) -> dict[str, Any]:
        conn = await self._connect()
        try:
            terms = _search_terms(query)
            search_terms = [query, *terms]
            limit_param = len(search_terms) + 1
            rows = await conn.fetch(
                f"""
                SELECT
                    document_id,
                    home_team_id,
                    conversation_id,
                    conversation_type,
                    title,
                    body,
                    is_ext_shared,
                    last_seen_at,
                    source_updated_at,
                    participant_user_ids,
                    participant_labels,
                    participant_count,
                    metadata,
                    paradedb.score(document_id) AS score
                FROM slack_private_conversation_context_documents
                WHERE {_search_where_clause(len(terms))}
                ORDER BY paradedb.score(document_id) DESC,
                         last_seen_at DESC NULLS LAST,
                         source_updated_at DESC NULLS LAST
                LIMIT ${limit_param}
                """,
                *search_terms,
                limit,
            )
            results = []
            query_terms = [term.lower() for term in _search_terms(query)]
            for row in rows:
                result = _dm_conversation_summary(row)
                result["score"] = float(_row_value(row, "score", 0.0) or 0.0)
                result["preview"] = _body_preview(
                    str(_row_value(row, "body", "") or ""),
                    query=query,
                )
                result["matched_labels"] = [
                    label
                    for label in result["participant_labels"]
                    if any(term in str(label).lower() for term in query_terms)
                ]
                results.append(result)
            return {
                "status": "ok",
                "query": query,
                "source": SLACK_DM_SOURCE,
                "count": len(results),
                "results": results,
            }
        finally:
            await conn.close()

    def search_dm_conversations(
        self,
        query: str,
        limit: int = DEFAULT_SEARCH_LIMIT,
    ) -> dict:
        """Find Slack DM and group DM conversations visible to the current Slack user."""
        normalized_query = query.strip()
        if not normalized_query:
            return {"status": "error", "error": "query cannot be empty"}

        try:
            return asyncio.run(
                self._search_dm_conversations_async(
                    query=normalized_query,
                    limit=_clamp(limit, minimum=1, maximum=MAX_SEARCH_LIMIT),
                )
            )
        except Exception as exc:
            return {"status": "error", "error": str(exc)}

    async def _search_dms_async(
        self,
        *,
        query: str,
        limit: int,
        conversation_id: str | None,
        occurred_after: datetime | None,
        occurred_before: datetime | None,
    ) -> dict[str, Any]:
        conn = await self._connect()
        try:
            terms = _search_terms(query)
            search_terms = [query, *terms]
            conversation_id_param = len(search_terms) + 1
            occurred_after_param = len(search_terms) + 2
            occurred_before_param = len(search_terms) + 3
            limit_param = len(search_terms) + 4
            rows = await conn.fetch(
                f"""
                SELECT
                    document_id,
                    home_team_id,
                    conversation_id,
                    message_ts,
                    conversation_type,
                    thread_ts,
                    parent_message_ts,
                    is_thread_root,
                    user_id,
                    user_team_id,
                    bot_id,
                    message_type,
                    message_subtype,
                    title,
                    body,
                    permalink,
                    occurred_at,
                    source_updated_at,
                    metadata,
                    paradedb.score(document_id) AS score
                FROM slack_private_context_documents
                WHERE {_search_where_clause(len(terms))}
                  AND (${conversation_id_param}::text IS NULL
                       OR conversation_id = ${conversation_id_param})
                  AND (${occurred_after_param}::timestamptz IS NULL
                       OR occurred_at >= ${occurred_after_param})
                  AND (${occurred_before_param}::timestamptz IS NULL
                       OR occurred_at < ${occurred_before_param})
                ORDER BY paradedb.score(document_id) DESC,
                         occurred_at DESC NULLS LAST,
                         source_updated_at DESC NULLS LAST
                LIMIT ${limit_param}
                """,
                *search_terms,
                conversation_id,
                occurred_after,
                occurred_before,
                limit,
            )
            results = []
            for row in rows:
                result = _dm_document_summary(row)
                result["score"] = float(_row_value(row, "score", 0.0) or 0.0)
                result["preview"] = _body_preview(
                    str(_row_value(row, "body", "") or ""),
                    query=query,
                )
                result["lane"] = "indexed"
                result["result_type"] = str(result["source_type"] or SLACK_DM_SOURCE)
                results.append(result)

            return {
                "status": "ok",
                "query": query,
                "source": SLACK_DM_SOURCE,
                "conversation_id": conversation_id,
                "occurred_after": _isoformat(occurred_after),
                "occurred_before": _isoformat(occurred_before),
                "count": len(results),
                "results": results,
            }
        finally:
            await conn.close()

    def search_dms(
        self,
        query: str,
        limit: int = DEFAULT_SEARCH_LIMIT,
        conversation_id: str | None = None,
        occurred_after: str | datetime | None = None,
        occurred_before: str | datetime | None = None,
    ) -> dict:
        """Search private Slack context visible to the current Slack user."""
        normalized_query = query.strip()
        if not normalized_query:
            return {"status": "error", "error": "query cannot be empty"}

        try:
            parsed_occurred_after = _parse_datetime_filter(
                occurred_after,
                name="occurred_after",
            )
            parsed_occurred_before = _parse_datetime_filter(
                occurred_before,
                name="occurred_before",
            )
            _validate_date_window(parsed_occurred_after, parsed_occurred_before)
            return asyncio.run(
                self._search_dms_async(
                    query=normalized_query,
                    limit=_clamp(limit, minimum=1, maximum=MAX_SEARCH_LIMIT),
                    conversation_id=conversation_id.strip() if conversation_id else None,
                    occurred_after=parsed_occurred_after,
                    occurred_before=parsed_occurred_before,
                )
            )
        except Exception as exc:
            return {"status": "error", "error": str(exc)}

    async def _list_documents_async(
        self,
        *,
        limit: int,
        source: str | None,
        source_type: str | None,
        occurred_after: datetime | None,
        occurred_before: datetime | None,
    ) -> dict[str, Any]:
        conn = await self._connect()
        try:
            results = []
            google_docs_error = None
            granola_error = None
            company_source, company_source_type = _company_context_filters_for_source(
                source,
                source_type,
            )
            rows = await conn.fetch(
                """
                SELECT
                    document_id,
                    source,
                    source_type,
                    source_document_id,
                    source_chunk_id,
                    parent_document_id,
                    title,
                    url,
                    author_name,
                    access_scope,
                    body,
                    occurred_at,
                    source_updated_at,
                    metadata
                FROM company_context_documents
                WHERE ($1::text IS NULL OR source = $1)
                  AND ($2::text IS NULL OR source_type = $2)
                  AND ($3::timestamptz IS NULL OR occurred_at >= $3)
                  AND ($4::timestamptz IS NULL OR occurred_at < $4)
                ORDER BY occurred_at ASC NULLS LAST, source_updated_at DESC NULLS LAST,
                         document_id ASC
                LIMIT $5
                """,
                company_source,
                company_source_type,
                occurred_after,
                occurred_before,
                limit,
            )
            for row in rows:
                result = _document_summary(row)
                result["preview"] = _body_preview(
                    str(_row_value(row, "body", "") or ""),
                    query="",
                )
                results.append(result)
            if _include_google_docs_source(source, source_type):
                try:
                    google_rows = await self._list_google_docs_async(
                        conn,
                        limit=limit,
                        modified_after=occurred_after,
                        modified_before=occurred_before,
                    )
                    for row in google_rows:
                        result = _google_doc_summary(row)
                        result["preview"] = _body_preview(
                            str(_row_value(row, "body", "") or ""),
                            query="",
                        )
                        results.append(result)
                except asyncpg.UndefinedTableError as exc:
                    google_docs_error = str(exc)
            if _include_granola_source(source, source_type):
                try:
                    granola_rows = await self._list_granola_async(
                        conn,
                        limit=limit,
                        occurred_after=occurred_after,
                        occurred_before=occurred_before,
                    )
                    for row in granola_rows:
                        result = _granola_doc_summary(row)
                        result["preview"] = _body_preview(
                            str(_row_value(row, "body", "") or ""),
                            query="",
                        )
                        results.append(result)
                except asyncpg.UndefinedTableError as exc:
                    granola_error = str(exc)
            results.sort(
                key=lambda item: (
                    str(item.get("occurred_at") or ""),
                    str(item.get("source_updated_at") or ""),
                    str(item.get("document_id") or ""),
                )
            )
            results = results[:limit]
            response = {
                "status": "ok",
                "source": source,
                "source_type": source_type,
                "occurred_after": _isoformat(occurred_after),
                "occurred_before": _isoformat(occurred_before),
                "count": len(results),
                "results": results,
            }
            if google_docs_error:
                response["google_docs_error"] = google_docs_error
            if granola_error:
                response["granola_error"] = granola_error
            return response
        finally:
            await conn.close()

    async def _list_google_docs_async(
        self,
        conn: asyncpg.Connection,
        *,
        limit: int,
        modified_after: datetime | None,
        modified_before: datetime | None,
    ) -> list[Any]:
        return await conn.fetch(
            """
            SELECT
                document_id,
                file_id,
                chunk_id,
                title,
                body,
                url,
                provider_author_id,
                provider_author_name,
                mime_type,
                drive_id,
                source_created_at,
                source_modified_at,
                metadata
            FROM google_docs_context_documents
            WHERE ($1::timestamptz IS NULL OR source_modified_at >= $1)
              AND ($2::timestamptz IS NULL OR source_modified_at < $2)
            ORDER BY source_modified_at DESC NULLS LAST, source_created_at DESC NULLS LAST,
                     document_id ASC
            LIMIT $3
            """,
            modified_after,
            modified_before,
            limit,
        )

    async def _list_granola_async(
        self,
        conn: asyncpg.Connection,
        *,
        limit: int,
        occurred_after: datetime | None,
        occurred_before: datetime | None,
    ) -> list[Any]:
        return await conn.fetch(
            """
            SELECT
                document_id,
                note_id,
                title,
                body,
                url,
                owner_id,
                owner_email,
                owner_name,
                access_emails,
                attendee_labels,
                occurred_at,
                source_updated_at,
                metadata
            FROM granola_context_documents
            WHERE ($1::timestamptz IS NULL OR occurred_at >= $1)
              AND ($2::timestamptz IS NULL OR occurred_at < $2)
            ORDER BY occurred_at DESC NULLS LAST, source_updated_at DESC NULLS LAST,
                     document_id ASC
            LIMIT $3
            """,
            occurred_after,
            occurred_before,
            limit,
        )

    def list_documents(
        self,
        limit: int = DEFAULT_SEARCH_LIMIT,
        source: str | None = None,
        source_type: str | None = None,
        occurred_after: str | datetime | None = None,
        occurred_before: str | datetime | None = None,
    ) -> dict:
        """List company context documents, optionally bounded by occurred_at."""
        try:
            parsed_occurred_after = _parse_datetime_filter(
                occurred_after,
                name="occurred_after",
            )
            parsed_occurred_before = _parse_datetime_filter(
                occurred_before,
                name="occurred_before",
            )
            _validate_date_window(parsed_occurred_after, parsed_occurred_before)
            return asyncio.run(
                self._list_documents_async(
                    limit=_clamp(limit, minimum=1, maximum=MAX_SEARCH_LIMIT),
                    source=source.strip() if source else None,
                    source_type=source_type.strip() if source_type else None,
                    occurred_after=parsed_occurred_after,
                    occurred_before=parsed_occurred_before,
                )
            )
        except Exception as exc:
            return {"status": "error", "error": str(exc)}

    async def _latest_date_async(
        self,
        *,
        source: str | None,
        source_type: str | None,
    ) -> dict[str, Any]:
        conn = await self._connect()
        try:
            indexed = await self._latest_date_for_connection(
                conn,
                source=source,
                source_type=source_type,
            )
            google_docs = self._empty_latest_date_result(source=source, source_type=source_type)
            with suppress(asyncpg.UndefinedTableError):
                google_docs = await self._latest_google_docs_for_connection(
                    conn,
                    source=source,
                    source_type=source_type,
                )
            slack_dms = self._empty_latest_date_result(source=source, source_type=source_type)
            slack_dms = await self._latest_slack_dms_for_connection(
                conn,
                source=source,
                source_type=source_type,
            )
            granola = self._empty_latest_date_result(source=source, source_type=source_type)
            with suppress(asyncpg.UndefinedTableError):
                granola = await self._latest_granola_for_connection(
                    conn,
                    source=source,
                    source_type=source_type,
                )
            return self._merge_latest_dates(
                source=source,
                source_type=source_type,
                indexed=indexed,
                google_docs=google_docs,
                slack_dms=slack_dms,
                granola=granola,
            )
        finally:
            await conn.close()

    def latest_date(self, source: str | None = None, source_type: str | None = None) -> dict:
        """Return the latest indexed timestamp for company context documents."""
        try:
            return asyncio.run(
                self._latest_date_async(
                    source=source.strip() if source else None,
                    source_type=source_type.strip() if source_type else None,
                )
            )
        except Exception as exc:
            return {"status": "error", "error": str(exc)}

    async def _related_documents_async(
        self,
        conn: asyncpg.Connection,
        *,
        row: Any,
        max_children: int,
    ) -> dict[str, Any]:
        parent = None
        if row["parent_document_id"]:
            parent_row = await conn.fetchrow(
                """
                SELECT
                    document_id,
                    source,
                    source_type,
                    source_document_id,
                    source_chunk_id,
                    parent_document_id,
                    title,
                    url,
                    author_name,
                    access_scope,
                    occurred_at,
                    source_updated_at,
                    metadata
                FROM company_context_documents
                WHERE document_id = $1
                """,
                row["parent_document_id"],
            )
            if parent_row:
                parent = _document_summary(parent_row)

        child_rows = await conn.fetch(
            """
            SELECT
                document_id,
                source,
                source_type,
                source_document_id,
                source_chunk_id,
                parent_document_id,
                title,
                url,
                author_name,
                access_scope,
                occurred_at,
                source_updated_at,
                metadata
            FROM company_context_documents
            WHERE parent_document_id = $1
            ORDER BY occurred_at ASC NULLS LAST, document_id ASC
            LIMIT $2
            """,
            row["document_id"],
            max_children,
        )
        children = [_document_summary(child_row) for child_row in child_rows]
        return {
            "parent": parent,
            "children": children,
            "child_count": len(children),
        }

    async def _read_document_async(
        self,
        document_id: str,
        max_chars: int | None,
        *,
        include_related: bool,
        max_related_children: int,
    ) -> dict[str, Any]:
        conn = await self._connect()
        try:
            row = await conn.fetchrow(
                """
                SELECT
                    document_id,
                    source,
                    source_type,
                    source_document_id,
                    source_chunk_id,
                    parent_document_id,
                    title,
                    body,
                    url,
                    author_name,
                    access_scope,
                    occurred_at,
                    source_updated_at,
                    metadata
                FROM company_context_documents
                WHERE document_id = $1
                """,
                document_id,
            )
            if not row:
                try:
                    google_doc = await self._read_google_doc_async(conn, document_id, max_chars)
                except asyncpg.UndefinedTableError:
                    google_doc = None
                if google_doc is not None:
                    return google_doc
                try:
                    granola_doc = await self._read_granola_doc_async(
                        conn,
                        document_id,
                        max_chars,
                    )
                except asyncpg.UndefinedTableError:
                    granola_doc = None
                if granola_doc is not None:
                    return granola_doc
                return {
                    "status": "error",
                    "error": f"document not found: {document_id}",
                }

            body = str(row["body"] or "")
            content = body if max_chars is None else body[:max_chars]
            truncated = max_chars is not None and len(body) > max_chars
            result = {
                "status": "ok",
                **_document_summary(row),
                "chars": len(content),
                "total_chars": len(body),
                "truncated": truncated,
                "content": content,
            }
            if include_related:
                result["related"] = await self._related_documents_async(
                    conn,
                    row=row,
                    max_children=max_related_children,
                )
            return result
        finally:
            await conn.close()

    async def _read_google_doc_async(
        self,
        conn: asyncpg.Connection,
        document_id: str,
        max_chars: int | None,
    ) -> dict[str, Any] | None:
        row = await conn.fetchrow(
            """
            SELECT
                document_id,
                file_id,
                chunk_id,
                title,
                body,
                url,
                provider_author_id,
                provider_author_name,
                mime_type,
                drive_id,
                source_created_at,
                source_modified_at,
                metadata
            FROM google_docs_context_documents
            WHERE document_id = $1
            """,
            document_id,
        )
        if not row:
            return None

        body = str(row["body"] or "")
        content = body if max_chars is None else body[:max_chars]
        truncated = max_chars is not None and len(body) > max_chars
        return {
            "status": "ok",
            **_google_doc_summary(row),
            "chars": len(content),
            "total_chars": len(body),
            "truncated": truncated,
            "content": content,
        }

    async def _read_granola_doc_async(
        self,
        conn: asyncpg.Connection,
        document_id: str,
        max_chars: int | None,
    ) -> dict[str, Any] | None:
        row = await conn.fetchrow(
            """
            SELECT
                document_id,
                note_id,
                title,
                body,
                url,
                owner_id,
                owner_email,
                owner_name,
                access_emails,
                attendee_labels,
                occurred_at,
                source_updated_at,
                metadata
            FROM granola_context_documents
            WHERE document_id = $1
            """,
            document_id,
        )
        if not row:
            return None

        body = str(row["body"] or "")
        content = body if max_chars is None else body[:max_chars]
        truncated = max_chars is not None and len(body) > max_chars
        return {
            "status": "ok",
            **_granola_doc_summary(row),
            "chars": len(content),
            "total_chars": len(body),
            "truncated": truncated,
            "content": content,
        }

    def read_document(
        self,
        document_id: str,
        max_chars: int = 0,
        include_related: bool = False,
        max_related_children: int = MAX_RELATED_CHILDREN,
    ) -> dict:
        """Read a company context document by id, returning full content by default."""
        normalized_document_id = document_id.strip()
        if not normalized_document_id:
            return {"status": "error", "error": "document_id cannot be empty"}

        try:
            return asyncio.run(
                self._read_document_async(
                    document_id=normalized_document_id,
                    max_chars=max_chars if max_chars > 0 else None,
                    include_related=include_related,
                    max_related_children=_clamp(
                        max_related_children,
                        minimum=1,
                        maximum=MAX_RELATED_CHILDREN,
                    ),
                )
            )
        except Exception as exc:
            return {"status": "error", "error": str(exc)}


def _client() -> CompanyContextClient:
    return CompanyContextClient()
