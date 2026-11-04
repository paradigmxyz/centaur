"""Web search over one configurable vendor: Parallel (default), Exa, or Tako.

`WEBSEARCH_BACKEND` selects the vendor. Each call sends the vendor's API key
placeholder for iron-proxy to replace. Parallel falls back to its free
anonymous Search MCP when the key is rejected; Exa and Tako require a key.
"""

from __future__ import annotations

import datetime as dt
import json
import os
from dataclasses import dataclass

import httpx

from centaur_sdk import secret

BACKENDS = ("parallel", "exa", "tako")
DEFAULT_BACKEND = "parallel"
MODES = ("basic", "advanced")

PARALLEL_API_URL = "https://api.parallel.ai/v1/search"
PARALLEL_MCP_URL = "https://search.parallel.ai/mcp"
PARALLEL_ATTRIBUTION = (
    "Search powered by the free Parallel Web Search MCP (https://parallel.ai). "
    "See https://parallel.ai/customer-terms."
)
EXA_API_URL = "https://api.exa.ai/search"
TAKO_API_URL = "https://tako.com/api/v3/search"

PARALLEL_MAX_RESULTS = 40
EXA_MAX_RESULTS = 100
TAKO_MAX_WEB_RESULTS = 20
TAKO_DATA_CARDS = 2
EXA_TYPES = {"basic": "fast", "advanced": "auto"}
TAKO_EFFORTS = {"basic": "instant", "advanced": "fast"}


class KeyRejected(RuntimeError):
    pass


@dataclass(frozen=True)
class Query:
    text: str
    num_results: int
    include_domains: list[str]
    exclude_domains: list[str]
    max_age_hours: int | None
    mode: str | None
    max_chars_total: int | None
    session_id: str | None

    def published_after(self) -> dt.datetime | None:
        if not self.max_age_hours:
            return None
        return dt.datetime.now(dt.UTC) - dt.timedelta(hours=self.max_age_hours)

    def ignored(self, backend: str, *names: str) -> list[str]:
        used = [name for name in names if getattr(self, name)]
        return [f"{backend} ignores {', '.join(used)}."] if used else []


@dataclass
class Page:
    backend: str
    request_id: str | None
    results: list[dict]
    warnings: list[str]
    attribution: str | None = None


def _result(title: str | None, url: str, snippet: str | None, published: str | None) -> dict:
    return {
        "title": title or url,
        "url": url,
        "snippet": snippet or "",
        "published_date": published,
    }


def _post(http: httpx.Client, url: str, body: dict, *, key: str, header: str) -> httpx.Response:
    response = http.post(url, json=body, headers={header: secret(key, key)})
    if response.status_code in (401, 403):
        raise KeyRejected(
            f"{key} was rejected ({response.status_code}); configure and grant it for this backend."
        )
    if response.is_error:
        raise RuntimeError(f"{url} returned {response.status_code}: {response.text[:500]}")
    return response


def _search_parallel(http: httpx.Client, q: Query) -> Page:
    body: dict = {
        "objective": q.text,
        "search_queries": [q.text],
        "advanced_settings": {"max_results": min(q.num_results, PARALLEL_MAX_RESULTS)},
    }
    policy: dict = {}
    if q.include_domains:
        policy["include_domains"] = q.include_domains
    if q.exclude_domains:
        policy["exclude_domains"] = q.exclude_domains
    if after := q.published_after():
        policy["after_date"] = after.date().isoformat()
    if policy:
        body["advanced_settings"]["source_policy"] = policy
    for name in ("mode", "max_chars_total", "session_id"):
        if value := getattr(q, name):
            body[name] = value
    try:
        response = _post(http, PARALLEL_API_URL, body, key="PARALLEL_API_KEY", header="x-api-key")
    except KeyRejected:
        return _search_parallel_free(http, q)
    data = response.json()
    return Page("parallel:api", data.get("search_id"), _parallel_results(data), [])


def _search_parallel_free(http: httpx.Client, q: Query) -> Page:
    arguments = {"objective": q.text, "search_queries": [q.text]}
    if q.session_id:
        arguments["session_id"] = q.session_id
    response = http.post(
        PARALLEL_MCP_URL,
        headers={"Accept": "application/json, text/event-stream"},
        json={
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "web_search", "arguments": arguments},
        },
    )
    if response.is_error:
        raise RuntimeError(
            f"Parallel Search MCP returned {response.status_code}: {response.text[:500]}"
        )
    message = _jsonrpc_message(response)
    if "error" in message:
        raise RuntimeError(f"Parallel Search MCP error: {str(message['error'])[:500]}")
    result = message.get("result") or {}
    if result.get("isError"):
        raise RuntimeError(f"Parallel Search MCP tool error: {str(result.get('content'))[:500]}")
    data = result.get("structuredContent") or json.loads(result["content"][0]["text"])
    warnings = q.ignored(
        "Free Parallel search (PARALLEL_API_KEY was rejected)",
        "include_domains",
        "exclude_domains",
        "max_age_hours",
        "mode",
    )
    return Page(
        "parallel:free",
        data.get("search_id"),
        _parallel_results(data),
        warnings,
        PARALLEL_ATTRIBUTION,
    )


def _jsonrpc_message(response: httpx.Response) -> dict:
    if "text/event-stream" not in response.headers.get("content-type", ""):
        return response.json()
    data_lines = [
        line[5:].strip() for line in response.text.splitlines() if line.startswith("data:")
    ]
    if not data_lines:
        raise RuntimeError("Parallel Search MCP returned an empty event stream.")
    return json.loads(data_lines[-1])


def _parallel_results(data: dict) -> list[dict]:
    return [
        _result(
            item.get("title"),
            item["url"],
            "\n\n".join(item.get("excerpts") or []),
            item.get("publish_date"),
        )
        for item in data.get("results") or []
    ]


def _search_exa(http: httpx.Client, q: Query) -> Page:
    body: dict = {
        "query": q.text,
        "numResults": min(q.num_results, EXA_MAX_RESULTS),
        "contents": {"highlights": True},
    }
    if q.mode:
        body["type"] = EXA_TYPES[q.mode]
    if q.include_domains:
        body["includeDomains"] = q.include_domains
    if q.exclude_domains:
        body["excludeDomains"] = q.exclude_domains
    if after := q.published_after():
        body["startPublishedDate"] = after.isoformat(timespec="seconds")
    data = _post(http, EXA_API_URL, body, key="EXA_API_KEY", header="x-api-key").json()
    results = [
        _result(
            item.get("title"),
            item["url"],
            "\n\n".join(item.get("highlights") or []) or item.get("text"),
            item.get("publishedDate"),
        )
        for item in data.get("results") or []
    ]
    return Page("exa", data.get("requestId"), results, q.ignored("Exa", "session_id"))


def _search_tako(http: httpx.Client, q: Query) -> Page:
    web: dict = {"count": min(q.num_results, TAKO_MAX_WEB_RESULTS), "highlights": True}
    if q.include_domains:
        web["include_domains"] = q.include_domains
    if q.exclude_domains:
        web["exclude_domains"] = q.exclude_domains
    if after := q.published_after():
        web["published_after"] = after.date().isoformat()
    body: dict = {
        "query": q.text,
        "sources": {"data": {"count": min(q.num_results, TAKO_DATA_CARDS)}, "web": web},
    }
    if q.mode:
        body["effort"] = TAKO_EFFORTS[q.mode]
    data = _post(http, TAKO_API_URL, body, key="TAKO_API_KEY", header="X-API-Key").json()
    results = [
        _result(
            card.get("title"),
            card["webpage_url"],
            card.get("description"),
            (card.get("data_freshness") or {}).get("last_updated"),
        )
        for card in data.get("cards") or []
        if card.get("webpage_url")
    ]
    results += [
        _result(item.get("title"), item["url"], item.get("snippet"), item.get("publish_date"))
        for item in data.get("web_results") or []
    ]
    return Page("tako", data.get("request_id"), results, q.ignored("Tako", "session_id"))


SEARCHES = {"parallel": _search_parallel, "exa": _search_exa, "tako": _search_tako}


def _dedupe_and_trim(
    results: list[dict], num_results: int, max_chars_total: int | None
) -> list[dict]:
    unique: dict[str, dict] = {}
    for result in results:
        unique.setdefault(result["url"], result)
    trimmed = list(unique.values())[:num_results]
    if max_chars_total is None:
        return trimmed
    remaining = max_chars_total
    for result in trimmed:
        result["snippet"] = result["snippet"][: max(remaining, 0)]
        remaining -= len(result["snippet"])
    return trimmed


def _unsupported_warning(arguments: dict[str, object]) -> list[str]:
    used = sorted(name for name, value in arguments.items() if value is not None)
    return [f"websearch ignores unsupported arguments: {', '.join(used)}."] if used else []


def _backend_name(backend: str | None) -> str:
    configured = os.environ.get("WEBSEARCH_BACKEND")  # noqa: TID251  # deployment config, not a secret
    name = (backend or configured or DEFAULT_BACKEND).strip().lower()
    if name not in BACKENDS:
        raise ValueError(f"backend must be one of {', '.join(BACKENDS)} (got {name!r}).")
    return name


class WebSearchClient:
    """Web search through Parallel, Exa, or Tako."""

    def __init__(self, *, transport: httpx.BaseTransport | None = None) -> None:
        self._transport = transport

    def search(
        self,
        query: str,
        *,
        num_results: int = 10,
        include_domains: list[str] | None = None,
        exclude_domains: list[str] | None = None,
        max_age_hours: int | None = None,
        mode: str | None = None,
        max_chars_total: int | None = None,
        session_id: str | None = None,
        timeout_seconds: float = 60.0,
        backend: str | None = None,
        **unsupported: object,
    ) -> dict:
        """Search the web and return `{query, results, meta}`.

        `backend` is `parallel`, `exa`, or `tako`; it defaults to
        `WEBSEARCH_BACKEND`, then `parallel`. Each result has `title`, `url`, `snippet`, and `published_date`.
        `mode` is `basic` (faster) or `advanced`. `max_age_hours` is rounded
        down to a UTC date on Parallel and Tako. `session_id` is Parallel-only.
        Flags a backend cannot honor, and any other keyword arguments (such as
        the retired `synthesize`), are ignored and listed in `meta.warnings`.
        """
        name = _backend_name(backend)
        text = query.strip()
        if not text:
            raise ValueError("query cannot be empty.")
        if num_results < 1:
            raise ValueError("num_results must be at least 1.")
        if mode is not None and mode not in MODES:
            raise ValueError(f"mode must be one of {', '.join(MODES)} (got {mode!r}).")
        q = Query(
            text=text,
            num_results=num_results,
            include_domains=list(include_domains or []),
            exclude_domains=list(exclude_domains or []),
            max_age_hours=max_age_hours,
            mode=mode,
            max_chars_total=max_chars_total,
            session_id=session_id,
        )
        with httpx.Client(timeout=timeout_seconds, transport=self._transport) as http:
            page = SEARCHES[name](http, q)
        meta: dict = {
            "backend": page.backend,
            "request_id": page.request_id,
            "warnings": page.warnings + _unsupported_warning(unsupported),
        }
        if page.attribution:
            meta["attribution"] = page.attribution
        return {
            "query": text,
            "results": _dedupe_and_trim(page.results, num_results, max_chars_total),
            "meta": meta,
        }


def _client() -> WebSearchClient:
    """Factory for the centaur tool loader."""
    return WebSearchClient()
