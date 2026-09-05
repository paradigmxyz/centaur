"""Tako backend.

`search` calls `POST /api/v3/search` through the Tako SDK with the `TAKO_API_KEY`
placeholder and, when that returns 401 or 403, the anonymous `tako_search` tool
on `mcp.tako.com`. `deep_research` runs the Tako Answer Agent over the SDK's SSE
stream (keyed only). Every transport is `httpx`, which honors `HTTPS_PROXY` so
iron-proxy can rewrite the header.

The SDK's `tako.aio` lane is the async client; its models are distinct classes
from the sync `tako.models`, so nothing here imports the sync lane.
"""

from __future__ import annotations

import asyncio
import datetime as dt
import uuid
from collections.abc import Callable
from typing import Any
from urllib.parse import urlparse

import httpx
from pydantic import BaseModel, ConfigDict, Field
from tako.aio import Configuration
from tako.aio.api.agent_api import AgentApi
from tako.aio.api.tako_api import TakoApi
from tako.aio.exceptions import ApiException
from tako.aio.models import (
    AgentRunStatus,
    AnswerAgentEffort,
    AnswerAgentResult,
    AnswerAgentRun,
    AnswerAgentRunRequest,
    AnswerAgentStreamEnvelope,
    DataSourceSettings,
    RunSummaryEvent,
    SearchEffortLevel,
    SearchRequest,
    SearchResponse,
    Sources,
    StatusEvent,
    SubagentEvent,
    TakoCard,
    ToolCallEvent,
    ToolRetryEvent,
    Usage,
    WebResult,
    WebSourceSettings,
)
from tako.lib.agent import AsyncAnswerAgentResource
from tako.lib.caller import stamped_async_api_client

from ._common import (
    USER_AGENT,
    append_within_budget,
    decode_jsonrpc_response,
    render_sources_block,
)
from .models import (
    DeepResearchResult,
    DeepResearchSpec,
    RetrievalResult,
    SearchRequestSpec,
    SourceDocument,
)

API_BASE_URL = "https://tako.com"
API_PATH_PREFIX = "/api"
MCP_URL = "https://mcp.tako.com/mcp"

DEFAULT_SEARCH_EFFORT = "fast"
DEFAULT_RESEARCH_EFFORT = "medium"
DEFAULT_DEEP_RESEARCH_TIMEOUT_SECONDS = 600.0
STREAM_READ_TIMEOUT_SECONDS = 120.0
POLL_INTERVAL_SECONDS = 5.0
SNIPPET_CHAR_LIMIT = 7000
MAX_SOURCE_COUNT = 20
DATA_CARD_COUNT = 2
MAX_DOMAIN_FILTERS = 20
FALLBACK_CITATION_URL = "https://tako.com"
RATE_LIMIT_KINDS = {"rate_limited", "global_rate_limited"}
NOT_GRANTED_STATUSES = {401, 403}
TERMINAL_RUN_STATUSES = {AgentRunStatus.COMPLETED, AgentRunStatus.FAILED}
SEARCH_PRICE_USD: dict[str, float] = {"instant": 0.007, "fast": 0.007, "deep": 0.012}


class ProjectedCard(BaseModel):
    """A card as the anonymous MCP worker projects it (slimmed, renamed fields)."""

    title: str | None = None
    description: str | None = None
    url: str | None = None
    source: str | None = None
    last_updated: str | None = None


class ProjectedWebResult(BaseModel):
    title: str | None = None
    url: str
    snippet: str | None = None
    source: str | None = None
    published: str | None = None


class AnonymousSearchOutput(BaseModel):
    """`tako_search` structuredContent from the anonymous MCP worker."""

    cards: list[ProjectedCard] = Field(default_factory=list)
    web_results: list[ProjectedWebResult] = Field(default_factory=list)
    usage: Usage | None = None
    metric_definitions: dict[str, str] = Field(default_factory=dict)
    source_notes: dict[str, str] = Field(default_factory=dict)


class McpToolResult(BaseModel):
    model_config = ConfigDict(populate_by_name=True)

    content: list[dict[str, Any]] = Field(default_factory=list)
    structured_content: dict[str, Any] | None = Field(default=None, alias="structuredContent")
    is_error: bool = Field(default=False, alias="isError")
    meta: dict[str, Any] = Field(default_factory=dict, alias="_meta")

    def error_kind(self) -> str | None:
        tako_error = self.meta.get("tako/error")
        return tako_error.get("kind") if isinstance(tako_error, dict) else None

    def text(self) -> str:
        return " ".join(str(block.get("text", "")) for block in self.content).strip()


class JsonRpcError(BaseModel):
    code: int
    message: str
    data: dict[str, Any] | None = None


class JsonRpcResponse(BaseModel):
    result: McpToolResult | None = None
    error: JsonRpcError | None = None


class NotGranted(Exception):
    """The placeholder was not rewritten: this principal has no TAKO_API_KEY grant."""


def _host(url: str) -> str | None:
    return urlparse(url).netloc or None


def _join_lines(lines: list[str]) -> str:
    return "\n".join(line for line in lines if line)[:SNIPPET_CHAR_LIMIT]


def _card_snippet(card: TakoCard) -> str:
    lines = [(card.description or card.semantic_description or "").strip()]
    for metric in card.metric_definitions or []:
        lines.append(f"{metric.name}: {metric.definition}")
    for method in card.methodologies or []:
        if method.methodology_name and method.methodology_description:
            lines.append(f"{method.methodology_name}: {method.methodology_description}")
    return _join_lines(lines)


def _card_domain(card: TakoCard, url: str) -> str | None:
    names = [source.source_name for source in card.sources or [] if source.source_name]
    return ", ".join(names) if names else _host(url)


def card_to_source(card: TakoCard, source_id: int) -> SourceDocument | None:
    if not card.webpage_url:
        return None
    return SourceDocument(
        source_id=source_id,
        title=card.title or card.webpage_url,
        url=card.webpage_url,
        snippet=_card_snippet(card),
        published_date=card.data_freshness.last_updated if card.data_freshness else None,
        domain=_card_domain(card, card.webpage_url),
    )


def _web_to_source(item: WebResult, source_id: int) -> SourceDocument:
    return SourceDocument(
        source_id=source_id,
        title=item.title or item.url,
        url=item.url,
        snippet=(item.snippet or "")[:SNIPPET_CHAR_LIMIT],
        published_date=item.publish_date,
        domain=item.source_name or _host(item.url),
    )


def normalize_search_response(payload: SearchResponse) -> list[SourceDocument]:
    """Cards first, then web results, deduplicated by URL and numbered by position."""
    sources: list[SourceDocument] = []
    seen: set[str] = set()
    for card in payload.cards or []:
        document = card_to_source(card, len(sources))
        if document is None or document.url in seen:
            continue
        seen.add(document.url)
        sources.append(document)
    for item in payload.web_results or []:
        if item.url in seen:
            continue
        seen.add(item.url)
        sources.append(_web_to_source(item, len(sources)))
    return sources


def normalize_anonymous_output(payload: AnonymousSearchOutput) -> list[SourceDocument]:
    """Same order as `normalize_search_response`, from the worker's projected shape."""
    definition_lines = [f"{name}: {text}" for name, text in payload.metric_definitions.items()]
    sources: list[SourceDocument] = []
    seen: set[str] = set()
    for card in payload.cards:
        if not card.url or card.url in seen:
            continue
        lines = [(card.description or "").strip(), *definition_lines]
        if card.source and card.source in payload.source_notes:
            lines.append(f"{card.source}: {payload.source_notes[card.source]}")
        seen.add(card.url)
        sources.append(
            SourceDocument(
                source_id=len(sources),
                title=card.title or card.url,
                url=card.url,
                snippet=_join_lines(lines),
                published_date=card.last_updated,
                domain=card.source or _host(card.url),
            )
        )
    for item in payload.web_results:
        if item.url in seen:
            continue
        seen.add(item.url)
        sources.append(
            SourceDocument(
                source_id=len(sources),
                title=item.title or item.url,
                url=item.url,
                snippet=(item.snippet or "")[:SNIPPET_CHAR_LIMIT],
                published_date=item.published,
                domain=item.source or _host(item.url),
            )
        )
    return sources


def _section(heading: str, lines: list[str]) -> str:
    if not lines:
        return ""
    return f"\n\n## {heading}\n" + "\n".join(lines)


def normalize_answer_result(
    result: AnswerAgentResult, *, max_report_chars: int
) -> tuple[list[SourceDocument], str]:
    """Citations keep their `[n]` indexes; uncited cards follow; the report gains
    Charts, Definitions, Assumptions, Methodology, and Sources sections."""
    cards = result.cards or []
    card_url_by_title = {
        card.title: card.webpage_url for card in cards if card.title and card.webpage_url
    }
    sources: list[SourceDocument] = []
    seen_indexes: set[int] = set()
    for citation in result.citations or []:
        if citation.index in seen_indexes:
            continue
        seen_indexes.add(citation.index)
        url = citation.url or card_url_by_title.get(citation.title) or FALLBACK_CITATION_URL
        sources.append(
            SourceDocument(
                source_id=citation.index,
                title=citation.title,
                url=url,
                snippet=(citation.excerpt or "")[:SNIPPET_CHAR_LIMIT],
                published_date=citation.publish_date,
                domain=citation.source_name or _host(url),
            )
        )
    cited_urls = {source.url for source in sources}
    next_id = max((source.source_id for source in sources), default=0) + 1
    for card in cards:
        if not card.webpage_url or card.webpage_url in cited_urls:
            continue
        document = card_to_source(card, next_id)
        if document is None:
            continue
        sources.append(document)
        cited_urls.add(document.url)
        next_id += 1

    body = (result.answer or "").strip()
    body += _section(
        "Charts",
        [f"- {card.title or 'Chart'}: {card.webpage_url}" for card in cards if card.webpage_url],
    )
    metadata = result.metadata
    if metadata is not None:
        body += _section(
            "Definitions",
            [
                f"- **{d.term}**: {d.definition}"
                + (f" [{d.source_ref}]" if d.source_ref is not None else "")
                for d in metadata.definitions or []
            ],
        )
        body += _section(
            "Assumptions", [f"- **{a.title}**: {a.description}" for a in metadata.assumptions or []]
        )
        body += _section(
            "Methodology", [f"- **{m.title}**: {m.description}" for m in metadata.methodology or []]
        )
    if sources:
        return sources, append_within_budget(body, render_sources_block(sources), max_report_chars)
    return sources, body[:max_report_chars].rstrip()


def _report_progress(block: object, progress: Callable[[str], None]) -> None:
    if isinstance(block, StatusEvent):
        progress(block.message)
    elif isinstance(block, ToolCallEvent):
        suffix = f": {block.status_message}" if block.status_message else ""
        progress(f"{'finished' if block.done else 'calling'} {block.tool}{suffix}")
    elif isinstance(block, ToolRetryEvent):
        progress(f"retrying {block.tool}: {block.error}")
    elif isinstance(block, SubagentEvent):
        progress(f"{block.event} {block.subagent_type}")


def _run_settled_by_stream(
    run_id: str, summary: RunSummaryEvent | None, result: AnswerAgentResult | None
) -> AnswerAgentRun | None:
    if summary is None or summary.status not in TERMINAL_RUN_STATUSES:
        return None
    if result is None and summary.status != AgentRunStatus.FAILED:
        return None
    return AnswerAgentRun(
        run_id=run_id,
        status=summary.status,
        created_at=summary.created_at,
        completed_at=summary.completed_at,
        result=result,
        error=summary.error,
        usage=summary.usage,
    )


def _agent_api_error(exc: ApiException) -> RuntimeError:
    if exc.status in NOT_GRANTED_STATUSES:
        return RuntimeError("deep_research requires a valid, granted TAKO_API_KEY.")
    return RuntimeError(f"Tako answer agent request failed ({exc.status}): {str(exc.body)[:500]}")


class TakoBackend:
    """Tako search (keyed REST or anonymous MCP) and the Tako Answer Agent."""

    def __init__(
        self,
        *,
        api_key: str | None,
        api_base_url: str = API_BASE_URL,
        mcp_url: str = MCP_URL,
    ) -> None:
        self._api_key = api_key
        self._config = Configuration(
            host=api_base_url.rstrip("/") + API_PATH_PREFIX,
            api_key={"apiKey": api_key} if api_key else None,
        )
        self._mcp_url = mcp_url
        self._rest_auth_failed = False

    @property
    def search_mode(self) -> str:
        return "api" if self._api_key and not self._rest_auth_failed else "anonymous"

    def _anonymous_headers(self) -> dict[str, str]:
        return {
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
            "User-Agent": USER_AGENT,
        }

    async def search(self, request: SearchRequestSpec) -> RetrievalResult:
        query = request.query.strip()
        if not query:
            raise RuntimeError("query cannot be empty.")
        partial_failures: list[dict[str, str]] = []
        if self._api_key and not self._rest_auth_failed:
            try:
                return await self._search_api(request, query)
            except NotGranted:
                self._rest_auth_failed = True
        if self._api_key and self._rest_auth_failed:
            partial_failures.append(
                {
                    "query": query,
                    "error": (
                        "TAKO_API_KEY did not authenticate; fell back to anonymous Tako "
                        "search. Configure a granted key to use the REST API."
                    ),
                }
            )
        return await self._search_anonymous(request, query, partial_failures)

    def _search_request(self, request: SearchRequestSpec, query: str) -> SearchRequest:
        count = max(1, min(MAX_SOURCE_COUNT, request.num_results))
        web = WebSourceSettings(count=count, highlights=True)
        if request.include_domains:
            web.include_domains = request.include_domains[:MAX_DOMAIN_FILTERS]
        if request.exclude_domains:
            web.exclude_domains = request.exclude_domains[:MAX_DOMAIN_FILTERS]
        if request.max_age_hours is not None and request.max_age_hours > 0:
            cutoff = dt.datetime.now(dt.UTC) - dt.timedelta(hours=request.max_age_hours)
            web.published_after = cutoff.date()
        return SearchRequest(
            query=query,
            effort=SearchEffortLevel(request.effort) if request.effort else None,
            sources=Sources(data=DataSourceSettings(count=min(count, DATA_CARD_COUNT)), web=web),
        )

    async def _search_api(self, request: SearchRequestSpec, query: str) -> RetrievalResult:
        async with stamped_async_api_client(self._config) as api_client:
            try:
                payload = await TakoApi(api_client).search(
                    self._search_request(request, query),
                    _request_timeout=request.timeout_seconds,
                )
            except ApiException as exc:
                if exc.status in NOT_GRANTED_STATUSES:
                    raise NotGranted from exc
                raise
        partial_failures: list[dict[str, str]] = []
        if request.max_chars_total is not None:
            partial_failures.append(
                {
                    "query": query,
                    "error": "max_chars_total is not supported by Tako search (its cap is per result); ignored.",
                }
            )
        parallel_only = [
            name
            for name, value in (
                ("client_model", request.client_model),
                ("session_id", request.session_id),
            )
            if value is not None
        ]
        if parallel_only:
            partial_failures.append(
                {
                    "query": query,
                    "error": f"{', '.join(parallel_only)} is a Parallel REST knob; Tako search ignores it.",
                }
            )
        billed = payload.usage.total_cost_usd if payload.usage else None
        effort = request.effort or DEFAULT_SEARCH_EFFORT
        return RetrievalResult(
            sources=normalize_search_response(payload),
            backend="tako:api",
            request_ids=[payload.request_id],
            usage=[payload.usage.to_dict()] if payload.usage else [],
            partial_failures=partial_failures,
            estimated_cost_usd=billed if billed is not None else SEARCH_PRICE_USD[effort],
        )

    async def _search_anonymous(
        self, request: SearchRequestSpec, query: str, partial_failures: list[dict[str, str]]
    ) -> RetrievalResult:
        ignored: list[str] = []
        if request.include_domains or request.exclude_domains or request.max_age_hours is not None:
            ignored.append("include_domains/exclude_domains/max_age_hours")
        if request.effort:
            ignored.append(f"effort={request.effort!r}")
        if request.max_chars_total is not None:
            ignored.append("max_chars_total")
        if request.client_model is not None:
            ignored.append("client_model")
        if request.session_id is not None:
            ignored.append("session_id")
        if request.num_results != 10:
            ignored.append(
                f"num_results={request.num_results} (anonymous search serves a fixed count; client-side cap only)"
            )
        if ignored:
            partial_failures.append(
                {
                    "query": query,
                    "error": (
                        f"Anonymous Tako search does not honor: {', '.join(ignored)}. "
                        "Set TAKO_API_KEY to use the REST API."
                    ),
                }
            )
        request_id = str(uuid.uuid4())
        envelope = {
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "tools/call",
            "params": {
                "name": "tako_search",
                "arguments": {"query": query, "sources": ["data", "web"]},
            },
        }
        async with httpx.AsyncClient(timeout=request.timeout_seconds) as client:
            response = await client.post(
                self._mcp_url, headers=self._anonymous_headers(), json=envelope
            )
        if response.status_code == 429:
            reply = JsonRpcResponse.model_validate(decode_jsonrpc_response(response, request_id))
            detail = reply.error.message if reply.error else response.text[:200]
            raise RuntimeError(f"Anonymous Tako search is rate limited: {detail}")
        response.raise_for_status()
        reply = JsonRpcResponse.model_validate(decode_jsonrpc_response(response, request_id))
        if reply.error is not None:
            raise RuntimeError(f"Tako MCP error: {reply.error.message[:500]}")
        result = reply.result
        if result is None:
            raise RuntimeError("Tako MCP returned no result.")
        if result.is_error:
            if result.error_kind() in RATE_LIMIT_KINDS:
                raise RuntimeError(f"Anonymous Tako search is rate limited: {result.text()}")
            raise RuntimeError(f"Tako MCP tool error: {result.text()[:500]}")
        if result.structured_content is None:
            raise RuntimeError("Tako MCP returned no structuredContent.")
        payload = AnonymousSearchOutput.model_validate(result.structured_content)
        return RetrievalResult(
            sources=normalize_anonymous_output(payload),
            backend="tako:anonymous",
            usage=[payload.usage.to_dict()] if payload.usage else [],
            partial_failures=partial_failures,
            estimated_cost_usd=0.0,
        )

    async def deep_research(
        self, request: DeepResearchSpec, progress: Callable[[str], None]
    ) -> DeepResearchResult:
        if not self._api_key:
            raise RuntimeError(
                "deep_research requires TAKO_API_KEY. Anonymous Tako access covers `search` only."
            )
        question = request.question.strip()
        if not question:
            raise RuntimeError("question cannot be empty.")
        effort = request.effort or DEFAULT_RESEARCH_EFFORT
        timeout_seconds = (
            DEFAULT_DEEP_RESEARCH_TIMEOUT_SECONDS
            if request.timeout_seconds is None
            else request.timeout_seconds
        )
        partial_failures: list[dict[str, str]] = []
        if request.processor:
            partial_failures.append(
                {
                    "query": question,
                    "error": (
                        f"--processor={request.processor!r} is Parallel-only; the Tako Answer Agent "
                        f"ran with effort={effort!r}."
                    ),
                }
            )
        progress(f"dispatching answer agent (effort={effort}, timeout={int(timeout_seconds)}s)")
        try:
            run = await asyncio.wait_for(
                self._run_answer_agent(question, effort, progress), timeout=timeout_seconds
            )
        except TimeoutError as exc:
            raise RuntimeError(
                f"Tako answer agent run did not finish within {int(timeout_seconds)}s. "
                "The run keeps going server-side; Tako has no cancel endpoint."
            ) from exc
        if run.result is None:
            detail = f": {run.error.message}" if run.error else ""
            raise RuntimeError(f"Tako answer agent run {run.run_id} {run.status.value}{detail}")
        if run.result.refusal_code:
            raise RuntimeError(
                f"Tako declined the question before running (refusal_code={run.result.refusal_code})."
            )
        sources, answer_markdown = normalize_answer_result(
            run.result, max_report_chars=request.max_report_chars
        )
        if not answer_markdown:
            raise RuntimeError(f"Tako answer agent run {run.run_id} returned no content.")
        billed = run.usage.total_cost_usd if run.usage else None
        return DeepResearchResult(
            sources=sources,
            answer_markdown=answer_markdown,
            backend="tako:agent",
            request_ids=[run.run_id],
            partial_failures=partial_failures,
            usage=[run.usage.to_dict()] if run.usage else [],
            estimated_cost_usd=billed,
        )

    async def _run_answer_agent(
        self, question: str, effort: str, progress: Callable[[str], None]
    ) -> AnswerAgentRun:
        request = AnswerAgentRunRequest(query=question, effort=AnswerAgentEffort(effort))
        try:
            stream = await AsyncAnswerAgentResource(self._config).stream(
                request, read_timeout=STREAM_READ_TIMEOUT_SECONDS
            )
        except ApiException as exc:
            raise _agent_api_error(exc) from exc
        summary: RunSummaryEvent | None = None
        async with stream:
            try:
                envelope: AnswerAgentStreamEnvelope
                async for envelope in stream:
                    block = envelope.block.actual_instance
                    if isinstance(block, RunSummaryEvent):
                        summary = block
                    _report_progress(block, progress)
            except (httpx.TransportError, ApiException) as exc:
                progress(f"stream interrupted: {exc}")
        run_id = stream.run_id
        if run_id is None:
            raise RuntimeError("Tako answer agent stream ended before a run_id arrived.")
        run = _run_settled_by_stream(run_id, summary, stream.result)
        if run is not None:
            return run
        return await self._poll_run(run_id, progress)

    async def _poll_run(self, run_id: str, progress: Callable[[str], None]) -> AnswerAgentRun:
        async with stamped_async_api_client(self._config) as api_client:
            agent_api = AgentApi(api_client)
            while True:
                try:
                    run = await agent_api.get_answer_agent_run(run_id)
                except ApiException as exc:
                    raise _agent_api_error(exc) from exc
                if run.status in TERMINAL_RUN_STATUSES:
                    return run
                progress(f"state={run.status.value}")
                await asyncio.sleep(POLL_INTERVAL_SECONDS)
