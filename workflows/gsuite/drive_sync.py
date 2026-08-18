"""Workflow: sync Google Drive Docs visible to the configured ETL account."""

from __future__ import annotations

import datetime as dt
import hashlib
import os
from dataclasses import dataclass, field
from typing import Any, Protocol

from workflows.gsuite.drive import GOOGLE_DOC_MIME_TYPE
from api.runtime_control import canonical_json
from workflows.etl_metrics import (
    record_etl_items_failed,
    record_etl_items_seen,
    record_etl_items_upserted,
)
from api.workflow_engine import WorkflowContext
from workflows.slack.shared import env_flag_enabled, positive_int

WORKFLOW_NAME = "google_drive_sync"
DEFAULT_SYNC_INTERVAL_SECONDS = 4 * 60 * 60
DEFAULT_PAGE_SIZE = 100
DEFAULT_WATERMARK_OVERLAP_SECONDS = 60


SCHEDULE = {
    "schedule_id": "google_drive_sync",
    "interval_seconds": positive_int(
        os.getenv("GOOGLE_DRIVE_SYNC_INTERVAL_SECONDS"),
        DEFAULT_SYNC_INTERVAL_SECONDS,
    ),
    "enabled": env_flag_enabled("GOOGLE_DRIVE_ETL_ENABLED", default=False),
    "no_delivery": True,
}


@dataclass
class Input:
    """Runtime options for a manual Google Drive sync workflow run."""

    since: str | None = None
    limit: int = DEFAULT_PAGE_SIZE
    watermark_overlap_seconds: int = DEFAULT_WATERMARK_OVERLAP_SECONDS
    metadata: dict[str, Any] = field(default_factory=dict)


class GoogleDriveSyncClient(Protocol):
    """Small adapter protocol used by the Drive ETL workflow."""

    def list_docs(
        self,
        *,
        query: str,
        page_size: int,
        page_token: str | None = None,
    ) -> dict[str, Any]: ...

    def docs_get_text(self, document_id: str) -> str: ...


def _client() -> GoogleDriveSyncClient:
    from workflows.gsuite.drive import GoogleDriveReadonlyClient

    return GoogleDriveReadonlyClient()


def _parse_datetime(value: str | None) -> dt.datetime | None:
    if not value:
        return None
    try:
        parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=dt.timezone.utc)
    return parsed.astimezone(dt.timezone.utc)


def _rfc3339(value: dt.datetime) -> str:
    return value.astimezone(dt.timezone.utc).isoformat().replace("+00:00", "Z")


def _drive_literal(value: str) -> str:
    return "'" + value.replace("\\", "\\\\").replace("'", "\\'") + "'"


def _content_hash(*parts: Any) -> str:
    return hashlib.sha256(canonical_json(parts).encode("utf-8")).hexdigest()


def _file_modified_time(file: dict[str, Any]) -> dt.datetime | None:
    return _parse_datetime(str(file.get("modifiedTime") or ""))


def _file_created_time(file: dict[str, Any]) -> dt.datetime | None:
    return _parse_datetime(str(file.get("createdTime") or ""))


def _owner_names(owners: Any) -> list[str]:
    if not isinstance(owners, list):
        return []
    names: list[str] = []
    for owner in owners:
        if not isinstance(owner, dict):
            continue
        name = str(owner.get("displayName") or owner.get("emailAddress") or "").strip()
        if name:
            names.append(name)
    return names


def _build_query(
    *,
    modified_after: dt.datetime | None,
) -> str:
    parts = [
        f"mimeType = {_drive_literal(GOOGLE_DOC_MIME_TYPE)}",
        "trashed = false",
    ]
    if modified_after:
        parts.append(f"modifiedTime > {_drive_literal(_rfc3339(modified_after))}")
    return " and ".join(parts)


async def _load_checkpoint(pool, scope_id: str) -> dict[str, Any] | None:
    row = await pool.fetchrow(
        "SELECT watermark_time, last_error FROM google_drive_sync_checkpoints "
        "WHERE scope_id = $1",
        scope_id,
    )
    return dict(row) if row else None


async def _update_checkpoint_success(
    pool,
    *,
    scope_id: str,
    watermark_time: dt.datetime | None,
    run_id: str,
) -> None:
    await pool.execute(
        "INSERT INTO google_drive_sync_checkpoints ("
        "scope_id, watermark_time, last_run_id, last_success_at, "
        "last_error, updated_at"
        ") VALUES ($1, $2, $3, NOW(), '', NOW()) "
        "ON CONFLICT (scope_id) DO UPDATE SET "
        "watermark_time = COALESCE(EXCLUDED.watermark_time, google_drive_sync_checkpoints.watermark_time), "
        "last_run_id = EXCLUDED.last_run_id, "
        "last_success_at = NOW(), "
        "last_error = '', "
        "updated_at = NOW()",
        scope_id,
        watermark_time,
        run_id,
    )


async def _update_checkpoint_failure(
    pool,
    *,
    scope_id: str,
    run_id: str,
    error: str,
) -> None:
    await pool.execute(
        "INSERT INTO google_drive_sync_checkpoints ("
        "scope_id, last_run_id, last_error, updated_at"
        ") VALUES ($1, $2, $3, NOW()) "
        "ON CONFLICT (scope_id) DO UPDATE SET "
        "last_run_id = EXCLUDED.last_run_id, "
        "last_error = EXCLUDED.last_error, "
        "updated_at = NOW()",
        scope_id,
        run_id,
        error,
    )


async def _record_run_start(
    pool,
    *,
    run_id: str,
    workflow_run_id: str,
    scopes_requested: list[dict[str, str]],
    metadata: dict[str, Any],
) -> None:
    await pool.execute(
        "INSERT INTO google_drive_sync_runs ("
        "run_id, workflow_run_id, mode, status, scopes_requested, metadata"
        ") VALUES ($1, $2, 'incremental', 'running', $3::jsonb, $4::jsonb) "
        "ON CONFLICT (run_id) DO UPDATE SET "
        "workflow_run_id = EXCLUDED.workflow_run_id, "
        "status = 'running', "
        "scopes_requested = EXCLUDED.scopes_requested, "
        "scopes_synced = '[]'::jsonb, "
        "scopes_failed = '[]'::jsonb, "
        "files_seen = 0, "
        "files_upserted = 0, "
        "docs_fetched = 0, "
        "docs_upserted = 0, "
        "finished_at = NULL, "
        "error_text = '', "
        "metadata = EXCLUDED.metadata",
        run_id,
        workflow_run_id,
        canonical_json(scopes_requested),
        canonical_json(metadata),
    )


async def _record_run_finish(
    pool,
    *,
    run_id: str,
    status: str,
    scopes_synced: list[dict[str, str]],
    scopes_failed: list[dict[str, str]],
    counts: dict[str, int],
    error_text: str = "",
) -> None:
    await pool.execute(
        "UPDATE google_drive_sync_runs SET "
        "status = $2, scopes_synced = $3::jsonb, scopes_failed = $4::jsonb, "
        "files_seen = $5, files_upserted = $6, docs_fetched = $7, docs_upserted = $8, "
        "finished_at = NOW(), error_text = $9 "
        "WHERE run_id = $1",
        run_id,
        status,
        canonical_json(scopes_synced),
        canonical_json(scopes_failed),
        counts.get("files_seen", 0),
        counts.get("files_upserted", 0),
        counts.get("docs_fetched", 0),
        counts.get("docs_upserted", 0),
        error_text,
    )


async def _record_file_error(
    pool,
    *,
    file_id: str,
    error: str,
    run_id: str,
) -> None:
    await pool.execute(
        "INSERT INTO google_drive_sync_files ("
        "file_id, last_error, source_run_id, last_seen_at, updated_at"
        ") VALUES ($1, $2, $3, NOW(), NOW()) "
        "ON CONFLICT (file_id) DO UPDATE SET "
        "last_error = EXCLUDED.last_error, "
        "source_run_id = EXCLUDED.source_run_id, "
        "updated_at = NOW()",
        file_id,
        error,
        run_id,
    )


async def _upsert_file(
    pool,
    *,
    file: dict[str, Any],
    text: str,
    run_id: str,
) -> None:
    owners = file.get("owners") if isinstance(file.get("owners"), list) else []
    last_modifying_user = (
        file.get("lastModifyingUser")
        if isinstance(file.get("lastModifyingUser"), dict)
        else {}
    )
    parent_ids = file.get("parents") if isinstance(file.get("parents"), list) else []
    await pool.execute(
        "INSERT INTO google_drive_sync_files ("
        "file_id, name, mime_type, web_view_link, drive_id, parent_ids, owners, "
        "last_modifying_user, trashed, source_created_at, source_modified_at, "
        "text_content, text_hash, raw_payload, source_run_id, last_seen_at, "
        "last_content_synced_at, last_error, updated_at"
        ") VALUES ("
        "$1, $2, $3, $4, $5, $6::jsonb, $7::jsonb, $8::jsonb, $9, $10, $11, "
        "$12, $13, $14::jsonb, $15, NOW(), NOW(), '', NOW()"
        ") ON CONFLICT (file_id) DO UPDATE SET "
        "name = EXCLUDED.name, "
        "mime_type = EXCLUDED.mime_type, "
        "web_view_link = EXCLUDED.web_view_link, "
        "drive_id = EXCLUDED.drive_id, "
        "parent_ids = EXCLUDED.parent_ids, "
        "owners = EXCLUDED.owners, "
        "last_modifying_user = EXCLUDED.last_modifying_user, "
        "trashed = EXCLUDED.trashed, "
        "source_created_at = EXCLUDED.source_created_at, "
        "source_modified_at = EXCLUDED.source_modified_at, "
        "text_content = EXCLUDED.text_content, "
        "text_hash = EXCLUDED.text_hash, "
        "raw_payload = EXCLUDED.raw_payload, "
        "source_run_id = EXCLUDED.source_run_id, "
        "last_seen_at = NOW(), "
        "last_content_synced_at = NOW(), "
        "last_error = '', "
        "updated_at = NOW()",
        str(file.get("id") or ""),
        str(file.get("name") or ""),
        str(file.get("mimeType") or ""),
        str(file.get("webViewLink") or ""),
        str(file.get("driveId") or ""),
        canonical_json(parent_ids),
        canonical_json(owners),
        canonical_json(last_modifying_user),
        bool(file.get("trashed")),
        _file_created_time(file),
        _file_modified_time(file),
        text,
        _content_hash(text),
        canonical_json(file),
        run_id,
    )


def _workflow_run_id_to_sync_run_id(workflow_run_id: str) -> str:
    safe_run_id = "".join(char if char.isalnum() else "_" for char in workflow_run_id)
    return f"google_drive_sync_{safe_run_id}"


def _scope_ref(scope_id: str, reason: str | None = None) -> dict[str, str]:
    result = {"scope_id": scope_id}
    if reason:
        result["reason"] = reason
    return result


async def handler(inp: Input, ctx: WorkflowContext) -> dict[str, Any]:
    """Sync changed Google Docs into raw Drive sync tables."""
    if not env_flag_enabled("GOOGLE_DRIVE_ETL_ENABLED", default=False):
        ctx.log("google_drive_sync_skipped_disabled")
        return {"status": "skipped", "reason": "google_drive_etl_disabled"}

    limit = positive_int(inp.limit, DEFAULT_PAGE_SIZE)
    overlap_seconds = max(int(inp.watermark_overlap_seconds), 0)
    run_id = _workflow_run_id_to_sync_run_id(ctx.run_id)

    scope_id = "all_visible"
    explicit_since = _parse_datetime(inp.since)
    checkpoint = await _load_checkpoint(ctx._pool, scope_id)
    watermark = explicit_since
    if watermark is None and checkpoint and checkpoint.get("watermark_time"):
        watermark = checkpoint["watermark_time"].astimezone(dt.timezone.utc)
    if watermark is not None:
        watermark = watermark - dt.timedelta(seconds=overlap_seconds)
    query = _build_query(modified_after=watermark)

    await _record_run_start(
        ctx._pool,
        run_id=run_id,
        workflow_run_id=ctx.run_id,
        scopes_requested=[_scope_ref(scope_id)],
        metadata={
            **inp.metadata,
            "page_size": limit,
        },
    )

    client = _client()
    synced: list[dict[str, str]] = []
    failed: list[dict[str, str]] = []
    counts = {
        "files_seen": 0,
        "files_upserted": 0,
        "docs_fetched": 0,
        "docs_upserted": 0,
    }

    for scope_id, query in [(scope_id, query)]:
        successful_watermark: dt.datetime | None = None
        try:
            page_token: str | None = None
            while True:
                page = client.list_docs(
                    query=query, page_size=limit, page_token=page_token
                )
                files = [
                    file
                    for file in page.get("files", [])
                    if str(file.get("id") or "")
                    and str(file.get("mimeType") or "") == GOOGLE_DOC_MIME_TYPE
                ]
                counts["files_seen"] += len(files)
                record_etl_items_seen("google_drive", "doc", "file", len(files))
                for file in files:
                    file_id = str(file.get("id") or "")
                    modified_at = _file_modified_time(file)
                    try:
                        text = client.docs_get_text(file_id)
                        counts["docs_fetched"] += 1
                        await _upsert_file(
                            ctx._pool, file=file, text=text, run_id=run_id
                        )
                        counts["files_upserted"] += 1
                        counts["docs_upserted"] += 1
                        record_etl_items_upserted("google_drive", "doc", "file", 1)
                        if modified_at and (
                            successful_watermark is None
                            or modified_at > successful_watermark
                        ):
                            successful_watermark = modified_at
                    except Exception as exc:
                        error = str(exc)
                        failed.append(_scope_ref(scope_id, f"file:{file_id}:{error}"))
                        record_etl_items_failed(
                            "google_drive",
                            "doc",
                            "file",
                            "permission_error"
                            if "permission" in error.lower() or "403" in error
                            else "api_error",
                        )
                        await _record_file_error(
                            ctx._pool,
                            file_id=file_id,
                            error=error,
                            run_id=run_id,
                        )
                        ctx.log(
                            "google_drive_sync_file_failed",
                            scope_id=scope_id,
                            file_id=file_id,
                            file_name=str(file.get("name") or ""),
                            error=error,
                        )
                page_token = page.get("nextPageToken")
                if not page_token:
                    break
            await _update_checkpoint_success(
                ctx._pool,
                scope_id=scope_id,
                watermark_time=successful_watermark,
                run_id=run_id,
            )
            synced.append(_scope_ref(scope_id))
            ctx.log(
                "google_drive_sync_scope_completed",
                scope_id=scope_id,
                files_seen=counts["files_seen"],
                files_upserted=counts["files_upserted"],
                docs_fetched=counts["docs_fetched"],
                docs_upserted=counts["docs_upserted"],
                watermark=_rfc3339(successful_watermark)
                if successful_watermark
                else "",
            )
        except Exception as exc:
            error = str(exc)
            failed.append(_scope_ref(scope_id, error))
            record_etl_items_failed("google_drive", "doc", "scope", "api_error")
            await _update_checkpoint_failure(
                ctx._pool,
                scope_id=scope_id,
                run_id=run_id,
                error=error,
            )
            ctx.log(
                "google_drive_sync_scope_failed",
                scope_id=scope_id,
                error=error,
            )

    status = "completed"
    error_text = ""
    if failed and synced:
        status = "partial_failed"
        error_text = f"{len(failed)} Drive item(s) failed"
    elif failed:
        status = "failed"
        error_text = f"{len(failed)} Drive item(s) failed"

    await _record_run_finish(
        ctx._pool,
        run_id=run_id,
        status=status,
        scopes_synced=synced,
        scopes_failed=failed,
        counts=counts,
        error_text=error_text,
    )

    return {
        "status": status,
        "run_id": run_id,
        "scopes_synced": len(synced),
        "scopes_failed": len(failed),
        **counts,
    }
