"""Workflow: sync Linear projects, issues, and comments into Postgres."""

from __future__ import annotations

import datetime as dt
import hashlib
import os
from dataclasses import dataclass, field
from typing import Any, Protocol

from api.runtime_control import canonical_json
from api.vm_metrics import (
    record_etl_items_failed,
    record_etl_items_seen,
    record_etl_items_upserted,
)
from api.workflow_engine import WorkflowContext
from workflows.slack.shared import env_flag_enabled, positive_int

WORKFLOW_NAME = "linear_sync"
DEFAULT_SYNC_INTERVAL_SECONDS = 4 * 60 * 60
DEFAULT_PAGE_SIZE = 100
DEFAULT_WATERMARK_OVERLAP_SECONDS = 5 * 60

PROJECTS_SCOPE = "projects"
ISSUES_SCOPE = "issues"
COMMENTS_SCOPE = "comments"


SCHEDULE = {
    "schedule_id": "linear_sync",
    "interval_seconds": positive_int(
        os.getenv("LINEAR_SYNC_INTERVAL_SECONDS"),
        DEFAULT_SYNC_INTERVAL_SECONDS,
    ),
    "enabled": env_flag_enabled("LINEAR_ETL_ENABLED", default=False),
    "no_delivery": True,
}


@dataclass
class Input:
    """Runtime options for a manual Linear sync workflow run."""

    since: str | None = None
    limit: int = DEFAULT_PAGE_SIZE
    watermark_overlap_seconds: int = DEFAULT_WATERMARK_OVERLAP_SECONDS
    include_archived: bool = True
    metadata: dict[str, Any] = field(default_factory=dict)


class LinearSyncClient(Protocol):
    """Small adapter protocol used by the Linear ETL workflow."""

    def list_etl_projects(
        self,
        *,
        page_size: int,
        cursor: str | None = None,
        updated_after: dt.datetime | str | None = None,
        include_archived: bool = True,
    ) -> dict[str, Any]: ...

    def list_etl_issues(
        self,
        *,
        page_size: int,
        cursor: str | None = None,
        updated_after: dt.datetime | str | None = None,
        include_archived: bool = True,
    ) -> dict[str, Any]: ...

    def list_etl_comments(
        self,
        *,
        page_size: int,
        cursor: str | None = None,
        updated_after: dt.datetime | str | None = None,
        include_archived: bool = True,
    ) -> dict[str, Any]: ...


def _client() -> LinearSyncClient:
    from workflows.linear import LinearReadonlyClient

    return LinearReadonlyClient()


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


def _parse_date(value: Any) -> dt.date | None:
    if not value:
        return None
    try:
        return dt.date.fromisoformat(str(value))
    except ValueError:
        return None


def _source_datetime(payload: dict[str, Any], key: str) -> dt.datetime | None:
    return _parse_datetime(str(payload.get(key) or ""))


def _rfc3339(value: dt.datetime) -> str:
    return value.astimezone(dt.timezone.utc).isoformat().replace("+00:00", "Z")


def _content_hash(*parts: Any) -> str:
    return hashlib.sha256(canonical_json(parts).encode("utf-8")).hexdigest()


def _json_object(value: Any) -> dict[str, Any]:
    return value if isinstance(value, dict) else {}


def _connection_nodes(value: Any) -> list[dict[str, Any]]:
    if not isinstance(value, dict):
        return []
    nodes = value.get("nodes")
    if not isinstance(nodes, list):
        return []
    return [node for node in nodes if isinstance(node, dict)]


def _text_value(value: Any) -> str:
    return str(value or "")


def _int_value(value: Any) -> int | None:
    if value is None:
        return None
    try:
        return int(value)
    except (TypeError, ValueError):
        return None


def _float_value(value: Any) -> float | None:
    if value is None:
        return None
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


def _workflow_run_id_to_sync_run_id(workflow_run_id: str) -> str:
    safe_run_id = "".join(char if char.isalnum() else "_" for char in workflow_run_id)
    return f"linear_sync_{safe_run_id}"


def _scope_ref(scope_id: str, reason: str | None = None) -> dict[str, str]:
    result = {"scope_id": scope_id}
    if reason:
        result["reason"] = reason
    return result


def _failure_reason(error: str) -> str:
    lowered = error.lower()
    if "rate" in lowered or "429" in lowered:
        return "rate_limited"
    if (
        "forbidden" in lowered
        or "permission" in lowered
        or "401" in lowered
        or "403" in lowered
    ):
        return "permission_error"
    if "database" in lowered or "postgres" in lowered:
        return "write_error"
    return "api_error"


async def _load_checkpoint(pool, scope_id: str) -> dict[str, Any] | None:
    row = await pool.fetchrow(
        "SELECT watermark_time, last_error FROM linear_sync_checkpoints WHERE scope_id = $1",
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
        "INSERT INTO linear_sync_checkpoints ("
        "scope_id, watermark_time, last_run_id, last_success_at, last_error, updated_at"
        ") VALUES ($1, $2, $3, NOW(), '', NOW()) "
        "ON CONFLICT (scope_id) DO UPDATE SET "
        "watermark_time = COALESCE(EXCLUDED.watermark_time, linear_sync_checkpoints.watermark_time), "
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
        "INSERT INTO linear_sync_checkpoints ("
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
        "INSERT INTO linear_sync_runs ("
        "run_id, workflow_run_id, mode, status, scopes_requested, metadata"
        ") VALUES ($1, $2, 'incremental', 'running', $3::jsonb, $4::jsonb) "
        "ON CONFLICT (run_id) DO UPDATE SET "
        "workflow_run_id = EXCLUDED.workflow_run_id, "
        "status = 'running', "
        "scopes_requested = EXCLUDED.scopes_requested, "
        "scopes_synced = '[]'::jsonb, "
        "scopes_failed = '[]'::jsonb, "
        "projects_seen = 0, "
        "projects_upserted = 0, "
        "issues_seen = 0, "
        "issues_upserted = 0, "
        "comments_seen = 0, "
        "comments_upserted = 0, "
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
        "UPDATE linear_sync_runs SET "
        "status = $2, scopes_synced = $3::jsonb, scopes_failed = $4::jsonb, "
        "projects_seen = $5, projects_upserted = $6, issues_seen = $7, "
        "issues_upserted = $8, comments_seen = $9, comments_upserted = $10, "
        "finished_at = NOW(), error_text = $11 "
        "WHERE run_id = $1",
        run_id,
        status,
        canonical_json(scopes_synced),
        canonical_json(scopes_failed),
        counts.get("projects_seen", 0),
        counts.get("projects_upserted", 0),
        counts.get("issues_seen", 0),
        counts.get("issues_upserted", 0),
        counts.get("comments_seen", 0),
        counts.get("comments_upserted", 0),
        error_text,
    )


async def _upsert_project(
    pool,
    *,
    project: dict[str, Any],
    run_id: str,
) -> None:
    status = _json_object(project.get("status"))
    lead = _json_object(project.get("lead"))
    teams = _connection_nodes(project.get("teams"))
    team_ids = [_text_value(team.get("id")) for team in teams if team.get("id")]
    team_keys = [_text_value(team.get("key")) for team in teams if team.get("key")]
    content_text = "\n".join(
        part
        for part in [
            _text_value(project.get("name")),
            _text_value(project.get("description")),
            _text_value(status.get("name")),
            _text_value(lead.get("name") or lead.get("displayName")),
            " ".join(team_keys),
        ]
        if part.strip()
    )
    await pool.execute(
        "INSERT INTO linear_sync_projects ("
        "project_id, name, description, slug_id, url, state, status_id, "
        "status_name, status_type, lead_user_id, lead_name, team_ids, team_keys, "
        "content_text, content_hash, source_created_at, source_updated_at, "
        "source_archived_at, source_completed_at, source_canceled_at, raw_payload, "
        "source_run_id, last_seen_at, last_error, updated_at"
        ") VALUES ("
        "$1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12::jsonb, $13::jsonb, "
        "$14, $15, $16, $17, $18, $19, $20, $21::jsonb, $22, NOW(), '', NOW()"
        ") ON CONFLICT (project_id) DO UPDATE SET "
        "name = EXCLUDED.name, "
        "description = EXCLUDED.description, "
        "slug_id = EXCLUDED.slug_id, "
        "url = EXCLUDED.url, "
        "state = EXCLUDED.state, "
        "status_id = EXCLUDED.status_id, "
        "status_name = EXCLUDED.status_name, "
        "status_type = EXCLUDED.status_type, "
        "lead_user_id = EXCLUDED.lead_user_id, "
        "lead_name = EXCLUDED.lead_name, "
        "team_ids = EXCLUDED.team_ids, "
        "team_keys = EXCLUDED.team_keys, "
        "content_text = EXCLUDED.content_text, "
        "content_hash = EXCLUDED.content_hash, "
        "source_created_at = EXCLUDED.source_created_at, "
        "source_updated_at = EXCLUDED.source_updated_at, "
        "source_archived_at = EXCLUDED.source_archived_at, "
        "source_completed_at = EXCLUDED.source_completed_at, "
        "source_canceled_at = EXCLUDED.source_canceled_at, "
        "raw_payload = EXCLUDED.raw_payload, "
        "source_run_id = EXCLUDED.source_run_id, "
        "last_seen_at = NOW(), "
        "last_error = '', "
        "updated_at = NOW()",
        _text_value(project.get("id")),
        _text_value(project.get("name")),
        _text_value(project.get("description")),
        _text_value(project.get("slugId")),
        _text_value(project.get("url")),
        _text_value(project.get("state")),
        _text_value(status.get("id")),
        _text_value(status.get("name")),
        _text_value(status.get("type")),
        _text_value(lead.get("id")),
        _text_value(lead.get("name") or lead.get("displayName")),
        canonical_json(team_ids),
        canonical_json(team_keys),
        content_text,
        _content_hash(content_text),
        _source_datetime(project, "createdAt"),
        _source_datetime(project, "updatedAt"),
        _source_datetime(project, "archivedAt"),
        _source_datetime(project, "completedAt"),
        _source_datetime(project, "canceledAt"),
        canonical_json(project),
        run_id,
    )


async def _upsert_issue(
    pool,
    *,
    issue: dict[str, Any],
    run_id: str,
) -> None:
    team = _json_object(issue.get("team"))
    project = _json_object(issue.get("project"))
    cycle = _json_object(issue.get("cycle"))
    state = _json_object(issue.get("state"))
    assignee = _json_object(issue.get("assignee"))
    creator = _json_object(issue.get("creator"))
    parent = _json_object(issue.get("parent"))
    content_text = "\n".join(
        part
        for part in [
            _text_value(issue.get("identifier")),
            _text_value(issue.get("title")),
            _text_value(issue.get("description")),
            _text_value(team.get("key")),
            _text_value(team.get("name")),
            _text_value(project.get("name")),
            _text_value(state.get("name")),
            _text_value(assignee.get("name") or assignee.get("displayName")),
        ]
        if part.strip()
    )
    await pool.execute(
        "INSERT INTO linear_sync_issues ("
        "issue_id, identifier, issue_number, title, description, url, priority, "
        "priority_label, estimate, due_date, team_id, team_key, team_name, "
        "project_id, project_name, cycle_id, cycle_name, state_id, state_name, "
        "state_type, assignee_user_id, assignee_name, creator_user_id, "
        "creator_name, parent_issue_id, parent_identifier, content_text, "
        "content_hash, source_created_at, source_updated_at, source_archived_at, "
        "source_started_at, source_completed_at, source_canceled_at, raw_payload, "
        "source_run_id, last_seen_at, last_error, updated_at"
        ") VALUES ("
        "$1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, "
        "$16, $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, "
        "$29, $30, $31, $32, $33, $34, $35::jsonb, $36, NOW(), '', NOW()"
        ") ON CONFLICT (issue_id) DO UPDATE SET "
        "identifier = EXCLUDED.identifier, "
        "issue_number = EXCLUDED.issue_number, "
        "title = EXCLUDED.title, "
        "description = EXCLUDED.description, "
        "url = EXCLUDED.url, "
        "priority = EXCLUDED.priority, "
        "priority_label = EXCLUDED.priority_label, "
        "estimate = EXCLUDED.estimate, "
        "due_date = EXCLUDED.due_date, "
        "team_id = EXCLUDED.team_id, "
        "team_key = EXCLUDED.team_key, "
        "team_name = EXCLUDED.team_name, "
        "project_id = EXCLUDED.project_id, "
        "project_name = EXCLUDED.project_name, "
        "cycle_id = EXCLUDED.cycle_id, "
        "cycle_name = EXCLUDED.cycle_name, "
        "state_id = EXCLUDED.state_id, "
        "state_name = EXCLUDED.state_name, "
        "state_type = EXCLUDED.state_type, "
        "assignee_user_id = EXCLUDED.assignee_user_id, "
        "assignee_name = EXCLUDED.assignee_name, "
        "creator_user_id = EXCLUDED.creator_user_id, "
        "creator_name = EXCLUDED.creator_name, "
        "parent_issue_id = EXCLUDED.parent_issue_id, "
        "parent_identifier = EXCLUDED.parent_identifier, "
        "content_text = EXCLUDED.content_text, "
        "content_hash = EXCLUDED.content_hash, "
        "source_created_at = EXCLUDED.source_created_at, "
        "source_updated_at = EXCLUDED.source_updated_at, "
        "source_archived_at = EXCLUDED.source_archived_at, "
        "source_started_at = EXCLUDED.source_started_at, "
        "source_completed_at = EXCLUDED.source_completed_at, "
        "source_canceled_at = EXCLUDED.source_canceled_at, "
        "raw_payload = EXCLUDED.raw_payload, "
        "source_run_id = EXCLUDED.source_run_id, "
        "last_seen_at = NOW(), "
        "last_error = '', "
        "updated_at = NOW()",
        _text_value(issue.get("id")),
        _text_value(issue.get("identifier")),
        _int_value(issue.get("number")),
        _text_value(issue.get("title")),
        _text_value(issue.get("description")),
        _text_value(issue.get("url")),
        _int_value(issue.get("priority")),
        _text_value(issue.get("priorityLabel")),
        _float_value(issue.get("estimate")),
        _parse_date(issue.get("dueDate")),
        _text_value(team.get("id")),
        _text_value(team.get("key")),
        _text_value(team.get("name")),
        _text_value(project.get("id")),
        _text_value(project.get("name")),
        _text_value(cycle.get("id")),
        _text_value(cycle.get("name")),
        _text_value(state.get("id")),
        _text_value(state.get("name")),
        _text_value(state.get("type")),
        _text_value(assignee.get("id")),
        _text_value(assignee.get("name") or assignee.get("displayName")),
        _text_value(creator.get("id")),
        _text_value(creator.get("name") or creator.get("displayName")),
        _text_value(parent.get("id")),
        _text_value(parent.get("identifier")),
        content_text,
        _content_hash(content_text),
        _source_datetime(issue, "createdAt"),
        _source_datetime(issue, "updatedAt"),
        _source_datetime(issue, "archivedAt"),
        _source_datetime(issue, "startedAt"),
        _source_datetime(issue, "completedAt"),
        _source_datetime(issue, "canceledAt"),
        canonical_json(issue),
        run_id,
    )


async def _upsert_comment(
    pool,
    *,
    comment: dict[str, Any],
    run_id: str,
) -> None:
    user = _json_object(comment.get("user"))
    user_name = _text_value(user.get("name") or user.get("displayName"))
    content_text = "\n".join(
        part
        for part in [
            _text_value(comment.get("body")),
            user_name,
            _text_value(comment.get("issueId")),
            _text_value(comment.get("projectId")),
        ]
        if part.strip()
    )
    await pool.execute(
        "INSERT INTO linear_sync_comments ("
        "comment_id, issue_id, project_id, parent_comment_id, user_id, user_name, "
        "body, url, content_text, content_hash, source_created_at, source_updated_at, "
        "source_archived_at, source_edited_at, source_resolved_at, raw_payload, "
        "source_run_id, last_seen_at, last_error, updated_at"
        ") VALUES ("
        "$1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, "
        "$16::jsonb, $17, NOW(), '', NOW()"
        ") ON CONFLICT (comment_id) DO UPDATE SET "
        "issue_id = EXCLUDED.issue_id, "
        "project_id = EXCLUDED.project_id, "
        "parent_comment_id = EXCLUDED.parent_comment_id, "
        "user_id = EXCLUDED.user_id, "
        "user_name = EXCLUDED.user_name, "
        "body = EXCLUDED.body, "
        "url = EXCLUDED.url, "
        "content_text = EXCLUDED.content_text, "
        "content_hash = EXCLUDED.content_hash, "
        "source_created_at = EXCLUDED.source_created_at, "
        "source_updated_at = EXCLUDED.source_updated_at, "
        "source_archived_at = EXCLUDED.source_archived_at, "
        "source_edited_at = EXCLUDED.source_edited_at, "
        "source_resolved_at = EXCLUDED.source_resolved_at, "
        "raw_payload = EXCLUDED.raw_payload, "
        "source_run_id = EXCLUDED.source_run_id, "
        "last_seen_at = NOW(), "
        "last_error = '', "
        "updated_at = NOW()",
        _text_value(comment.get("id")),
        _text_value(comment.get("issueId")),
        _text_value(comment.get("projectId")),
        _text_value(comment.get("parentId")),
        _text_value(user.get("id")),
        user_name,
        _text_value(comment.get("body")),
        _text_value(comment.get("url")),
        content_text,
        _content_hash(content_text),
        _source_datetime(comment, "createdAt"),
        _source_datetime(comment, "updatedAt"),
        _source_datetime(comment, "archivedAt"),
        _source_datetime(comment, "editedAt"),
        _source_datetime(comment, "resolvedAt"),
        canonical_json(comment),
        run_id,
    )


async def _sync_projects(
    *,
    client: LinearSyncClient,
    pool,
    page_size: int,
    updated_after: dt.datetime | None,
    include_archived: bool,
    run_id: str,
) -> tuple[int, int, dt.datetime | None]:
    seen = 0
    upserted = 0
    watermark: dt.datetime | None = None
    cursor: str | None = None
    while True:
        page = client.list_etl_projects(
            page_size=page_size,
            cursor=cursor,
            updated_after=updated_after,
            include_archived=include_archived,
        )
        projects = [
            project
            for project in page.get("nodes", []) or []
            if isinstance(project, dict) and project.get("id")
        ]
        seen += len(projects)
        record_etl_items_seen("linear", "workspace", "project", len(projects))
        for project in projects:
            await _upsert_project(pool, project=project, run_id=run_id)
            upserted += 1
            record_etl_items_upserted("linear", "workspace", "project", 1)
            updated_at = _source_datetime(project, "updatedAt")
            if updated_at and (watermark is None or updated_at > watermark):
                watermark = updated_at

        page_info = (
            page.get("pageInfo") if isinstance(page.get("pageInfo"), dict) else {}
        )
        cursor = page_info.get("endCursor")
        if not page_info.get("hasNextPage") or not cursor:
            break
    return seen, upserted, watermark


async def _sync_issues(
    *,
    client: LinearSyncClient,
    pool,
    page_size: int,
    updated_after: dt.datetime | None,
    include_archived: bool,
    run_id: str,
) -> tuple[int, int, dt.datetime | None]:
    seen = 0
    upserted = 0
    watermark: dt.datetime | None = None
    cursor: str | None = None
    while True:
        page = client.list_etl_issues(
            page_size=page_size,
            cursor=cursor,
            updated_after=updated_after,
            include_archived=include_archived,
        )
        issues = [
            issue
            for issue in page.get("nodes", []) or []
            if isinstance(issue, dict) and issue.get("id")
        ]
        seen += len(issues)
        record_etl_items_seen("linear", "workspace", "issue", len(issues))
        for issue in issues:
            await _upsert_issue(pool, issue=issue, run_id=run_id)
            upserted += 1
            record_etl_items_upserted("linear", "workspace", "issue", 1)
            updated_at = _source_datetime(issue, "updatedAt")
            if updated_at and (watermark is None or updated_at > watermark):
                watermark = updated_at

        page_info = (
            page.get("pageInfo") if isinstance(page.get("pageInfo"), dict) else {}
        )
        cursor = page_info.get("endCursor")
        if not page_info.get("hasNextPage") or not cursor:
            break
    return seen, upserted, watermark


async def _sync_comments(
    *,
    client: LinearSyncClient,
    pool,
    page_size: int,
    updated_after: dt.datetime | None,
    include_archived: bool,
    run_id: str,
) -> tuple[int, int, dt.datetime | None]:
    seen = 0
    upserted = 0
    watermark: dt.datetime | None = None
    cursor: str | None = None
    while True:
        page = client.list_etl_comments(
            page_size=page_size,
            cursor=cursor,
            updated_after=updated_after,
            include_archived=include_archived,
        )
        comments = [
            comment
            for comment in page.get("nodes", []) or []
            if isinstance(comment, dict) and comment.get("id")
        ]
        seen += len(comments)
        record_etl_items_seen("linear", "workspace", "comment", len(comments))
        for comment in comments:
            await _upsert_comment(pool, comment=comment, run_id=run_id)
            upserted += 1
            record_etl_items_upserted("linear", "workspace", "comment", 1)
            updated_at = _source_datetime(comment, "updatedAt")
            if updated_at and (watermark is None or updated_at > watermark):
                watermark = updated_at

        page_info = (
            page.get("pageInfo") if isinstance(page.get("pageInfo"), dict) else {}
        )
        cursor = page_info.get("endCursor")
        if not page_info.get("hasNextPage") or not cursor:
            break
    return seen, upserted, watermark


async def handler(inp: Input, ctx: WorkflowContext) -> dict[str, Any]:
    """Sync changed Linear projects, issues, and comments into raw sync tables."""
    if not env_flag_enabled("LINEAR_ETL_ENABLED", default=False):
        ctx.log("linear_sync_skipped_disabled")
        return {"status": "skipped", "reason": "linear_etl_disabled"}

    page_size = positive_int(inp.limit, DEFAULT_PAGE_SIZE)
    overlap_seconds = max(int(inp.watermark_overlap_seconds), 0)
    run_id = _workflow_run_id_to_sync_run_id(ctx.run_id)
    scopes_requested = [
        _scope_ref(PROJECTS_SCOPE),
        _scope_ref(ISSUES_SCOPE),
        _scope_ref(COMMENTS_SCOPE),
    ]

    await _record_run_start(
        ctx._pool,
        run_id=run_id,
        workflow_run_id=ctx.run_id,
        scopes_requested=scopes_requested,
        metadata={
            **inp.metadata,
            "page_size": page_size,
            "include_archived": inp.include_archived,
        },
    )

    client = _client()
    explicit_since = _parse_datetime(inp.since)
    synced: list[dict[str, str]] = []
    failed: list[dict[str, str]] = []
    counts = {
        "projects_seen": 0,
        "projects_upserted": 0,
        "issues_seen": 0,
        "issues_upserted": 0,
        "comments_seen": 0,
        "comments_upserted": 0,
    }

    for scope_id, sync_fn in [
        (PROJECTS_SCOPE, _sync_projects),
        (ISSUES_SCOPE, _sync_issues),
        (COMMENTS_SCOPE, _sync_comments),
    ]:
        checkpoint = await _load_checkpoint(ctx._pool, scope_id)
        watermark = explicit_since
        if watermark is None and checkpoint and checkpoint.get("watermark_time"):
            watermark = checkpoint["watermark_time"].astimezone(dt.timezone.utc)
        if watermark is not None:
            watermark = watermark - dt.timedelta(seconds=overlap_seconds)

        try:
            seen, upserted, successful_watermark = await sync_fn(
                client=client,
                pool=ctx._pool,
                page_size=page_size,
                updated_after=watermark,
                include_archived=inp.include_archived,
                run_id=run_id,
            )
            counts[f"{scope_id}_seen"] = seen
            counts[f"{scope_id}_upserted"] = upserted
            await _update_checkpoint_success(
                ctx._pool,
                scope_id=scope_id,
                watermark_time=successful_watermark,
                run_id=run_id,
            )
            synced.append(_scope_ref(scope_id))
            ctx.log(
                "linear_sync_scope_completed",
                scope_id=scope_id,
                items_seen=seen,
                items_upserted=upserted,
                watermark=_rfc3339(successful_watermark)
                if successful_watermark
                else "",
            )
        except Exception as exc:
            error = str(exc)
            failed.append(_scope_ref(scope_id, error))
            record_etl_items_failed(
                "linear", "workspace", "scope", _failure_reason(error)
            )
            await _update_checkpoint_failure(
                ctx._pool,
                scope_id=scope_id,
                run_id=run_id,
                error=error,
            )
            ctx.log("linear_sync_scope_failed", scope_id=scope_id, error=error)

    status = "completed"
    error_text = ""
    if failed and synced:
        status = "partial_failed"
        error_text = f"{len(failed)} Linear scope(s) failed"
    elif failed:
        status = "failed"
        error_text = f"{len(failed)} Linear scope(s) failed"

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
