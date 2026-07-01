from __future__ import annotations

from api.metrics import increment_metric, observe_histogram, set_gauge


_COMPANY_CONTEXT_DOCUMENT_SIZE_BUCKETS = [
    100,
    500,
    1_000,
    5_000,
    10_000,
    25_000,
    50_000,
    100_000,
    250_000,
    500_000,
]


def record_company_context_documents_changed(
    source: str,
    source_type: str,
    action: str,
    count: int = 1,
) -> None:
    increment_metric(
        "company_context_documents_changed_total",
        count,
        source=source,
        source_type=source_type,
        action=action,
    )


def observe_company_context_document_size(source: str, source_type: str, chars: int) -> None:
    observe_histogram(
        "company_context_document_size_chars",
        max(chars, 0),
        _COMPANY_CONTEXT_DOCUMENT_SIZE_BUCKETS,
        source=source,
        source_type=source_type,
    )


def set_company_context_projection_lag(source: str, projection_lag_s: float) -> None:
    set_gauge(
        "company_context_projection_lag_seconds",
        max(projection_lag_s, 0.0),
        source=source,
    )
