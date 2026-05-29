"""Pydantic models for websearch tool contracts."""

from __future__ import annotations

from typing import Any

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
