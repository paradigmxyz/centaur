"""Fetch historical company context documents."""

from __future__ import annotations

import asyncio
import importlib.util
import json
import os
import re
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import asyncpg

from centaur_sdk.tool_sdk import get_tool_context, secret

DEFAULT_SEARCH_LIMIT = 10
MAX_SEARCH_LIMIT = 50
TITLE_MATCH_BOOST = 4
EXACT_QUERY_TITLE_BOOST = 8
EXACT_QUERY_BODY_BOOST = 2
THREAD_SCORE_MULTIPLIER = 1.25
CHANNEL_DAY_SCORE_MULTIPLIER = 0.75
DEFAULT_PREVIEW_CHARS = 280
MAX_RELATED_CHILDREN = 25
SLACK_LIVE_SOURCE_TYPE = "slack_live_message"
COMPANY_CONTEXT_DSN_ENV = "COMPANY_CONTEXT_DSN"
_SLACK_AFTER_RE = re.compile(r"\bafter:\d{4}-\d{2}-\d{2}\b", re.IGNORECASE)

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
    value = os.getenv(COMPANY_CONTEXT_DSN_ENV)
    if value is None:
        value = secret(COMPANY_CONTEXT_DSN_ENV, default="")
    value = value.strip()
    if value == COMPANY_CONTEXT_DSN_ENV:
        return ""
    return value


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
            f"(title ||| ${index}::text::pdb.boost({TITLE_MATCH_BOOST}) OR body ||| ${index})"
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


def _slack_ts_to_iso(ts: str | None) -> str | None:
    """Convert a Slack timestamp string to ISO 8601 when possible."""
    if not ts:
        return None
    try:
        return datetime.fromtimestamp(float(ts), tz=UTC).isoformat()
    except (TypeError, ValueError, OSError):
        return None


def _slack_after_query(query: str, latest_date: str | None) -> str:
    """Append a Slack after:YYYY-MM-DD modifier unless the query already has one."""
    if not latest_date or _SLACK_AFTER_RE.search(query):
        return query
    return f"{query} after:{latest_date[:10]}"


def _current_slack_channel_id() -> str | None:
    """Return the channel/group id for the active Slack thread, or None for DMs."""
    try:
        thread_key = get_tool_context().thread_key
    except LookupError:
        return None
    if not thread_key:
        return None
    for segment in str(thread_key).split(":")[1:]:
        clean = segment.strip()
        if clean.startswith(("C", "G")):
            return clean
        if clean.startswith("D"):
            return None
    return None


async def _is_etl_admin_channel(conn: asyncpg.Connection) -> bool:
    value = await conn.fetchval("SELECT centaur_etl_admin_channel()")
    return bool(value)


def _load_slack_client() -> Any:
    """Load the sibling Slack tool client without making company_context import it eagerly."""
    candidate_roots = [
        Path("/app/tools/productivity/slack"),
        Path(__file__).resolve().parent.parent / "slack",
    ]
    slack_dir = next((path for path in candidate_roots if (path / "client.py").exists()), None)
    if slack_dir is None:
        raise RuntimeError("slack tool client not found")

    module_name = "_company_context_slack_client"
    spec = importlib.util.spec_from_file_location(module_name, slack_dir / "client.py")
    if spec is None or spec.loader is None:
        raise RuntimeError("failed to load slack tool client")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module._client()


def _live_slack_result(message: dict[str, Any]) -> dict[str, Any]:
    """Normalize slack.search_messages results into company_context search result shape."""
    channel = str(message.get("channel") or "")
    user = str(message.get("user") or "")
    timestamp = str(message.get("timestamp") or "")
    title_bits = []
    if channel:
        title_bits.append(f"#{channel}")
    if user:
        title_bits.append(f"from {user}")
    return {
        "document_id": "",
        "source": "slack",
        "source_type": SLACK_LIVE_SOURCE_TYPE,
        "source_document_id": str(message.get("thread_ts") or timestamp),
        "source_chunk_id": timestamp,
        "parent_document_id": None,
        "title": " ".join(title_bits) or "Slack message",
        "url": str(message.get("permalink") or ""),
        "author_name": user,
        "access_scope": "",
        "score": None,
        "preview": str(message.get("text") or ""),
        "occurred_at": _slack_ts_to_iso(timestamp),
        "source_updated_at": None,
        "lane": "live",
        "result_type": SLACK_LIVE_SOURCE_TYPE,
        "metadata": {
            "channel_name": channel,
            "channel_id": str(message.get("channel_id") or ""),
            "user_name": user,
            "user_id": str(message.get("user_id") or ""),
            "message_ts": timestamp,
            "thread_ts": message.get("thread_ts"),
            "reply_count": int(message.get("reply_count") or 0),
        },
    }


class CompanyContextClient:
    """Query the shared company context document table."""

    def __init__(self, database_url: str | None = None) -> None:
        self._database_url = (database_url or _scoped_database_url()).strip()

    def _require_database_url(self) -> str:
        if not self._database_url:
            raise RuntimeError(f"{COMPANY_CONTEXT_DSN_ENV} is required for company context search")
        return self._database_url

    async def _connect(self) -> asyncpg.Connection:
        return await asyncpg.connect(self._require_database_url(), command_timeout=30)

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
                source,
                source_type,
                occurred_after,
                occurred_before,
                limit,
            )
            results = []
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

            latest = None
            live_results: list[dict[str, Any]] = []
            live_error = None
            should_search_live_slack = source == "slack" and (
                source_type is None or source_type.startswith("slack")
            )
            should_search_live_slack = (
                should_search_live_slack and occurred_after is None and occurred_before is None
            )
            if should_search_live_slack:
                latest = await self._latest_date_for_connection(
                    conn,
                    source="slack",
                    source_type=source_type,
                )
                try:
                    slack_channel_id = _current_slack_channel_id()
                    if slack_channel_id is None:
                        live_error = "live Slack search requires a channel-scoped thread"
                    else:
                        live_channels = None if await _is_etl_admin_channel(conn) else [slack_channel_id]
                        live_query = _slack_after_query(query, latest.get("latest_date"))
                        live_messages = _load_slack_client().search_messages(
                            live_query,
                            max_results=limit,
                            channels=live_channels,
                        )
                        live_results = [_live_slack_result(message) for message in live_messages]
                except Exception as exc:
                    live_error = str(exc)

            return {
                "status": "ok",
                "query": query,
                "source": source,
                "source_type": source_type,
                "occurred_after": _isoformat(occurred_after),
                "occurred_before": _isoformat(occurred_before),
                "count": len(results) + len(live_results),
                "indexed_count": len(results),
                "live_count": len(live_results),
                "indexed_cutoff": latest.get("latest_date") if latest else None,
                "latest_source_updated_at": (
                    latest.get("latest_source_updated_at") if latest else None
                ),
                "latest_occurred_at": latest.get("latest_occurred_at") if latest else None,
                "live_error": live_error,
                "results": [*results, *live_results],
            }
        finally:
            await conn.close()

    async def _latest_date_for_connection(
        self,
        conn: asyncpg.Connection,
        *,
        source: str | None,
        source_type: str | None,
    ) -> dict[str, Any]:
        """Return latest indexed date using an existing DB connection."""
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
            source,
            source_type,
        )
        if not row or int(row["document_count"] or 0) == 0:
            return {
                "status": "ok",
                "source": source,
                "source_type": source_type,
                "document_count": 0,
                "latest_date": None,
                "latest_source_updated_at": None,
                "latest_occurred_at": None,
            }
        return {
            "status": "ok",
            "source": source,
            "source_type": source_type,
            "document_count": int(row["document_count"] or 0),
            "latest_date": _isoformat(row["latest_date"]),
            "latest_source_updated_at": _isoformat(row["latest_source_updated_at"]),
            "latest_occurred_at": _isoformat(row["latest_occurred_at"]),
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
        """Search company context documents and return candidate document ids."""
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
                self._search_async(
                    query=normalized_query,
                    limit=_clamp(limit, minimum=1, maximum=MAX_SEARCH_LIMIT),
                    source=source.strip() if source else None,
                    source_type=source_type.strip() if source_type else None,
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
                source,
                source_type,
                occurred_after,
                occurred_before,
                limit,
            )
            results = []
            for row in rows:
                result = _document_summary(row)
                result["preview"] = _body_preview(str(_row_value(row, "body", "") or ""), query="")
                results.append(result)
            return {
                "status": "ok",
                "source": source,
                "source_type": source_type,
                "occurred_after": _isoformat(occurred_after),
                "occurred_before": _isoformat(occurred_before),
                "count": len(results),
                "results": results,
            }
        finally:
            await conn.close()

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
            return await self._latest_date_for_connection(
                conn,
                source=source,
                source_type=source_type,
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
