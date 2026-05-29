"""Parallel Web Systems backend.

`search` and `deep_research` against:

- The official `parallel-web` Python SDK when `PARALLEL_API_KEY` is configured
  (https://docs.parallel.ai). Used for the Search API and the Task API.
- The free hosted Search MCP when no key is configured (for `search` only;
  `deep_research` requires a key).
    - https://search.parallel.ai/mcp  (Streamable HTTP, anonymous-friendly)
      Docs: https://docs.parallel.ai/integrations/mcp/search-mcp
"""

from __future__ import annotations

import datetime as dt
import json
import time
import uuid
from typing import Any
from urllib.parse import urlparse

import httpx
from parallel import APIStatusError, APITimeoutError, AsyncParallel, AuthenticationError

from .models import (
    DeepResearchIteration,
    DeepResearchResponse,
    ResponseMeta,
    SearchResponse,
    SourceDocument,
)

API_BASE_URL = "https://api.parallel.ai"
MCP_URL = "https://search.parallel.ai/mcp"
MCP_PROTOCOL_VERSION = "2025-06-18"
MCP_CLIENT_NAME = "centaur-websearch"
MCP_CLIENT_VERSION = "0.2.0"
SNIPPET_CHAR_LIMIT = 7000

# Per Parallel docs, Deep Research is "optimized within the `pro` and `ultra`
# processor families" — other processors (lite/base/core/core2x) return a flat
# single-field output rather than the rich auto-schema, so we restrict the
# `deep_research` method to the pro/ultra family. Values are ~2x the documented
# max-latency bands (https://docs.parallel.ai/task-api/examples/task-deep-research)
# because real runs routinely run past the optimistic upper bound — a too-tight
# default produces spurious timeouts on legitimate jobs. Callers can override
# via `timeout_seconds`.
PROCESSOR_TIMEOUT_SECONDS: dict[str, float] = {
    "pro": 1200.0,
    "pro-fast": 720.0,
    "ultra": 3000.0,
    "ultra-fast": 1500.0,
    "ultra2x": 4200.0,
    "ultra2x-fast": 2400.0,
    "ultra4x": 7200.0,
    "ultra4x-fast": 4200.0,
    "ultra8x": 10800.0,
    "ultra8x-fast": 6000.0,
}

# Published list prices per 1,000 units, used only for a best-effort
# `meta.estimated_cost_usd` (the API does not return billed cost). These will
# drift if Parallel changes pricing; source of truth is
# https://docs.parallel.ai/getting-started/pricing.
# Task pricing is per run and identical for a processor's `-fast` variant.
TASK_PRICE_USD_PER_1000: dict[str, float] = {
    "pro": 100.0,
    "ultra": 300.0,
    "ultra2x": 600.0,
    "ultra4x": 1200.0,
    "ultra8x": 2400.0,
}
# Search REST: $5 per 1,000 requests at the default 10 results, plus $0.001 per
# additional result/excerpt beyond that. The free MCP path is $0.
SEARCH_PRICE_USD_PER_1000 = 5.0
SEARCH_INCLUDED_RESULTS = 10
SEARCH_EXTRA_RESULT_USD = 0.001


def _estimate_task_cost_usd(processor: str) -> float | None:
    base = processor.removesuffix("-fast")
    price = TASK_PRICE_USD_PER_1000.get(base)
    return round(price / 1000, 6) if price is not None else None


def _estimate_search_cost_usd(num_results: int) -> float:
    extra = max(0, num_results - SEARCH_INCLUDED_RESULTS)
    return round(SEARCH_PRICE_USD_PER_1000 / 1000 + extra * SEARCH_EXTRA_RESULT_USD, 6)
DEFAULT_DEEP_RESEARCH_TIMEOUT_SECONDS = 1800.0
_FREE_MCP_ATTRIBUTION = (
    "Search powered by the free Parallel Web Search MCP "
    "(https://parallel.ai). See https://parallel.ai/customer-terms."
)


def _append_within_budget(body: str, trailer: str, max_chars: int) -> str:
    """Append `trailer` to `body`, keeping the total within `max_chars`.

    The trailer (the canonical ## Sources block or attribution footer) is
    always preserved intact — it carries citation integrity, so it is never
    sliced. The body absorbs truncation to make room. In the degenerate case
    where the trailer alone exceeds `max_chars`, the cap is exceeded rather
    than corrupting the citation map (integrity beats the best-effort cap).
    """
    body = body.rstrip()
    if len(body) + len(trailer) <= max_chars:
        return body + trailer
    body_budget = max(0, max_chars - len(trailer))
    return body[:body_budget].rstrip() + trailer


class ParallelBackend:
    """Parallel-powered search and deep research."""

    def __init__(
        self,
        *,
        api_key: str | None,
        api_base_url: str = API_BASE_URL,
        mcp_url: str = MCP_URL,
        deep_research_processor: str = "ultra-fast",
        max_retries: int = 3,
    ) -> None:
        self._api_key = api_key
        self._api_base_url = api_base_url.rstrip("/")
        self._mcp_url = mcp_url
        self._default_processor = deep_research_processor
        self._max_retries = max_retries
        # Per Parallel docs, callers should set a stable session_id and reuse
        # it across related Search/Extract calls — required for free-tier
        # rate-limit attribution and useful for call correlation. We mint one
        # per backend instance so callers who omit it still get continuity
        # within a process.
        self._default_session_id = f"centaur-websearch-{uuid.uuid4().hex}"
        # Set once a REST search fails auth — i.e. the configured "key" was an
        # un-swapped placeholder (centaur replace-mode secret that iron-proxy
        # had nothing to resolve). Subsequent searches skip REST and use the
        # anonymous MCP path (see the fallback in `search`).
        self._rest_auth_failed = False

    @property
    def has_api_key(self) -> bool:
        return bool(self._api_key)

    @property
    def search_mode(self) -> str:
        return "api" if self._api_key else "mcp"

    def _sdk_client(self, *, timeout_seconds: float) -> AsyncParallel:
        if not self._api_key:
            raise RuntimeError("PARALLEL_API_KEY is required for the SDK path.")
        return AsyncParallel(
            api_key=self._api_key,
            base_url=self._api_base_url,
            max_retries=self._max_retries,
            timeout=timeout_seconds,
        )

    async def search(
        self,
        *,
        search_queries: list[str],
        objective: str | None = None,
        num_results: int = 10,
        timeout_seconds: float = 60.0,
        synthesize: bool = True,
        max_report_chars: int = 12000,
        mode: str | None = None,
        client_model: str | None = None,
        max_chars_total: int | None = None,
        include_domains: list[str] | None = None,
        exclude_domains: list[str] | None = None,
        max_age_hours: int | None = None,
        session_id: str | None = None,
        synthesis_pipeline: Any | None = None,
        thread_context: list[str] | None = None,
    ) -> dict:
        started = time.perf_counter()
        queries = [q.strip() for q in (search_queries or []) if q and q.strip()]
        if not queries:
            raise RuntimeError("search_queries must contain at least one non-empty query.")
        cleaned_objective = (objective or "").strip() or None
        # Reuse a stable session id when the caller doesn't supply one (free
        # tier MCP needs it for rate-limit attribution; REST benefits from
        # call correlation in Parallel's logs).
        effective_session_id = session_id or self._default_session_id
        partial_failures: list[dict[str, str]] = []

        display_query = cleaned_objective or "; ".join(queries)
        use_rest = bool(self._api_key) and not self._rest_auth_failed
        if use_rest:
            try:
                sources, request_id, usage = await self._search_api(
                    objective=cleaned_objective,
                    search_queries=queries,
                    timeout_seconds=timeout_seconds,
                    mode=mode,
                    client_model=client_model,
                    max_chars_total=max_chars_total,
                    num_results=num_results,
                    include_domains=include_domains,
                    exclude_domains=exclude_domains,
                    max_age_hours=max_age_hours,
                    session_id=effective_session_id,
                )
                backend_label = "parallel:api"
            except AuthenticationError:
                # The configured key was an un-swapped placeholder (centaur
                # replace-mode secret with no real value in the vault). Fall
                # back to the anonymous MCP path and skip REST from now on.
                self._rest_auth_failed = True
                use_rest = False
                partial_failures.append(
                    {
                        "query": display_query,
                        "error": (
                            "PARALLEL_API_KEY did not authenticate; fell back to the "
                            "free Search MCP. Configure a valid key to use the REST API."
                        ),
                    }
                )

        if not use_rest:
            ignored = []
            if include_domains or exclude_domains or max_age_hours is not None:
                ignored.append("include_domains/exclude_domains/max_age_hours")
            if mode and mode != "basic":
                ignored.append(f"mode={mode!r} (MCP forces basic)")
            if max_chars_total is not None:
                ignored.append("max_chars_total")
            if num_results != 10:
                ignored.append(
                    f"num_results={num_results} (MCP serves a fixed default; client-side cap only)"
                )
            if ignored:
                partial_failures.append(
                    {
                        "query": display_query,
                        "error": (
                            f"Free Search MCP does not honor: {', '.join(ignored)}. "
                            "Set PARALLEL_API_KEY to use the Search REST API."
                        ),
                    }
                )
            sources, request_id, usage = await self._search_mcp(
                objective=cleaned_objective,
                search_queries=queries,
                client_model=client_model,
                timeout_seconds=timeout_seconds,
                session_id=effective_session_id,
            )
            backend_label = "parallel:mcp"

        capped_sources = sources[: max(1, min(40, num_results))]
        # On the free-MCP path we append an attribution footer; reserve its
        # length up front so the synthesized report (including its trailing
        # ## Sources section) is generated within the remaining budget and the
        # footer never has to displace the citation map.
        attribution = _FREE_MCP_ATTRIBUTION if backend_label == "parallel:mcp" else None
        footer = f"\n\n---\n_{attribution}_\n" if attribution else ""
        synthesis_budget = max(1, max_report_chars - len(footer))
        answer_markdown: str | None = None
        if synthesize and capped_sources:
            if synthesis_pipeline is not None:
                try:
                    syn_result = await synthesis_pipeline.synthesize(
                        question=display_query,
                        sources=capped_sources,
                        thread_context=thread_context,
                        max_report_chars=synthesis_budget,
                    )
                    answer_markdown = syn_result["report"]
                    if syn_result["validation_error"]:
                        partial_failures.append(
                            {
                                "query": display_query,
                                "error": f"synthesis failed: {syn_result['validation_error']}",
                            }
                        )
                except Exception as exc:
                    partial_failures.append(
                        {"query": display_query, "error": f"synthesis failed: {exc}"}
                    )
            else:
                partial_failures.append(
                    {
                        "query": display_query,
                        "error": (
                            "synthesize=true requested but ANTHROPIC_API_KEY is not set; "
                            "returning raw excerpts. Set ANTHROPIC_API_KEY (or pass "
                            "synthesize=false) to silence this notice."
                        ),
                    }
                )

        if footer and answer_markdown:
            answer_markdown = f"{answer_markdown.rstrip()}{footer}"
        estimated_cost_usd = (
            _estimate_search_cost_usd(num_results) if backend_label == "parallel:api" else 0.0
        )
        meta = ResponseMeta(
            duration_ms=int((time.perf_counter() - started) * 1000),
            request_ids=[request_id] if request_id else [],
            partial_failures=partial_failures,
            backend=backend_label,
            usage=usage,
            attribution=attribution,
            estimated_cost_usd=estimated_cost_usd,
        )
        return SearchResponse(
            query=display_query,
            results=capped_sources,
            answer_markdown=answer_markdown,
            meta=meta,
        ).model_dump()

    async def deep_research(
        self,
        *,
        question: str,
        progress: Any,
        processor: str | None = None,
        timeout_seconds: float | None = None,
        max_report_chars: int,
    ) -> dict:
        if not self._api_key:
            raise RuntimeError(
                "deep_research requires PARALLEL_API_KEY. The free Search MCP is "
                "only available for `search`; set PARALLEL_API_KEY to enable deep "
                "research via the Parallel Task API."
            )
        normalized = question.strip()
        if not normalized:
            raise RuntimeError("question cannot be empty.")

        effective_processor = (processor or self._default_processor).strip()
        if effective_processor not in PROCESSOR_TIMEOUT_SECONDS:
            raise RuntimeError(
                f"deep_research requires a pro/ultra processor (got {effective_processor!r}). "
                f"Per Parallel docs, Deep Research is optimized within the pro and ultra "
                f"families. Supported: {sorted(PROCESSOR_TIMEOUT_SECONDS)}."
            )
        effective_timeout = timeout_seconds or PROCESSOR_TIMEOUT_SECONDS[effective_processor]

        started = time.perf_counter()
        progress(f"creating task ({effective_processor}, timeout={int(effective_timeout)}s)")
        client = self._sdk_client(timeout_seconds=effective_timeout)
        # Use auto schema (Parallel's default for pro/ultra processors), which
        # returns a structured JSON report with per-field basis grounding. The
        # canonical Deep Research example in the docs uses this — text mode
        # gives looser, less reliably-cited output.
        async with client:
            task = await client.task_run.create(
                input=normalized,
                processor=effective_processor,
                enable_events=True,
            )
            run_id = task.run_id
            progress(f"queued {run_id}")

            await _stream_progress(client, run_id, progress)

            result = await _await_task_result(
                client, run_id=run_id, deadline=started + effective_timeout
            )

        sources, answer_markdown = _normalize_task_result(result, max_report_chars=max_report_chars)
        if not answer_markdown:
            raise RuntimeError(f"Parallel task run {run_id} returned no content.")

        meta = ResponseMeta(
            duration_ms=int((time.perf_counter() - started) * 1000),
            request_ids=[run_id],
            backend=f"parallel:task:{effective_processor}",
            estimated_cost_usd=_estimate_task_cost_usd(effective_processor),
        )
        iterations = [
            DeepResearchIteration(
                iteration=1,
                queries=[normalized],
                results_count=len(sources),
                continue_reason=f"parallel:{effective_processor}",
            )
        ]
        return DeepResearchResponse(
            question=normalized,
            answer_markdown=answer_markdown,
            sources=sources,
            iterations=iterations,
            meta=meta,
        ).model_dump()

    async def _search_api(
        self,
        *,
        objective: str | None,
        search_queries: list[str],
        timeout_seconds: float,
        mode: str | None,
        client_model: str | None,
        max_chars_total: int | None,
        num_results: int,
        include_domains: list[str] | None,
        exclude_domains: list[str] | None,
        max_age_hours: int | None,
        session_id: str | None,
    ) -> tuple[list[SourceDocument], str, list[dict[str, Any]]]:
        client = self._sdk_client(timeout_seconds=timeout_seconds)
        kwargs: dict[str, Any] = {"search_queries": search_queries}
        if objective:
            kwargs["objective"] = objective
        if mode:
            kwargs["mode"] = mode
        if client_model:
            kwargs["client_model"] = client_model
        # Default to a generous excerpt budget — Parallel's dynamic default
        # is conservative and the resulting short excerpts measurably hurt
        # downstream synthesis quality.
        kwargs["max_chars_total"] = max_chars_total if max_chars_total is not None else 30000
        if session_id:
            kwargs["session_id"] = session_id

        source_policy = _build_source_policy(include_domains, exclude_domains, max_age_hours)
        advanced_settings: dict[str, Any] = {
            "excerpt_settings": {"max_chars_per_result": 3000},
        }
        if source_policy:
            advanced_settings["source_policy"] = source_policy
        # Pass num_results through as max_results so callers asking for fewer
        # actually get reduced latency/cost. Parallel's best-practices doc
        # warns against using max_results to over-constrain quality, but
        # silently ignoring an explicit caller knob is worse.
        advanced_settings["max_results"] = max(1, min(40, num_results))
        kwargs["advanced_settings"] = advanced_settings

        async with client:
            result = await client.search(**kwargs)
        usage = _serialize_usage(getattr(result, "usage", None))
        return _normalize_search_results(result.results), result.search_id or "", usage

    async def _search_mcp(
        self,
        *,
        objective: str | None,
        search_queries: list[str],
        client_model: str | None,
        timeout_seconds: float,
        session_id: str | None,
    ) -> tuple[list[SourceDocument], str, list[dict[str, Any]]]:
        # MCP's web_search tool requires `objective` (REST treats it as
        # optional). Fall back to a synthesized one from the search queries
        # when the caller didn't supply it.
        arguments: dict[str, Any] = {
            "search_queries": search_queries,
            "objective": objective or "; ".join(search_queries),
        }
        if session_id:
            arguments["session_id"] = session_id
        if client_model:
            arguments["model_name"] = client_model
        payload = await self._mcp_call_tool(
            tool_name="web_search",
            arguments=arguments,
            timeout_seconds=timeout_seconds,
        )
        request_id = str(payload.get("search_id") or "")
        usage = _serialize_usage(payload.get("usage"))
        return _normalize_search_results(payload.get("results")), request_id, usage

    async def _mcp_call_tool(
        self,
        *,
        tool_name: str,
        arguments: dict[str, Any],
        timeout_seconds: float,
    ) -> dict[str, Any]:
        async with httpx.AsyncClient(timeout=timeout_seconds) as client:
            mcp_session_id = await self._mcp_initialize(client)
            envelope = {
                "jsonrpc": "2.0",
                "id": str(uuid.uuid4()),
                "method": "tools/call",
                "params": {"name": tool_name, "arguments": arguments},
            }
            response = await client.post(
                self._mcp_url,
                headers=self._mcp_headers(mcp_session_id),
                json=envelope,
            )
            response.raise_for_status()
            envelope_out = _decode_mcp_envelope(response)
        if "error" in envelope_out:
            raise RuntimeError(f"Parallel MCP error: {str(envelope_out['error'])[:500]}")
        result = envelope_out.get("result") or {}
        if result.get("isError"):
            raise RuntimeError(f"Parallel MCP tool error: {str(result)[:500]}")
        # Prefer structuredContent (machine-readable) over the human text
        # block — newer MCP servers return both and structuredContent is the
        # authoritative payload for downstream parsing.
        structured = result.get("structuredContent")
        if isinstance(structured, dict):
            return structured
        # Scan all text blocks for the first one whose payload is parseable
        # JSON. Only fall back to wrapping a raw string if no block was JSON.
        first_text: str | None = None
        for block in result.get("content", []) or []:
            if not (isinstance(block, dict) and block.get("type") == "text"):
                continue
            text = str(block.get("text") or "")
            if not text:
                continue
            try:
                return json.loads(text)
            except json.JSONDecodeError:
                if first_text is None:
                    first_text = text
        if first_text is not None:
            return {"text": first_text}
        raise RuntimeError(f"Parallel MCP returned no parseable content: {str(result)[:500]}")

    async def _mcp_initialize(self, client: httpx.AsyncClient) -> str:
        init_envelope = {
            "jsonrpc": "2.0",
            "id": str(uuid.uuid4()),
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": MCP_CLIENT_NAME, "version": MCP_CLIENT_VERSION},
            },
        }
        init_response = await client.post(
            self._mcp_url,
            headers=self._mcp_headers(None),
            json=init_envelope,
        )
        init_response.raise_for_status()
        mcp_session_id = init_response.headers.get("mcp-session-id")
        _ = _decode_mcp_envelope(init_response)
        notify = {"jsonrpc": "2.0", "method": "notifications/initialized"}
        ack = await client.post(
            self._mcp_url,
            headers=self._mcp_headers(mcp_session_id),
            json=notify,
        )
        if ack.status_code >= 400:
            raise RuntimeError(
                f"Parallel MCP initialize ack failed ({ack.status_code}): {ack.text[:500]}"
            )
        return mcp_session_id or ""

    def _mcp_headers(self, session_id: str | None) -> dict[str, str]:
        headers = {
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
        }
        if session_id:
            headers["Mcp-Session-Id"] = session_id
        # Only attach a Bearer token when we hold a key that actually
        # authenticates. If REST already failed auth (placeholder/un-swapped
        # replacer), the MCP fallback must stay anonymous — sending the bogus
        # token would get the free endpoint to 401 as well.
        if self._api_key and not self._rest_auth_failed:
            headers["Authorization"] = f"Bearer {self._api_key}"
        return headers


async def _await_task_result(client: AsyncParallel, *, run_id: str, deadline: float) -> Any:
    while True:
        remaining = deadline - time.perf_counter()
        if remaining <= 0:
            raise RuntimeError(f"Parallel task run {run_id} timed out before completion.")
        block_seconds = int(min(600, max(5, remaining)))
        try:
            return await client.task_run.result(run_id, api_timeout=block_seconds)
        except APIStatusError as exc:
            if exc.status_code != 408:
                raise
        except APITimeoutError:
            pass


async def _stream_progress(client: AsyncParallel, run_id: str, progress: Any) -> None:
    """Pump progress events from Parallel into the centaur progress callback."""
    try:
        stream = await client.task_run.events(run_id)
    except Exception as exc:
        progress(f"events stream unavailable: {exc}")
        return
    try:
        async with stream as events:
            async for event in events:
                event_type = getattr(event, "type", "")
                if event_type.startswith("task_run.progress_msg"):
                    # Suffixed variants: .plan / .search / .result / .tool_call
                    # / .exec_status. Surface the phase plus the message text.
                    phase = event_type.rsplit(".", 1)[-1]
                    message = getattr(event, "message", None)
                    if message:
                        progress(f"{phase}: {message}")
                elif event_type == "task_run.progress_stats":
                    stats = getattr(event, "source_stats", None)
                    meter = getattr(event, "progress_meter", None)
                    parts = []
                    if meter is not None:
                        parts.append(f"progress={meter}")
                    if stats is not None:
                        for attr in (
                            "num_sources_considered",
                            "num_sources_read",
                            "num_sources_used",
                        ):
                            value = getattr(stats, attr, None)
                            if value is not None:
                                parts.append(f"{attr}={value}")
                    if parts:
                        progress("stats: " + ", ".join(parts))
                elif event_type == "task_run.state":
                    run = getattr(event, "run", None)
                    status = getattr(run, "status", None) if run is not None else None
                    if status:
                        progress(f"state={status}")
                    if status == "completed":
                        return
                    if status in {"failed", "cancelled"}:
                        err = getattr(run, "error", None)
                        raise RuntimeError(
                            f"Parallel task run {run_id} {status}"
                            + (f": {err}" if err is not None else "")
                        )
                elif event_type == "error":
                    error = getattr(event, "error", None)
                    raise RuntimeError(
                        f"Parallel task run {run_id} errored"
                        + (f": {error}" if error is not None else "")
                    )
    except RuntimeError:
        raise
    except Exception as exc:
        progress(f"events stream interrupted: {exc}")


def _build_source_policy(
    include_domains: list[str] | None,
    exclude_domains: list[str] | None,
    max_age_hours: int | None,
) -> dict[str, Any] | None:
    policy: dict[str, Any] = {}
    if include_domains:
        policy["include_domains"] = list(include_domains)
    if exclude_domains:
        policy["exclude_domains"] = list(exclude_domains)
    if max_age_hours is not None and max_age_hours > 0:
        cutoff = dt.datetime.now(dt.UTC) - dt.timedelta(hours=max_age_hours)
        policy["after_date"] = cutoff.date().isoformat()
    return policy or None


def _serialize_usage(usage: Any) -> list[dict[str, Any]]:
    if usage is None:
        return []
    out: list[dict[str, Any]] = []
    items = usage if isinstance(usage, list) else [usage]
    for item in items:
        if hasattr(item, "model_dump"):
            out.append(item.model_dump())
        elif isinstance(item, dict):
            out.append(item)
        else:
            out.append({"raw": str(item)})
    return out


def _decode_mcp_envelope(response: httpx.Response) -> dict[str, Any]:
    content_type = response.headers.get("content-type", "")
    text = response.text
    if "text/event-stream" in content_type:
        # SSE: events are delimited by blank lines. Within an event, each
        # `data:` line contributes one line of the payload. The last event
        # whose data is parseable JSON wins (intermediate events may be
        # comments, retry directives, or progress notifications).
        latest: dict[str, Any] | None = None
        for event_block in text.split("\n\n"):
            data_lines = [
                line[len("data:") :].lstrip(" ")
                for line in event_block.splitlines()
                if line.startswith("data:")
            ]
            if not data_lines:
                continue
            payload_text = "\n".join(data_lines).strip()
            if not payload_text:
                continue
            try:
                parsed = json.loads(payload_text)
            except json.JSONDecodeError:
                continue
            if isinstance(parsed, dict):
                latest = parsed
        if latest is None:
            raise RuntimeError("Parallel MCP returned an empty SSE stream.")
        return latest
    if not text.strip():
        return {}
    return json.loads(text)


def _normalize_search_results(raw_results: Any) -> list[SourceDocument]:
    sources: list[SourceDocument] = []
    seen: set[str] = set()
    if not raw_results:
        return sources
    for item in raw_results:
        url = _attr_or_key(item, "url")
        if not url or url in seen:
            continue
        seen.add(url)
        title = _attr_or_key(item, "title") or url
        published_date = _attr_or_key(item, "publish_date") or _attr_or_key(item, "published_date")
        if published_date is not None:
            published_date = str(published_date)
        excerpts = _attr_or_key(item, "excerpts")
        if isinstance(excerpts, list) and excerpts:
            snippet = "\n\n".join(str(e) for e in excerpts if e)
        else:
            snippet = str(_attr_or_key(item, "snippet") or _attr_or_key(item, "text") or "")
        sources.append(
            SourceDocument(
                source_id=len(sources),
                title=title,
                url=url,
                snippet=snippet[:SNIPPET_CHAR_LIMIT],
                published_date=published_date,
                domain=urlparse(url).netloc or None,
            )
        )
    return sources


def _normalize_task_result(
    result: Any, *, max_report_chars: int
) -> tuple[list[SourceDocument], str]:
    """Render a Parallel Task API result into our (sources, markdown) shape.

    Handles both auto-schema output (a structured dict with per-field basis
    citations — the canonical Deep Research format) and text-mode output
    (a single markdown string). For auto-schema, we render the structured
    content to markdown with one section per top-level field, attaching the
    citation refs for each field, then append a canonical Sources section.

    The report body is truncated to `max_report_chars` *before* the Sources
    section is appended, so the citation map always survives truncation.
    """
    output = getattr(result, "output", None)
    if output is None:
        return [], ""
    content = getattr(output, "content", None)
    basis_entries = getattr(output, "basis", None) or []

    # The Task API already returns a fully-cited report: the answer text carries
    # the model's own inline [N] markers, which are 1-based into the basis
    # citation list (verified against live output: prose [1] == basis[0]). We
    # collect those basis citations in order and render a Sources block numbered
    # to match, rather than imposing a second numbering scheme of our own.
    sources: list[SourceDocument] = []
    url_to_id: dict[str, int] = {}
    for entry in basis_entries:
        for citation in _attr_or_key(entry, "citations") or []:
            url = _attr_or_key(citation, "url")
            if not url:
                continue
            url = str(url)
            if url in url_to_id:
                continue
            title = _attr_or_key(citation, "title") or url
            excerpts = _attr_or_key(citation, "excerpts")
            if isinstance(excerpts, list) and excerpts:
                snippet = "\n\n".join(str(e) for e in excerpts if e)
            else:
                snippet = ""
            source_id = len(sources) + 1
            sources.append(
                SourceDocument(
                    source_id=source_id,
                    title=str(title),
                    url=url,
                    snippet=snippet[:SNIPPET_CHAR_LIMIT],
                    published_date=None,
                    domain=urlparse(url).netloc or None,
                )
            )
            url_to_id[url] = source_id

    if isinstance(content, dict):
        answer_markdown = _render_auto_content(content)
    elif isinstance(content, str):
        answer_markdown = content
    else:
        return sources, ""

    if sources:
        source_lines = [f"[{source.source_id}] {source.title} — {source.url}" for source in sources]
        sources_block = "\n\n## Sources\n" + "\n".join(source_lines)
        answer_markdown = _append_within_budget(answer_markdown, sources_block, max_report_chars)
    else:
        answer_markdown = answer_markdown[:max_report_chars].rstrip()
    return sources, answer_markdown


def _render_auto_content(content: dict[str, Any]) -> str:
    """Render an auto-schema dict to markdown, one section per top-level key.

    String values (the deep-research answer field) are emitted verbatim so the
    model's own inline [N] citations survive intact; the canonical Sources block
    appended by the caller carries the matching, 1-based source list.
    """
    lines: list[str] = []
    for key, value in content.items():
        lines.append(f"## {_humanize_key(key)}")
        lines.append("")
        lines.extend(_render_value(value, depth=3))
        lines.append("")
    return "\n".join(lines).rstrip()


def _render_value(value: Any, *, depth: int) -> list[str]:
    out: list[str] = []
    if isinstance(value, str):
        out.append(value.rstrip())
    elif isinstance(value, dict):
        for sub_key, sub_value in value.items():
            out.append(f"{'#' * min(depth, 6)} {_humanize_key(sub_key)}")
            out.append("")
            out.extend(_render_value(sub_value, depth=depth + 1))
            out.append("")
    elif isinstance(value, list):
        for index, item in enumerate(value):
            if isinstance(item, dict):
                title_key = next(
                    (k for k in item if k in {"name", "provider_name", "title"}),
                    None,
                )
                title_candidate = str(item[title_key]) if title_key else f"Item {index + 1}"
                out.append(f"{'#' * min(depth, 6)} {title_candidate}")
                out.append("")
                for sub_key, sub_value in item.items():
                    if sub_key == title_key:
                        continue
                    out.append(f"- **{_humanize_key(sub_key)}:** {sub_value}")
                out.append("")
            else:
                out.append(f"- {item}")
        if not value:
            out.append("_(no items)_")
    elif value is None:
        out.append("_(none)_")
    else:
        out.append(str(value))
    return out


def _humanize_key(key: str) -> str:
    return key.replace("_", " ").title()


def _attr_or_key(obj: Any, name: str) -> Any:
    if obj is None:
        return None
    if isinstance(obj, dict):
        return obj.get(name)
    return getattr(obj, name, None)
