"""Websearch client.

`search` retrieves sources through one backend and, when `ANTHROPIC_API_KEY`
is configured, runs the Claude synthesis pipeline (reviewer → writer →
citation repair) over them to produce a cited markdown report. `deep_research`
asks the same backend for a finished report.

The backend is chosen once from `WEBSEARCH_BACKEND` (`tako`, the default, or
`parallel`). Within a backend, a keyed path is tried first with the iron-proxy
placeholder and the anonymous path is used when that returns 401.
"""

from __future__ import annotations

import json
import os
import re
import time
import warnings
from collections.abc import Callable
from typing import Any, Protocol, get_args

from anthropic import AsyncAnthropic, AuthenticationError

from centaur_sdk import get_tool_context, secret

from ._parallel import API_BASE_URL, MCP_URL, ParallelBackend
from ._tako import API_BASE_URL as TAKO_API_BASE_URL
from ._tako import MCP_URL as TAKO_MCP_URL
from ._tako import TakoBackend
from .models import (
    DeepResearchIteration,
    DeepResearchResponse,
    DeepResearchResult,
    DeepResearchSpec,
    ResearchEffort,
    ResponseMeta,
    RetrievalResult,
    SearchEffort,
    SearchRequestSpec,
    SearchResponse,
    SourceDocument,
)
from .prompts import EVIDENCE_REVIEWER_SYSTEM, REPORT_REPAIR_SYSTEM, REPORT_WRITER_SYSTEM

REVIEW_SOURCE_CHAR_LIMIT = 3500
REVIEW_TOTAL_CHAR_BUDGET = 120000
WRITE_SOURCE_CHAR_LIMIT = 7000
WRITE_TOTAL_CHAR_BUDGET = 220000

WEBSEARCH_BACKEND_ENV = "WEBSEARCH_BACKEND"
BACKEND_NAMES = ("tako", "parallel")
DEFAULT_BACKEND = "tako"
SEARCH_EFFORTS = get_args(SearchEffort)
RESEARCH_EFFORTS = get_args(ResearchEffort)
MODE_TO_EFFORT = {"basic": "instant", "advanced": "fast"}


class ResearchBackend(Protocol):
    """One vendor's retrieval and research. Synthesis and assembly stay in the client.

    Each call reports which path it took as `RetrievalResult.backend`
    (`tako:api`, `tako:anonymous`, `parallel:api`, `parallel:mcp`); a backend
    keeps its own keyed-vs-anonymous state private.
    """

    async def search(self, request: SearchRequestSpec) -> RetrievalResult: ...

    async def deep_research(
        self, request: DeepResearchSpec, progress: Callable[[str], None]
    ) -> DeepResearchResult: ...


def _resolve_backend_name(override: str | None) -> str:
    raw = override or os.getenv(WEBSEARCH_BACKEND_ENV, "")  # noqa: TID251  # deployment config, not a secret
    name = raw.strip().lower() or DEFAULT_BACKEND
    if name not in BACKEND_NAMES:
        raise RuntimeError(
            f"{WEBSEARCH_BACKEND_ENV}={name!r} is not supported. "
            f"Set it to one of: {', '.join(BACKEND_NAMES)}."
        )
    return name


def _resolve_search_effort(effort: str | None, mode: str | None) -> str | None:
    if effort is not None and mode is not None:
        raise ValueError("Pass either --effort or --mode, not both. --mode is deprecated.")
    if mode is not None:
        if mode not in MODE_TO_EFFORT:
            raise ValueError(f"--mode must be 'basic' or 'advanced' (got {mode!r}).")
        warnings.warn(
            "mode is deprecated; use effort ('basic' -> 'instant', 'advanced' -> 'fast').",
            DeprecationWarning,
            stacklevel=3,
        )
        return MODE_TO_EFFORT[mode]
    if effort is not None and effort not in SEARCH_EFFORTS:
        raise ValueError(f"effort must be one of {', '.join(SEARCH_EFFORTS)} (got {effort!r}).")
    return effort


def _is_configured(key: str) -> bool:
    """Authoritative check for whether a secret was explicitly configured.

    `secret(key)` is unsafe for routing decisions: under centaur's default
    StubBackend it returns the literal key name as a placeholder for
    un-configured secrets (the stub goes in outbound HTTP headers where
    the firewall swaps it in-flight). Both signals are needed to cover
    server and CLI use:

    - Server / tool-runtime: ToolManager populates ``ctx.secrets[key]``
      only for secrets it actually resolved, so dict membership is the
      authoritative signal.
    - CLI / direct-invoke: no ToolContext is bound; fall through to
      ``secret(key)`` and treat the value-equals-key stub case as
      "not configured" (the firewall has nothing to swap into).
    """
    try:
        ctx = get_tool_context()
        return bool(ctx.secrets.get(key))
    except LookupError:
        try:
            val = secret(key)
        except KeyError:
            return False
        return bool(val) and val != key


def _placeholder(key: str) -> str:
    """Resolve a secret to its value, or to the key name iron-proxy swaps in flight.

    An exported-but-empty variable resolves to `""` through `secret()`, which
    would skip the keyed attempt entirely rather than let the 401 fallback
    decide. Kubernetes produces that shape for an optional secret left unset.
    """
    return secret(key, key) or key


class WebSearchClient:
    """Web search and deep research over one configurable backend."""

    def __init__(
        self,
        *,
        backend: str | None = None,
        parallel_api_key: str | None = None,
        parallel_api_base_url: str | None = None,
        parallel_mcp_url: str | None = None,
        parallel_deep_research_processor: str | None = None,
        tako_api_key: str | None = None,
        tako_api_base_url: str | None = None,
        tako_mcp_url: str | None = None,
        anthropic_api_key: str | None = None,
        synthesis_model: str | None = None,
        max_retries: int = 3,
    ) -> None:
        self._backend_name = _resolve_backend_name(backend)
        # Always pass the StubBackend placeholder so the SDK sends x-api-key.
        # Search falls back to anonymous MCP if injected authentication fails.
        self._parallel_api_key = parallel_api_key or _placeholder("PARALLEL_API_KEY")
        self._tako_api_key = tako_api_key or _placeholder("TAKO_API_KEY")
        self._has_anthropic_key = anthropic_api_key is not None or _is_configured(
            "ANTHROPIC_API_KEY"
        )
        self._anthropic_api_key = anthropic_api_key or (
            secret("ANTHROPIC_API_KEY") if self._has_anthropic_key else None
        )
        # Non-secret config: hardcoded defaults, overridable via constructor
        # args. We deliberately do NOT route these through secret() — under
        # StubBackend that would return the literal key name as a value.
        self._api_base_url = parallel_api_base_url or API_BASE_URL
        self._mcp_url = parallel_mcp_url or MCP_URL
        self._tako_api_base_url = tako_api_base_url
        self._tako_mcp_url = tako_mcp_url
        self._deep_research_processor = parallel_deep_research_processor or "ultra-fast"
        self._synthesis_model = synthesis_model or "claude-opus-4-6"
        self._max_retries = max_retries
        self._progress_callback: Callable[[str], None] | None = None
        self._backend: ResearchBackend = self._build_backend()

    def _build_backend(self) -> ResearchBackend:
        if self._backend_name == "parallel":
            return ParallelBackend(
                api_key=self._parallel_api_key,
                api_base_url=self._api_base_url,
                mcp_url=self._mcp_url,
                deep_research_processor=self._deep_research_processor,
                max_retries=self._max_retries,
            )
        return TakoBackend(
            api_key=self._tako_api_key,
            api_base_url=self._tako_api_base_url or TAKO_API_BASE_URL,
            mcp_url=self._tako_mcp_url or TAKO_MCP_URL,
        )

    def _set_progress_callback(self, callback: Callable[[str], None] | None) -> None:
        self._progress_callback = callback

    def _emit_progress(self, stage: str) -> None:
        if self._progress_callback is not None:
            self._progress_callback(stage)

    @property
    def backend_name(self) -> str:
        return self._backend_name

    @property
    def has_synthesis(self) -> bool:
        return self._has_anthropic_key

    def _build_synthesis_pipeline(self) -> ClaudeSynthesisPipeline | None:
        if not self._has_anthropic_key or not self._synthesis_model:
            return None
        return ClaudeSynthesisPipeline(
            api_key=self._anthropic_api_key or "",
            model=self._synthesis_model,
        )

    async def search(
        self,
        query: str,
        *,
        num_results: int = 10,
        timeout_seconds: float = 60.0,
        synthesize: bool = True,
        effort: str | None = None,
        mode: str | None = None,
        client_model: str | None = None,
        max_chars_total: int | None = None,
        include_domains: list[str] | None = None,
        exclude_domains: list[str] | None = None,
        max_age_hours: int | None = None,
        session_id: str | None = None,
        thread_context: list[str] | None = None,
        max_report_chars: int = 12000,
        search_type: str | None = None,
    ) -> dict:
        """Retrieve sources through the configured backend and optionally synthesize.

        Args:
          query: Required. The question or topic.
          synthesize: Run the Claude reviewer + writer pipeline over the
            results. Requires `ANTHROPIC_API_KEY`; without one the call returns
            raw results and records the skipped synthesis in
            `meta.partial_failures`.
          effort: `instant`, `fast` (default), or `deep`. Tako honors all three;
            Parallel maps `instant` to `basic` and the others to `advanced`.
            Anonymous paths record it in `meta.partial_failures`.
          mode: Deprecated alias for `effort` (`basic` -> `instant`,
            `advanced` -> `fast`). Passing both is an error.
          client_model, max_chars_total, session_id: Parallel REST knobs. Other
            paths ignore them or note them in `meta.partial_failures`.
          include_domains / exclude_domains / max_age_hours: Source filters on
            the keyed paths; anonymous paths note them in `meta.partial_failures`.
            `max_age_hours` rounds down to a UTC calendar date.
          thread_context: Prior-turn context for the synthesis writer.
          search_type: Accepted for backward compatibility; ignored.
        """
        if search_type is not None:
            warnings.warn(
                "search_type is ignored — no current backend exposes Exa's neural/keyword/auto modes.",
                DeprecationWarning,
                stacklevel=2,
            )
        resolved_effort = _resolve_search_effort(effort, mode)
        started = time.perf_counter()
        spec = SearchRequestSpec(
            query=query,
            num_results=num_results,
            timeout_seconds=timeout_seconds,
            include_domains=include_domains,
            exclude_domains=exclude_domains,
            max_age_hours=max_age_hours,
            effort=resolved_effort,
            client_model=client_model,
            max_chars_total=max_chars_total,
            session_id=session_id,
        )
        retrieval = await self._backend.search(spec)
        capped = retrieval.sources[: max(1, min(40, num_results))]
        partial_failures = list(retrieval.partial_failures)
        footer = f"\n\n---\n_{retrieval.attribution}_\n" if retrieval.attribution else ""
        answer_markdown: str | None = None
        if synthesize and capped:
            answer_markdown = await self._synthesize(
                question=query,
                sources=capped,
                thread_context=thread_context,
                max_report_chars=max(1, max_report_chars - len(footer)),
                partial_failures=partial_failures,
            )
        if synthesize and not capped:
            partial_failures.append(
                {"query": query, "error": "synthesis skipped: retrieval returned no sources."}
            )
        if footer and answer_markdown:
            answer_markdown = f"{answer_markdown.rstrip()}{footer}"
        meta = ResponseMeta(
            duration_ms=int((time.perf_counter() - started) * 1000),
            request_ids=retrieval.request_ids,
            partial_failures=partial_failures,
            backend=retrieval.backend,
            usage=retrieval.usage,
            attribution=retrieval.attribution,
            estimated_cost_usd=retrieval.estimated_cost_usd,
        )
        return SearchResponse(
            query=query, results=capped, answer_markdown=answer_markdown, meta=meta
        ).model_dump()

    async def _synthesize(
        self,
        *,
        question: str,
        sources: list[SourceDocument],
        thread_context: list[str] | None,
        max_report_chars: int,
        partial_failures: list[dict[str, str]],
    ) -> str | None:
        pipeline = self._build_synthesis_pipeline()
        if pipeline is None:
            partial_failures.append(
                {
                    "query": question,
                    "error": (
                        "synthesize=true requested but ANTHROPIC_API_KEY is not set; "
                        "returning raw excerpts. Set ANTHROPIC_API_KEY (or pass "
                        "synthesize=false) to silence this notice."
                    ),
                }
            )
            return None
        try:
            outcome = await pipeline.synthesize(
                question=question,
                sources=sources,
                thread_context=thread_context,
                max_report_chars=max_report_chars,
            )
        except Exception as exc:
            partial_failures.append({"query": question, "error": f"synthesis failed: {exc}"})
            return None
        if outcome["validation_error"]:
            partial_failures.append(
                {"query": question, "error": f"synthesis failed: {outcome['validation_error']}"}
            )
        return outcome["report"]

    async def deep_research(
        self,
        question: str,
        *,
        effort: str | None = None,
        processor: str | None = None,
        timeout_seconds: float | None = None,
        max_report_chars: int = 50000,
        max_iterations: int | None = None,
        num_queries_per_iteration: int | None = None,
        num_results_per_query: int | None = None,
        thread_context: list[str] | None = None,
    ) -> dict:
        """Run deep research through the configured backend and return a cited report.

        Args:
          effort: `medium` (default) or `high`. Tako passes it to the Answer
            Agent; Parallel maps `medium` to `ultra-fast` and `high` to `ultra`.
          processor: Deprecated, Parallel-only. Overrides the `effort` mapping
            on Parallel; Tako records it in `meta.partial_failures`.
          timeout_seconds: Overall budget. Defaults to 600 s on Tako and to a
            processor-appropriate value on Parallel.
          max_iterations / num_queries_per_iteration / num_results_per_query /
          thread_context: Accepted for backward compatibility; ignored.

        Requires the backend's API key. Neither backend has an anonymous tier
        for deep research.
        """
        deprecated = [
            ("max_iterations", max_iterations),
            ("num_queries_per_iteration", num_queries_per_iteration),
            ("num_results_per_query", num_results_per_query),
            ("thread_context", thread_context),
        ]
        used = [name for name, value in deprecated if value is not None]
        if used:
            warnings.warn(
                f"deep_research kwargs ignored: {used}. Both backends run a single "
                "multi-source job; iteration knobs no longer apply.",
                DeprecationWarning,
                stacklevel=2,
            )
        if processor is not None:
            warnings.warn(
                "processor is deprecated and Parallel-only; use effort ('medium' or 'high').",
                DeprecationWarning,
                stacklevel=2,
            )
        if effort is not None and effort not in RESEARCH_EFFORTS:
            raise ValueError(
                f"effort must be one of {', '.join(RESEARCH_EFFORTS)} (got {effort!r})."
            )
        started = time.perf_counter()
        normalized = question.strip()
        spec = DeepResearchSpec(
            question=normalized,
            effort=effort,
            processor=processor,
            timeout_seconds=timeout_seconds,
            max_report_chars=max_report_chars,
        )
        result = await self._backend.deep_research(spec, self._emit_progress)
        meta = ResponseMeta(
            duration_ms=int((time.perf_counter() - started) * 1000),
            request_ids=result.request_ids,
            partial_failures=result.partial_failures,
            backend=result.backend,
            usage=result.usage,
            estimated_cost_usd=result.estimated_cost_usd,
        )
        iterations = [
            DeepResearchIteration(
                iteration=1,
                queries=[normalized],
                results_count=len(result.sources),
                continue_reason=result.backend,
            )
        ]
        return DeepResearchResponse(
            question=normalized,
            answer_markdown=result.answer_markdown,
            sources=result.sources,
            iterations=iterations,
            meta=meta,
        ).model_dump()


class ClaudeSynthesisPipeline:
    """Reviewer + writer + LLM-driven citation repair.

    Byte-identical to the synthesis pipeline in centaur main: the
    reviewer extracts claims and contradictions, the writer drafts a
    cited report, and `_validate_and_repair_citations` invokes the
    repair prompt up to two times to fix any citation IDs that aren't
    grounded in the source list before raising. `synthesize()` returns
    a dict so that callers can mirror the original tool's "partial
    failure flagged but writer output retained" behavior when the
    repair loop ultimately throws.
    """

    def __init__(self, *, api_key: str, model: str) -> None:
        self._client = AsyncAnthropic(api_key=api_key)
        self._model = model

    async def synthesize(
        self,
        *,
        question: str,
        sources: list[SourceDocument],
        thread_context: list[str] | None = None,
        max_report_chars: int = 12000,
    ) -> dict[str, Any]:
        """Run reviewer → writer → validate-and-repair-citations.

        Returns a dict with 'report' (the markdown — writer output retained
        even when citation validation throws, matching the original tool's
        behavior) and 'validation_error' (str | None when repair could not
        produce a valid Sources section).
        """
        normalized_context = _normalize_thread_context(thread_context)
        reviewer = await self._review_evidence(
            question=question,
            sources=sources,
            thread_context=normalized_context,
        )
        report = await self._write_report(
            question=question,
            sources=sources,
            claims=reviewer["claims"],
            contradictions=reviewer["contradictions"],
            thread_context=normalized_context,
            max_report_chars=max_report_chars,
        )
        validation_error: str | None = None
        try:
            report = await self._validate_and_repair_citations(
                report=report, sources=sources, max_report_chars=max_report_chars
            )
        except Exception as exc:
            validation_error = str(exc)
        return {"report": report, "validation_error": validation_error}

    async def _review_evidence(
        self,
        *,
        question: str,
        sources: list[SourceDocument],
        thread_context: list[str],
    ) -> dict[str, Any]:
        compact_sources = _trim_sources_for_budget(
            sources,
            per_source_chars=REVIEW_SOURCE_CHAR_LIMIT,
            total_chars=REVIEW_TOTAL_CHAR_BUDGET,
        )
        user_prompt = json.dumps(
            {
                "question": question,
                "iteration": 1,
                "max_iterations": 1,
                "thread_context": thread_context,
                "sources": compact_sources,
            },
            indent=2,
        )
        payload = await self._call_claude_json(
            system_prompt=EVIDENCE_REVIEWER_SYSTEM,
            user_prompt=user_prompt,
            max_tokens=3600,
        )
        if not isinstance(payload, dict):
            raise RuntimeError("Evidence reviewer output must be a JSON object.")
        valid_source_ids = {source.source_id for source in sources}
        claims = _normalize_claims(
            payload.get("claims", []) if isinstance(payload.get("claims"), list) else [],
            valid_source_ids,
        )
        contradictions = _normalize_contradictions(
            payload.get("contradictions", [])
            if isinstance(payload.get("contradictions"), list)
            else [],
            valid_source_ids,
        )
        return {"claims": claims, "contradictions": contradictions}

    async def _write_report(
        self,
        *,
        question: str,
        sources: list[SourceDocument],
        claims: list[dict[str, Any]],
        contradictions: list[dict[str, Any]],
        thread_context: list[str],
        max_report_chars: int,
    ) -> str:
        selected_sources = _trim_sources_for_budget(
            sources,
            per_source_chars=WRITE_SOURCE_CHAR_LIMIT,
            total_chars=WRITE_TOTAL_CHAR_BUDGET,
        )
        source_map = {source["source_id"]: source for source in selected_sources}
        user_prompt = json.dumps(
            {
                "question": question,
                "claims": claims,
                "contradictions": contradictions,
                "thread_context": thread_context,
                "source_map": source_map,
            },
            indent=2,
        )
        report = await self._call_claude_text(
            system_prompt=REPORT_WRITER_SYSTEM,
            user_prompt=user_prompt,
            max_tokens=8000,
        )
        return report[:max_report_chars]

    async def _repair_report_citations(
        self,
        *,
        report: str,
        invalid_ids: list[int],
        missing_sources_ids: list[int],
        sources: list[SourceDocument],
        max_report_chars: int,
    ) -> str:
        source_map = {
            source.source_id: {
                "title": source.title,
                "url": source.url,
                "snippet": source.snippet[:REVIEW_SOURCE_CHAR_LIMIT],
            }
            for source in sources
        }
        user_prompt = json.dumps(
            {
                "invalid_citation_ids": invalid_ids,
                "missing_sources_section_ids": missing_sources_ids,
                "source_map": source_map,
                "report": report,
            },
            indent=2,
        )
        repaired = await self._call_claude_text(
            system_prompt=REPORT_REPAIR_SYSTEM,
            user_prompt=user_prompt,
            max_tokens=7000,
        )
        return repaired[:max_report_chars]

    async def _validate_and_repair_citations(
        self,
        *,
        report: str,
        sources: list[SourceDocument],
        max_report_chars: int,
    ) -> str:
        max_repair_attempts = 2
        invalid_ids = sorted(_invalid_citation_ids(report, sources))
        cited_ids = _extract_citation_ids(report)
        sources_section_ids = _extract_sources_section_ids(report)
        missing_sources_ids = sorted(cited_ids - sources_section_ids)
        attempt = 0
        while (invalid_ids or missing_sources_ids) and attempt < max_repair_attempts:
            attempt += 1
            report = await self._repair_report_citations(
                report=report,
                invalid_ids=invalid_ids,
                missing_sources_ids=missing_sources_ids,
                sources=sources,
                max_report_chars=max_report_chars,
            )
            invalid_ids = sorted(_invalid_citation_ids(report, sources))
            cited_ids = _extract_citation_ids(report)
            sources_section_ids = _extract_sources_section_ids(report)
            missing_sources_ids = sorted(cited_ids - sources_section_ids)
        if invalid_ids:
            raise RuntimeError(
                f"Citation validation failed. Invalid source IDs in report: {invalid_ids}"
            )
        if missing_sources_ids:
            raise RuntimeError(
                "Citation validation failed. Sources section missing cited IDs: "
                f"{missing_sources_ids}"
            )
        if not _extract_citation_ids(report):
            raise RuntimeError(
                "Citation validation failed. Report did not include source citations."
            )
        return report

    async def _call_claude_text(
        self,
        *,
        system_prompt: str,
        user_prompt: str,
        max_tokens: int,
    ) -> str:
        try:
            message = await self._client.messages.create(
                model=self._model,
                max_tokens=max_tokens,
                system=system_prompt,
                messages=[{"role": "user", "content": user_prompt}],
            )
        except AuthenticationError as exc:
            raise RuntimeError(
                "Anthropic authentication failed. Check ANTHROPIC_API_KEY."
            ) from exc
        body = _extract_text_content(message)
        if not body:
            raise RuntimeError("Claude returned empty content.")
        return body

    async def _call_claude_json(
        self,
        *,
        system_prompt: str,
        user_prompt: str,
        max_tokens: int,
    ) -> Any:
        raw = await self._call_claude_text(
            system_prompt=system_prompt,
            user_prompt=user_prompt,
            max_tokens=max_tokens,
        )
        return _coerce_json(raw)


def _normalize_thread_context(
    thread_context: list[str] | None,
    *,
    max_items: int = 20,
    max_chars_per_item: int = 1200,
) -> list[str]:
    if not thread_context:
        return []
    normalized: list[str] = []
    for item in thread_context:
        text = str(item).strip()
        if not text:
            continue
        normalized.append(text[:max_chars_per_item])
        if len(normalized) >= max_items:
            break
    return normalized


def _trim_sources_for_budget(
    sources: list[SourceDocument],
    *,
    per_source_chars: int,
    total_chars: int,
) -> list[dict[str, Any]]:
    selected: list[dict[str, Any]] = []
    consumed = 0
    ranked = sorted(sources, key=_source_quality_score, reverse=True)
    for source in ranked:
        snippet = source.snippet[:per_source_chars] or source.title
        projected = consumed + len(snippet)
        if selected and projected > total_chars:
            break
        selected.append(
            {
                "source_id": source.source_id,
                "title": source.title,
                "url": source.url,
                "snippet": snippet,
                "published_date": source.published_date,
                "domain": source.domain,
            }
        )
        consumed = projected
    return selected


def _source_quality_score(source: SourceDocument) -> int:
    score = 0
    snippet_lower = source.snippet.lower()
    domain = (source.domain or "").lower()
    if source.published_date:
        score += 1
    if len(source.snippet) > 600:
        score += 1
    if domain.endswith(".gov") or domain.endswith(".edu"):
        score += 3
    low_signal_tokens = [
        "book now",
        "free 30-min",
        "cookie policy",
        "skip to content",
    ]
    if any(token in snippet_lower for token in low_signal_tokens):
        score -= 2
    if "linkedin.com" in domain:
        score -= 2
    return score


def _normalize_claims(claims: list[Any], valid_ids: set[int]) -> list[dict[str, Any]]:
    out: list[dict[str, Any]] = []
    seen: set[str] = set()
    for claim in claims:
        if not isinstance(claim, dict):
            continue
        text = str(claim.get("claim", "")).strip()
        if not text:
            continue
        key = text.casefold()
        if key in seen:
            continue
        raw_ids = claim.get("source_ids", [])
        ids: list[int] = []
        if isinstance(raw_ids, list):
            for raw_id in raw_ids:
                if isinstance(raw_id, int) and raw_id in valid_ids:
                    ids.append(raw_id)
        support = str(claim.get("support_level", "none")).strip().lower()
        if support not in {"strong", "partial", "weak", "none"}:
            support = "none"
        out.append({"claim": text, "source_ids": sorted(set(ids)), "support_level": support})
        seen.add(key)
    return out


def _normalize_contradictions(
    contradictions: list[Any], valid_ids: set[int]
) -> list[dict[str, Any]]:
    out: list[dict[str, Any]] = []
    seen: set[str] = set()
    for entry in contradictions:
        if not isinstance(entry, dict):
            continue
        summary = str(entry.get("summary", "")).strip()
        if not summary:
            continue
        key = summary.casefold()
        if key in seen:
            continue
        raw_ids = entry.get("source_ids", [])
        ids: list[int] = []
        if isinstance(raw_ids, list):
            for raw_id in raw_ids:
                if isinstance(raw_id, int) and raw_id in valid_ids:
                    ids.append(raw_id)
        out.append({"summary": summary, "source_ids": sorted(set(ids))})
        seen.add(key)
    return out


def _extract_citation_ids(text: str) -> set[int]:
    return {int(m) for m in re.findall(r"\[\s*(\d+)\s*\]", text)}


def _extract_sources_section_ids(text: str) -> set[int]:
    match = re.search(r"##\s*Sources\s*(.*)$", text, flags=re.IGNORECASE | re.DOTALL)
    if not match:
        return set()
    return {
        int(source_id)
        for source_id in re.findall(
            r"^\s*(?:[-*]\s+)?\[\s*(\d+)\s*\]\s+", match.group(1), flags=re.MULTILINE
        )
    }


def _invalid_citation_ids(text: str, sources: list[SourceDocument]) -> set[int]:
    valid = {source.source_id for source in sources}
    return {cid for cid in _extract_citation_ids(text) if cid not in valid}


def _extract_text_content(message: Any) -> str:
    blocks = getattr(message, "content", None)
    if not isinstance(blocks, list):
        return ""
    parts: list[str] = []
    for block in blocks:
        text = getattr(block, "text", None)
        if isinstance(text, str):
            parts.append(text)
    return "".join(parts).strip()


def _coerce_json(raw_text: str) -> Any:
    stripped = raw_text.strip()
    if stripped.startswith("```"):
        stripped = re.sub(r"^```(?:json)?\s*", "", stripped, flags=re.IGNORECASE)
        stripped = re.sub(r"\s*```$", "", stripped)
    try:
        return json.loads(stripped)
    except json.JSONDecodeError:
        pass
    object_match = re.search(r"\{.*\}", stripped, flags=re.DOTALL)
    if object_match:
        return json.loads(object_match.group(0))
    array_match = re.search(r"\[.*\]", stripped, flags=re.DOTALL)
    if array_match:
        return json.loads(array_match.group(0))
    raise ValueError("Model response did not contain valid JSON.")


def _client() -> WebSearchClient:
    """Factory for the centaur tool loader."""
    return WebSearchClient()
