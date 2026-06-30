from __future__ import annotations

from api.metrics import increment_metric, observe_histogram, set_gauge


_SLACK_ARCHIVE_IMPORT_BATCH_SIZE_BUCKETS = [1, 10, 100, 500, 1_000, 5_000, 10_000]
_SLACK_ARCHIVE_IMPORT_DURATION_BUCKETS = [1, 5, 10, 30, 60, 120, 300, 600, 1_200, 3_600]
_SLACK_RETENTION_DURATION_BUCKETS = [1, 5, 10, 30, 60, 120, 300, 600, 1_200]


def record_slack_etl_rate_limit(
    workflow: str,
    method: str,
    outcome: str,
    retry_after_seconds: int | float,
) -> None:
    retry_after = max(float(retry_after_seconds), 0.0)
    labels = {"workflow": workflow, "method": method, "outcome": outcome}
    increment_metric("slack_etl_rate_limits_total", 1, **labels)
    increment_metric("slack_etl_rate_limit_retry_after_seconds_total", retry_after, **labels)


def record_slack_archive_import_run(
    status: str,
    reason: str = "none",
    count: int = 1,
) -> None:
    increment_metric("slack_archive_import_runs_total", count, status=status, reason=reason)


def observe_slack_archive_import_duration(status: str, duration_s: float) -> None:
    observe_histogram(
        "slack_archive_import_duration_seconds",
        max(duration_s, 0.0),
        _SLACK_ARCHIVE_IMPORT_DURATION_BUCKETS,
        status=status,
    )


def record_slack_archive_import_bytes(count: int) -> None:
    increment_metric("slack_archive_import_bytes_total", count)


def record_slack_archive_import_channels(result: str, count: int) -> None:
    increment_metric("slack_archive_import_channels_total", count, result=result)


def record_slack_archive_import_users(result: str, count: int) -> None:
    increment_metric("slack_archive_import_users_total", count, result=result)


def record_slack_archive_import_messages(result: str, count: int) -> None:
    increment_metric("slack_archive_import_messages_total", count, result=result)


def record_slack_archive_import_message_files(result: str, count: int) -> None:
    increment_metric("slack_archive_import_message_files_total", count, result=result)


def record_slack_archive_import_attachments(result: str, count: int) -> None:
    increment_metric("slack_archive_import_attachments_total", count, result=result)


def observe_slack_archive_import_batch_duration(entity: str, duration_s: float) -> None:
    observe_histogram(
        "slack_archive_import_batch_duration_seconds",
        max(duration_s, 0.0),
        _SLACK_ARCHIVE_IMPORT_DURATION_BUCKETS,
        entity=entity,
    )


def record_slack_archive_import_batch_size(entity: str, count: int) -> None:
    observe_histogram(
        "slack_archive_import_batch_size",
        max(count, 0),
        _SLACK_ARCHIVE_IMPORT_BATCH_SIZE_BUCKETS,
        entity=entity,
    )


def record_slack_archive_import_failure(stage: str, reason: str, count: int = 1) -> None:
    increment_metric("slack_archive_import_failures_total", count, stage=stage, reason=reason)


def record_slack_archive_import_skipped_items(
    item_type: str,
    reason: str,
    count: int = 1,
) -> None:
    increment_metric(
        "slack_archive_import_skipped_items_total",
        count,
        item_type=item_type,
        reason=reason,
    )


def record_slack_archive_import_batch_failure(
    entity: str,
    reason: str,
    count: int = 1,
) -> None:
    increment_metric(
        "slack_archive_import_batch_failures_total",
        count,
        entity=entity,
        reason=reason,
    )


def set_slack_archive_import_last_failure_timestamp(timestamp_s: float) -> None:
    set_gauge("slack_archive_import_last_failure_timestamp_seconds", timestamp_s)


def record_slack_retention_run(
    workflow: str,
    status: str,
    mode: str,
    reason: str = "none",
    count: int = 1,
) -> None:
    increment_metric(
        "slack_retention_runs_total",
        count,
        workflow=workflow,
        status=status,
        mode=mode,
        reason=reason,
    )


def observe_slack_retention_run_duration(
    workflow: str,
    mode: str,
    status: str,
    duration_s: float,
) -> None:
    observe_histogram(
        "slack_retention_run_duration_seconds",
        max(duration_s, 0.0),
        _SLACK_RETENTION_DURATION_BUCKETS,
        workflow=workflow,
        mode=mode,
        status=status,
    )


def record_slack_retention_messages_processed(
    workflow: str,
    mode: str,
    result: str,
    count: int,
) -> None:
    increment_metric(
        "slack_retention_messages_processed_total",
        count,
        workflow=workflow,
        mode=mode,
        result=result,
    )


def record_slack_retention_backfill_job(
    job_type: str,
    result: str,
    reason: str = "none",
    count: int = 1,
) -> None:
    increment_metric(
        "slack_retention_backfill_jobs_total",
        count,
        job_type=job_type,
        result=result,
        reason=reason,
    )


def record_slack_retention_failure(
    workflow: str,
    operation: str,
    reason: str,
    count: int = 1,
) -> None:
    increment_metric(
        "slack_retention_failures_total",
        count,
        workflow=workflow,
        operation=operation,
        reason=reason,
    )


def record_slack_retention_api_request(
    operation: str,
    result: str,
    reason: str = "none",
    count: int = 1,
) -> None:
    increment_metric(
        "slack_retention_api_requests_total",
        count,
        operation=operation,
        result=result,
        reason=reason,
    )


def record_slack_retention_api_rate_limited(operation: str, count: int = 1) -> None:
    increment_metric("slack_retention_api_rate_limited_total", count, operation=operation)


def record_slack_retention_backfill_job_failure(
    job_type: str,
    reason: str,
    count: int = 1,
) -> None:
    increment_metric(
        "slack_retention_backfill_job_failures_total",
        count,
        job_type=job_type,
        reason=reason,
    )


def record_slack_retention_backfill_terminal_skip(
    job_type: str,
    reason: str,
    count: int = 1,
) -> None:
    increment_metric(
        "slack_retention_backfill_terminal_skips_total",
        count,
        job_type=job_type,
        reason=reason,
    )


def record_slack_retention_channel_failure(
    workflow: str,
    reason: str,
    count: int = 1,
) -> None:
    increment_metric(
        "slack_retention_channel_failures_total",
        count,
        workflow=workflow,
        reason=reason,
    )


def set_slack_retention_last_failure_timestamp(workflow: str, timestamp_s: float) -> None:
    set_gauge("slack_retention_last_failure_timestamp_seconds", timestamp_s, workflow=workflow)


def set_slack_retention_watermark_lag_seconds(mode: str, lag_s: float) -> None:
    set_gauge("slack_retention_watermark_lag_seconds", max(lag_s, 0.0), mode=mode)
