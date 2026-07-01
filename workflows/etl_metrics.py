from __future__ import annotations

from api.metrics import increment_metric, set_gauge


def record_etl_items_seen(source: str, source_type: str, item_type: str, count: int) -> None:
    increment_metric(
        "etl_items_seen_total",
        count,
        source=source,
        source_type=source_type,
        item_type=item_type,
    )


def record_etl_items_enqueued(source: str, source_type: str, item_type: str, count: int) -> None:
    increment_metric(
        "etl_items_enqueued_total",
        count,
        source=source,
        source_type=source_type,
        item_type=item_type,
    )


def record_etl_items_upserted(source: str, source_type: str, item_type: str, count: int) -> None:
    increment_metric(
        "etl_items_upserted_total",
        count,
        source=source,
        source_type=source_type,
        item_type=item_type,
    )


def record_etl_items_deleted(source: str, source_type: str, item_type: str, count: int) -> None:
    increment_metric(
        "etl_items_deleted_total",
        count,
        source=source,
        source_type=source_type,
        item_type=item_type,
    )


def record_etl_items_failed(
    source: str,
    source_type: str,
    item_type: str,
    reason: str,
    count: int = 1,
) -> None:
    increment_metric(
        "etl_items_failed_total",
        count,
        source=source,
        source_type=source_type,
        item_type=item_type,
        reason=reason,
    )


def set_etl_active_scopes(source: str, count: int) -> None:
    set_gauge("etl_active_scopes", max(count, 0), source=source)


def set_etl_failed_scopes(source: str, count: int) -> None:
    set_gauge("etl_failed_scopes", max(count, 0), source=source)


def set_etl_scope_sync_freshness_seconds(source: str, freshness_s: int | float) -> None:
    set_gauge("etl_scope_sync_freshness_seconds", max(float(freshness_s), 0.0), source=source)


def set_etl_backfill_jobs(source: str, job_type: str, status: str, count: int) -> None:
    set_gauge(
        "etl_backfill_jobs",
        max(count, 0),
        source=source,
        job_type=job_type,
        status=status,
    )

def set_etl_backfill_job_age_seconds(
    source: str,
    job_type: str,
    status: str,
    age_s: int | float,
) -> None:
    set_gauge(
        "etl_backfill_job_age_seconds",
        max(float(age_s), 0.0),
        source=source,
        job_type=job_type,
        status=status,
    )
