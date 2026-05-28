"""Websearch client.

Powered by Parallel Web Systems (https://docs.parallel.ai). Search runs
through the free hosted Parallel Search MCP when no `PARALLEL_API_KEY` is
configured, and through the Parallel Search REST API when one is. Deep
research always goes through the Parallel Task API and requires a key.

`search(synthesize=True)` runs the original tool's Claude pipeline
(reviewer → writer → citation-repair) against the Parallel results
when `ANTHROPIC_API_KEY` is set. The synthesis prompts and repair loop
are byte-identical to centaur main, so output quality and failure
modes mirror that pipeline. Without an Anthropic key, the call still
returns raw Parallel results and records the skipped synthesis in
`meta.partial_failures`.
"""

from __future__ import annotations

import json
import re
import warnings
from collections.abc import Callable
from typing import Any

from anthropic import AsyncAnthropic, AuthenticationError

from centaur_sdk import get_tool_context, secret

from ._parallel import API_BASE_URL, MCP_URL, ParallelBackend
from .models import SourceDocument
from .prompts import EVIDENCE_REVIEWER_SYSTEM, REPORT_REPAIR_SYSTEM, REPORT_WRITER_SYSTEM

REVIEW_SOURCE_CHAR_LIMIT = 3500
REVIEW_TOTAL_CHAR_BUDGET = 120000
WRITE_SOURCE_CHAR_LIMIT = 7000
WRITE_TOTAL_CHAR_BUDGET = 220000


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


class WebSearchClient:
    """Web search and deep research via Parallel."""

    def __init__(
        self,
        parallel_api_key: str | None = None,
        parallel_api_base_url: str | None = None,
        parallel_mcp_url: str | None = None,
        parallel_deep_research_processor: str | None = None,
        anthropic_api_key: str | None = None,
        synthesis_model: str | None = None,
        max_retries: int = 3,
    ) -> None:
        # PARALLEL_API_KEY / ANTHROPIC_API_KEY: read via secret() so the
        # firewall (StubBackend → mitmproxy) can swap in real values for
        # outbound headers. _is_configured() is the routing signal — it
        # checks ctx.secrets membership rather than relying on secret()'s
        # stub-as-placeholder fallback.
        self._has_parallel_key = parallel_api_key is not None or _is_configured("PARALLEL_API_KEY")
        self._has_anthropic_key = anthropic_api_key is not None or _is_configured(
            "ANTHROPIC_API_KEY"
        )
        self._parallel_api_key = parallel_api_key or (
            secret("PARALLEL_API_KEY") if self._has_parallel_key else None
        )
        self._anthropic_api_key = anthropic_api_key or (
            secret("ANTHROPIC_API_KEY") if self._has_anthropic_key else None
        )
        # Non-secret config: hardcoded defaults, overridable via constructor
        # args. We deliberately do NOT route these through secret() — under
        # StubBackend that would return the literal key name as a value.
        self._api_base_url = parallel_api_base_url or API_BASE_URL
        self._mcp_url = parallel_mcp_url or MCP_URL
        self._deep_research_processor = parallel_deep_research_processor or "ultra-fast"
        self._synthesis_model = synthesis_model or "claude-opus-4-6"
        self._max_retries = max_retries
        self._progress_callback: Callable[[str], None] | None = None
        self._backend = ParallelBackend(
            api_key=self._parallel_api_key if self._has_parallel_key else None,
            api_base_url=self._api_base_url,
            mcp_url=self._mcp_url,
            deep_research_processor=self._deep_research_processor,
            max_retries=self._max_retries,
        )

    def _set_progress_callback(self, callback: Callable[[str], None] | None) -> None:
        self._progress_callback = callback

    def _emit_progress(self, stage: str) -> None:
        if self._progress_callback is not None:
            self._progress_callback(stage)

    @property
    def search_mode(self) -> str:
        return self._backend.search_mode

    @property
    def has_api_key(self) -> bool:
        return self._backend.has_api_key

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
        """Search the web via Parallel.

        Args:
          query: Required. The user's question or topic. Used as the search
            objective (and as a single-query fallback per Parallel best
            practices).
          synthesize: When true, runs the Claude reviewer + writer pipeline
            on top of Parallel results (matches the prior Exa+Claude
            behavior). Requires `ANTHROPIC_API_KEY`; without one the call
            still returns raw results and records the skipped synthesis in
            `meta.partial_failures`.
          mode: `basic` (lower latency, 2-3 queries) or `advanced` (default,
            higher quality). REST path only.
          client_model: Identifier of the LLM that will consume the
            excerpts. Enables per-model optimization. Forwarded to MCP as
            `model_name`.
          max_chars_total: Hard cap on total excerpt characters. REST only.
          include_domains / exclude_domains / max_age_hours: Source filters
            for the REST path. Not honored by the free MCP — surfaced in
            `meta.partial_failures` when used without a key. `max_age_hours`
            is rounded down to a UTC calendar-date cutoff (Parallel's
            source policy is date-granular, not hour-precise).
          session_id: Stable identifier (UUID recommended) reused across
            related Search/Extract calls. Required for free-tier rate
            limiting when called over MCP.
          thread_context: Optional prior-turn context strings passed to the
            synthesis reviewer/writer for disambiguation. Synthesis only.
          search_type: Accepted for backward compatibility with the original
            Exa-backed tool; ignored under Parallel retrieval. Pass `None`.
        """
        if search_type is not None:
            warnings.warn(
                "search_type is ignored — the Parallel retrieval backend does "
                "not expose Exa's neural/keyword/auto modes.",
                DeprecationWarning,
                stacklevel=2,
            )
        synthesis_pipeline = self._build_synthesis_pipeline() if synthesize else None
        return await self._backend.search(
            objective=query,
            search_queries=[query],
            num_results=num_results,
            timeout_seconds=timeout_seconds,
            synthesize=synthesize,
            mode=mode,
            client_model=client_model,
            max_chars_total=max_chars_total,
            include_domains=include_domains,
            exclude_domains=exclude_domains,
            max_age_hours=max_age_hours,
            session_id=session_id,
            max_report_chars=max_report_chars,
            synthesis_pipeline=synthesis_pipeline,
            thread_context=thread_context,
        )

    async def deep_research(
        self,
        question: str,
        *,
        processor: str | None = None,
        timeout_seconds: float | None = None,
        max_report_chars: int = 50000,
        max_iterations: int | None = None,
        num_queries_per_iteration: int | None = None,
        num_results_per_query: int | None = None,
        thread_context: list[str] | None = None,
    ) -> dict:
        """Run deep research and return a cited markdown report.

        Args:
          processor: Task API processor (pro/ultra family). Defaults to
            the value passed to `WebSearchClient(parallel_deep_research_processor=...)`,
            or `"ultra-fast"` if neither is set.
          timeout_seconds: Override the request timeout. When omitted, a
            processor-appropriate default is used (e.g. `ultra4x` waits up
            to ~2h).
          max_iterations / num_queries_per_iteration / num_results_per_query /
          thread_context: Accepted for backward compatibility with the
            original Exa+Claude iterative pipeline; ignored under Parallel
            Task API (a single multi-source run replaces the iterative
            planner→search→reviewer→writer loop).

        Requires `PARALLEL_API_KEY`.
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
                f"deep_research kwargs ignored under Parallel Task API: {used}. "
                "The new backend is single-call; iteration knobs no longer apply.",
                DeprecationWarning,
                stacklevel=2,
            )
        return await self._backend.deep_research(
            question=question,
            progress=self._emit_progress,
            processor=processor,
            timeout_seconds=timeout_seconds,
            max_report_chars=max_report_chars,
        )


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
