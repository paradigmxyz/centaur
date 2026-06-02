from __future__ import annotations

import datetime as dt
import importlib
import json
from typing import Any

import pytest
import pytest_asyncio


class FakeCtx:
    def __init__(self, db_pool, run_id: str = "wfr-test-linear-sync"):
        self._pool = db_pool
        self.run_id = run_id
        self.logs: list[tuple[str, dict[str, Any]]] = []

    def log(self, msg: str, **kwargs: Any) -> None:
        self.logs.append((msg, kwargs))


class FakeLinearClient:
    def __init__(
        self,
        *,
        project_pages: list[dict[str, Any]],
        issue_pages: list[dict[str, Any]],
        comment_pages: list[dict[str, Any]],
    ) -> None:
        self.project_pages = list(project_pages)
        self.issue_pages = list(issue_pages)
        self.comment_pages = list(comment_pages)
        self.project_calls: list[dict[str, Any]] = []
        self.issue_calls: list[dict[str, Any]] = []
        self.comment_calls: list[dict[str, Any]] = []

    def list_etl_projects(
        self,
        *,
        page_size: int,
        cursor: str | None = None,
        updated_after: dt.datetime | str | None = None,
        include_archived: bool = True,
    ) -> dict[str, Any]:
        self.project_calls.append(
            {
                "page_size": page_size,
                "cursor": cursor,
                "updated_after": updated_after,
                "include_archived": include_archived,
            }
        )
        if self.project_pages:
            return self.project_pages.pop(0)
        return {"nodes": [], "pageInfo": {"hasNextPage": False, "endCursor": None}}

    def list_etl_issues(
        self,
        *,
        page_size: int,
        cursor: str | None = None,
        updated_after: dt.datetime | str | None = None,
        include_archived: bool = True,
    ) -> dict[str, Any]:
        self.issue_calls.append(
            {
                "page_size": page_size,
                "cursor": cursor,
                "updated_after": updated_after,
                "include_archived": include_archived,
            }
        )
        if self.issue_pages:
            return self.issue_pages.pop(0)
        return {"nodes": [], "pageInfo": {"hasNextPage": False, "endCursor": None}}

    def list_etl_comments(
        self,
        *,
        page_size: int,
        cursor: str | None = None,
        updated_after: dt.datetime | str | None = None,
        include_archived: bool = True,
    ) -> dict[str, Any]:
        self.comment_calls.append(
            {
                "page_size": page_size,
                "cursor": cursor,
                "updated_after": updated_after,
                "include_archived": include_archived,
            }
        )
        if self.comment_pages:
            return self.comment_pages.pop(0)
        return {"nodes": [], "pageInfo": {"hasNextPage": False, "endCursor": None}}


@pytest_asyncio.fixture(autouse=True)
async def _clear_linear_sync_tables(db_pool):
    await db_pool.execute(
        "TRUNCATE TABLE linear_sync_checkpoints, linear_sync_comments, linear_sync_issues, "
        "linear_sync_projects, linear_sync_runs, workflow_runs CASCADE",
    )
    yield


def test_schedule_defaults_disabled_with_four_hour_interval(monkeypatch):
    monkeypatch.delenv("LINEAR_ETL_ENABLED", raising=False)
    monkeypatch.delenv("LINEAR_SYNC_INTERVAL_SECONDS", raising=False)

    from workflows import linear_sync

    reloaded = importlib.reload(linear_sync)

    assert reloaded.SCHEDULE == {
        "schedule_id": "linear_sync",
        "interval_seconds": 14400,
        "enabled": False,
        "no_delivery": True,
    }


def test_schedule_respects_env_overrides(monkeypatch):
    monkeypatch.setenv("LINEAR_ETL_ENABLED", "true")
    monkeypatch.setenv("LINEAR_SYNC_INTERVAL_SECONDS", "600")

    from workflows import linear_sync

    reloaded = importlib.reload(linear_sync)

    assert reloaded.SCHEDULE["enabled"] is True
    assert reloaded.SCHEDULE["interval_seconds"] == 600


@pytest.mark.asyncio
async def test_syncs_linear_projects_issues_and_comments_into_raw_tables(
    db_pool, monkeypatch
):
    from workflows import linear_sync

    monkeypatch.setenv("LINEAR_ETL_ENABLED", "true")
    fake = FakeLinearClient(
        project_pages=[
            {
                "nodes": [
                    {
                        "id": "project-1",
                        "name": "Launch plan",
                        "description": "Ship the thing",
                        "slugId": "LAUNCH",
                        "url": "https://linear.app/acme/project/launch",
                        "state": "started",
                        "status": {
                            "id": "status-1",
                            "name": "On track",
                            "type": "started",
                        },
                        "lead": {
                            "id": "user-1",
                            "name": "Ada Lovelace",
                            "displayName": "Ada",
                        },
                        "teams": {
                            "nodes": [
                                {"id": "team-1", "name": "Engineering", "key": "ENG"}
                            ]
                        },
                        "createdAt": "2026-05-01T10:00:00Z",
                        "updatedAt": "2026-05-02T10:00:00Z",
                        "completedAt": None,
                        "canceledAt": None,
                    }
                ],
                "pageInfo": {"hasNextPage": False, "endCursor": None},
            }
        ],
        issue_pages=[
            {
                "nodes": [
                    {
                        "id": "issue-1",
                        "identifier": "ENG-101",
                        "number": 101,
                        "title": "Wire Linear sync",
                        "description": "Store projects and issues",
                        "url": "https://linear.app/acme/issue/ENG-101",
                        "priority": 2,
                        "priorityLabel": "High",
                        "estimate": 3,
                        "dueDate": "2026-05-10",
                        "team": {"id": "team-1", "name": "Engineering", "key": "ENG"},
                        "project": {"id": "project-1", "name": "Launch plan"},
                        "cycle": {"id": "cycle-1", "name": "Cycle 1", "number": 1},
                        "state": {
                            "id": "state-1",
                            "name": "In Progress",
                            "type": "started",
                        },
                        "assignee": {"id": "user-1", "name": "Ada Lovelace"},
                        "creator": {"id": "user-2", "name": "Grace Hopper"},
                        "parent": {"id": "issue-parent", "identifier": "ENG-1"},
                        "createdAt": "2026-05-01T11:00:00Z",
                        "updatedAt": "2026-05-03T12:00:00Z",
                        "startedAt": "2026-05-02T11:00:00Z",
                        "completedAt": None,
                        "canceledAt": None,
                    }
                ],
                "pageInfo": {"hasNextPage": False, "endCursor": None},
            }
        ],
        comment_pages=[
            {
                "nodes": [
                    {
                        "id": "comment-1",
                        "body": "Looks good for the launch plan.",
                        "url": "https://linear.app/acme/comment/comment-1",
                        "issueId": "issue-1",
                        "projectId": "project-1",
                        "parentId": None,
                        "user": {"id": "user-2", "name": "Grace Hopper"},
                        "createdAt": "2026-05-03T13:00:00Z",
                        "updatedAt": "2026-05-03T13:30:00Z",
                        "archivedAt": None,
                        "editedAt": "2026-05-03T13:20:00Z",
                        "resolvedAt": None,
                    }
                ],
                "pageInfo": {"hasNextPage": False, "endCursor": None},
            }
        ],
    )
    monkeypatch.setattr(linear_sync, "_client", lambda: fake)

    result = await linear_sync.handler(
        linear_sync.Input(limit=25, watermark_overlap_seconds=0),
        FakeCtx(db_pool),
    )

    assert result["status"] == "completed"
    assert result["projects_seen"] == 1
    assert result["projects_upserted"] == 1
    assert result["issues_seen"] == 1
    assert result["issues_upserted"] == 1
    assert result["comments_seen"] == 1
    assert result["comments_upserted"] == 1
    assert fake.project_calls[0]["page_size"] == 25
    assert fake.issue_calls[0]["include_archived"] is True
    assert fake.comment_calls[0]["include_archived"] is True

    project = await db_pool.fetchrow(
        "SELECT project_id, name, slug_id, status_name, lead_name, team_ids, "
        "source_updated_at, raw_payload FROM linear_sync_projects",
    )
    assert project["project_id"] == "project-1"
    assert project["name"] == "Launch plan"
    assert project["slug_id"] == "LAUNCH"
    assert project["status_name"] == "On track"
    assert project["lead_name"] == "Ada Lovelace"
    assert json.loads(project["team_ids"]) == ["team-1"]
    assert project["source_updated_at"] == dt.datetime(
        2026, 5, 2, 10, 0, tzinfo=dt.timezone.utc
    )
    assert json.loads(project["raw_payload"])["slugId"] == "LAUNCH"

    issue = await db_pool.fetchrow(
        "SELECT issue_id, identifier, issue_number, title, priority_label, "
        "estimate, due_date, team_key, project_id, state_name, assignee_name, "
        "parent_identifier, source_updated_at, raw_payload FROM linear_sync_issues",
    )
    assert issue["issue_id"] == "issue-1"
    assert issue["identifier"] == "ENG-101"
    assert issue["issue_number"] == 101
    assert issue["title"] == "Wire Linear sync"
    assert issue["priority_label"] == "High"
    assert issue["estimate"] == 3
    assert issue["due_date"] == dt.date(2026, 5, 10)
    assert issue["team_key"] == "ENG"
    assert issue["project_id"] == "project-1"
    assert issue["state_name"] == "In Progress"
    assert issue["assignee_name"] == "Ada Lovelace"
    assert issue["parent_identifier"] == "ENG-1"
    assert issue["source_updated_at"] == dt.datetime(
        2026, 5, 3, 12, 0, tzinfo=dt.timezone.utc
    )
    assert json.loads(issue["raw_payload"])["identifier"] == "ENG-101"

    comment = await db_pool.fetchrow(
        "SELECT comment_id, issue_id, project_id, user_id, user_name, body, "
        "source_updated_at, source_edited_at, raw_payload FROM linear_sync_comments",
    )
    assert comment["comment_id"] == "comment-1"
    assert comment["issue_id"] == "issue-1"
    assert comment["project_id"] == "project-1"
    assert comment["user_id"] == "user-2"
    assert comment["user_name"] == "Grace Hopper"
    assert comment["body"] == "Looks good for the launch plan."
    assert comment["source_updated_at"] == dt.datetime(
        2026, 5, 3, 13, 30, tzinfo=dt.timezone.utc
    )
    assert comment["source_edited_at"] == dt.datetime(
        2026, 5, 3, 13, 20, tzinfo=dt.timezone.utc
    )
    assert json.loads(comment["raw_payload"])["issueId"] == "issue-1"

    checkpoints = await db_pool.fetch(
        "SELECT scope_id, watermark_time FROM linear_sync_checkpoints ORDER BY scope_id",
    )
    assert [(row["scope_id"], row["watermark_time"]) for row in checkpoints] == [
        ("comments", dt.datetime(2026, 5, 3, 13, 30, tzinfo=dt.timezone.utc)),
        ("issues", dt.datetime(2026, 5, 3, 12, 0, tzinfo=dt.timezone.utc)),
        ("projects", dt.datetime(2026, 5, 2, 10, 0, tzinfo=dt.timezone.utc)),
    ]


@pytest.mark.asyncio
async def test_incremental_sync_uses_checkpoint_overlap(db_pool, monkeypatch):
    from workflows import linear_sync

    monkeypatch.setenv("LINEAR_ETL_ENABLED", "true")
    await db_pool.execute(
        "INSERT INTO linear_sync_checkpoints (scope_id, watermark_time) "
        "VALUES ('projects', $1), ('issues', $1), ('comments', $1)",
        dt.datetime(2026, 5, 10, 12, 0, tzinfo=dt.timezone.utc),
    )
    fake = FakeLinearClient(
        project_pages=[{"nodes": [], "pageInfo": {"hasNextPage": False}}],
        issue_pages=[{"nodes": [], "pageInfo": {"hasNextPage": False}}],
        comment_pages=[{"nodes": [], "pageInfo": {"hasNextPage": False}}],
    )
    monkeypatch.setattr(linear_sync, "_client", lambda: fake)

    result = await linear_sync.handler(
        linear_sync.Input(watermark_overlap_seconds=300),
        FakeCtx(db_pool),
    )

    assert result["status"] == "completed"
    expected = dt.datetime(2026, 5, 10, 11, 55, tzinfo=dt.timezone.utc)
    assert fake.project_calls[0]["updated_after"] == expected
    assert fake.issue_calls[0]["updated_after"] == expected
    assert fake.comment_calls[0]["updated_after"] == expected
