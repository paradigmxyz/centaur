from __future__ import annotations

from api.metrics import increment_metric, set_gauge


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


def set_company_context_projection_lag(source: str, projection_lag_s: float) -> None:
    set_gauge(
        "company_context_projection_lag_seconds",
        max(projection_lag_s, 0.0),
        source=source,
    )
