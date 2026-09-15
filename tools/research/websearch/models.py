"""Pydantic models for websearch tool contracts."""

from __future__ import annotations

from typing import Any, Literal

from pydantic import BaseModel, Field, model_validator


class SourceDocument(BaseModel):
    source_id: int
    title: str
    url: str
    snippet: str = ""
    published_date: str | None = None
    domain: str | None = None


class ResponseMeta(BaseModel):
    duration_ms: int
    request_ids: list[str] = Field(default_factory=list)
    partial_failures: list[dict[str, str]] = Field(default_factory=list)
    backend: str | None = None
    estimated_cost_usd: float | None = None
    usage: list[dict[str, Any]] = Field(default_factory=list)
    # Attribution for the upstream provider when applicable (e.g. the free
    # hosted Parallel Search MCP). Surface in UIs that display result metadata.
    attribution: str | None = None
    # Backward-compat alias for `request_ids`. The original Exa-based tool
    # exposed `exa_request_ids`; external consumers may still read it.
    exa_request_ids: list[str] = Field(default_factory=list)

    @model_validator(mode="after")
    def _mirror_request_ids(self) -> ResponseMeta:
        if self.request_ids and not self.exa_request_ids:
            self.exa_request_ids = list(self.request_ids)
        elif self.exa_request_ids and not self.request_ids:
            self.request_ids = list(self.exa_request_ids)
        return self


class SearchResponse(BaseModel):
    query: str
    results: list[SourceDocument]
    answer_markdown: str | None = None
    meta: ResponseMeta


class DeepResearchIteration(BaseModel):
    """Retained for backward-compat with the original tool's response shape.

    The new Parallel Task API path is single-call rather than iterative, so
    `iterations` always contains a single synthetic entry representing the run.
    """

    iteration: int
    queries: list[str]
    results_count: int
    continue_reason: str = ""


class DeepResearchResponse(BaseModel):
    question: str
    answer_markdown: str
    sources: list[SourceDocument]
    iterations: list[DeepResearchIteration] = Field(default_factory=list)
    meta: ResponseMeta


SearchEffort = Literal["instant", "fast", "deep"]
ResearchEffort = Literal["medium", "high"]


class SearchRequestSpec(BaseModel):
    """What a backend needs to retrieve sources. Built by `WebSearchClient.search`."""

    query: str
    num_results: int = 10
    timeout_seconds: float = 60.0
    include_domains: list[str] | None = None
    exclude_domains: list[str] | None = None
    max_age_hours: int | None = None
    effort: SearchEffort | None = None
    client_model: str | None = None
    max_chars_total: int | None = None
    session_id: str | None = None


class RetrievalResult(BaseModel):
    """What a backend returns from `search`: sources plus provenance, no synthesis."""

    sources: list[SourceDocument]
    backend: str
    request_ids: list[str] = Field(default_factory=list)
    usage: list[dict[str, Any]] = Field(default_factory=list)
    partial_failures: list[dict[str, str]] = Field(default_factory=list)
    attribution: str | None = None
    estimated_cost_usd: float | None = None


class DeepResearchSpec(BaseModel):
    """What a backend needs to run research. `processor` is a Parallel-only passthrough."""

    question: str
    effort: ResearchEffort | None = None
    processor: str | None = None
    timeout_seconds: float | None = None
    max_report_chars: int = 50000


class DeepResearchResult(BaseModel):
    """What a backend returns from `deep_research`: a cited report plus provenance."""

    sources: list[SourceDocument]
    answer_markdown: str
    backend: str
    request_ids: list[str] = Field(default_factory=list)
    partial_failures: list[dict[str, str]] = Field(default_factory=list)
    usage: list[dict[str, Any]] = Field(default_factory=list)
    estimated_cost_usd: float | None = None
