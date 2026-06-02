from __future__ import annotations

import datetime as dt
import importlib
import json
from typing import Any

import pytest
import pytest_asyncio


class FakeCtx:
    def __init__(self, db_pool, run_id: str = "wfr-test-slack-context-documents"):
        self._pool = db_pool
        self.run_id = run_id
        self.logs: list[tuple[str, dict[str, Any]]] = []

    def log(self, msg: str, **kwargs: Any) -> None:
        self.logs.append((msg, kwargs))


@pytest_asyncio.fixture(autouse=True)
async def _clear_company_context_tables(db_pool):
    await db_pool.execute(
        "TRUNCATE TABLE company_context_documents, google_drive_sync_checkpoints, "
        "google_drive_sync_files, google_drive_sync_runs, google_calendar_sync_checkpoints, "
        "google_calendar_sync_events, google_calendar_sync_calendars, google_calendar_sync_runs, "
        "linear_sync_checkpoints, linear_sync_comments, linear_sync_issues, "
        "linear_sync_projects, linear_sync_runs, "
        "slack_sync_backfill_jobs, slack_sync_checkpoints, slack_sync_messages, "
        "slack_sync_runs, slack_sync_users, slack_sync_channels, workflow_runs CASCADE",
    )
    yield


@pytest.fixture(autouse=True)
def _enable_slack_etl(monkeypatch):
    monkeypatch.setenv("SLACK_ETL_ENABLED", "true")


def test_schedule_defaults_disabled_with_four_hour_interval(monkeypatch):
    monkeypatch.delenv("SLACK_ETL_ENABLED", raising=False)
    monkeypatch.delenv("COMPANY_CONTEXT_DOCUMENTS_ENABLED", raising=False)
    monkeypatch.delenv("COMPANY_CONTEXT_DOCUMENTS_INTERVAL_SECONDS", raising=False)

    from workflows import company_context_documents

    reloaded = importlib.reload(company_context_documents)

    assert reloaded.SCHEDULE == {
        "schedule_id": "company_context_documents",
        "interval_seconds": 14400,
        "enabled": False,
        "no_delivery": True,
    }


def test_schedule_respects_env_overrides(monkeypatch):
    monkeypatch.setenv("SLACK_ETL_ENABLED", "true")
    monkeypatch.setenv("COMPANY_CONTEXT_DOCUMENTS_ENABLED", "false")
    monkeypatch.setenv("COMPANY_CONTEXT_DOCUMENTS_INTERVAL_SECONDS", "300")

    from workflows import company_context_documents

    reloaded = importlib.reload(company_context_documents)

    assert reloaded.SCHEDULE["enabled"] is False
    assert reloaded.SCHEDULE["interval_seconds"] == 300


def test_schedule_enabled_when_google_drive_etl_enabled(monkeypatch):
    monkeypatch.delenv("SLACK_ETL_ENABLED", raising=False)
    monkeypatch.setenv("GOOGLE_DRIVE_ETL_ENABLED", "true")
    monkeypatch.delenv("GOOGLE_CALENDAR_ETL_ENABLED", raising=False)
    monkeypatch.delenv("COMPANY_CONTEXT_DOCUMENTS_ENABLED", raising=False)

    from workflows import company_context_documents

    reloaded = importlib.reload(company_context_documents)

    assert reloaded.SCHEDULE["enabled"] is True


def test_schedule_enabled_when_google_calendar_etl_enabled(monkeypatch):
    monkeypatch.delenv("SLACK_ETL_ENABLED", raising=False)
    monkeypatch.delenv("GOOGLE_DRIVE_ETL_ENABLED", raising=False)
    monkeypatch.setenv("GOOGLE_CALENDAR_ETL_ENABLED", "true")
    monkeypatch.delenv("LINEAR_ETL_ENABLED", raising=False)
    monkeypatch.delenv("COMPANY_CONTEXT_DOCUMENTS_ENABLED", raising=False)

    from workflows import company_context_documents

    reloaded = importlib.reload(company_context_documents)

    assert reloaded.SCHEDULE["enabled"] is True


def test_schedule_enabled_when_linear_etl_enabled(monkeypatch):
    monkeypatch.delenv("SLACK_ETL_ENABLED", raising=False)
    monkeypatch.delenv("GOOGLE_DRIVE_ETL_ENABLED", raising=False)
    monkeypatch.delenv("GOOGLE_CALENDAR_ETL_ENABLED", raising=False)
    monkeypatch.setenv("LINEAR_ETL_ENABLED", "true")
    monkeypatch.delenv("COMPANY_CONTEXT_DOCUMENTS_ENABLED", raising=False)

    from workflows import company_context_documents

    reloaded = importlib.reload(company_context_documents)

    assert reloaded.SCHEDULE["enabled"] is True


async def _seed_slack_basics(db_pool) -> None:
    await db_pool.execute(
        "INSERT INTO slack_sync_channels (channel_id, channel_name, is_syncable) "
        "VALUES ('C_PUBLIC', 'team-eng', TRUE), ('C_OTHER', 'general', TRUE)",
    )
    await db_pool.execute(
        "INSERT INTO slack_sync_users (user_id, user_name, real_name, display_name) "
        "VALUES "
        "('U1', 'alice', 'Alice Example', 'Alice'), "
        "('U2', 'bob', 'Bob Example', 'Bob'), "
        "('U3', 'carol', 'Carol Example', 'Carol')",
    )


async def _insert_message(
    db_pool,
    *,
    channel_id: str = "C_PUBLIC",
    message_ts: str,
    occurred_at: dt.datetime,
    updated_at: dt.datetime,
    user_id: str,
    text: str,
    thread_ts: str | None = None,
    parent_message_ts: str | None = None,
    reply_count: int = 0,
) -> None:
    await db_pool.execute(
        "INSERT INTO slack_sync_messages ("
        "channel_id, message_ts, occurred_at, thread_ts, parent_message_ts, "
        "is_thread_root, user_id, text, permalink, reply_count, raw_payload, "
        "updated_at, last_seen_at"
        ") VALUES ("
        "$1, $2, $3, $4, $5, $6, $7, $8, $9, $10, '{}'::jsonb, $11, $11"
        ")",
        channel_id,
        message_ts,
        occurred_at,
        thread_ts,
        parent_message_ts,
        bool(thread_ts and thread_ts == message_ts),
        user_id,
        text,
        f"https://slack.com/archives/{channel_id}/p{message_ts.replace('.', '')}",
        reply_count,
        updated_at,
    )


async def _seed_linear_run(db_pool) -> None:
    await db_pool.execute(
        "INSERT INTO linear_sync_runs (run_id, status) "
        "VALUES ('run-linear', 'completed')",
    )


async def _insert_linear_issue(
    db_pool,
    *,
    issue_id: str = "issue-1",
    identifier: str = "DATA-123",
    title: str = "Fix warehouse sync",
    description: str = "Warehouse rows are missing after backfill.",
    url: str = "https://linear.app/acme/issue/DATA-123/fix-warehouse-sync",
    team_id: str = "team-data",
    team_key: str = "DATA",
    team_name: str = "Data",
    project_id: str = "project-1",
    project_name: str = "Data Platform",
    state_id: str = "state-1",
    state_name: str = "In Progress",
    state_type: str = "started",
    assignee_user_id: str = "user-assignee",
    assignee_name: str = "Akshaan",
    creator_user_id: str = "user-creator",
    creator_name: str = "Jane",
    source_created_at: dt.datetime | None = None,
    source_updated_at: dt.datetime | None = None,
    updated_at: dt.datetime | None = None,
) -> None:
    assert source_created_at is not None
    assert source_updated_at is not None
    assert updated_at is not None
    await db_pool.execute(
        "INSERT INTO linear_sync_issues ("
        "issue_id, identifier, issue_number, title, description, url, priority, "
        "priority_label, estimate, due_date, team_id, team_key, team_name, "
        "project_id, project_name, state_id, state_name, state_type, "
        "assignee_user_id, assignee_name, creator_user_id, creator_name, "
        "content_text, content_hash, source_created_at, source_updated_at, "
        "raw_payload, source_run_id, updated_at"
        ") VALUES ("
        "$1, $2, 123, $3, $4, $5, 2, 'High', 3.5, DATE '2026-06-15', "
        "$6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, "
        "$18, 'issue-hash', $19, $20, $21::jsonb, 'run-linear', $22"
        ")",
        issue_id,
        identifier,
        title,
        description,
        url,
        team_id,
        team_key,
        team_name,
        project_id,
        project_name,
        state_id,
        state_name,
        state_type,
        assignee_user_id,
        assignee_name,
        creator_user_id,
        creator_name,
        f"{identifier} {title} {description}",
        source_created_at,
        source_updated_at,
        json.dumps({"id": issue_id}),
        updated_at,
    )


async def _insert_linear_comment(
    db_pool,
    *,
    comment_id: str,
    issue_id: str = "issue-1",
    user_id: str = "comment-user",
    user_name: str = "Sam",
    body: str = "Comment body",
    source_created_at: dt.datetime | None = None,
    source_updated_at: dt.datetime | None = None,
    updated_at: dt.datetime | None = None,
) -> None:
    assert source_created_at is not None
    assert source_updated_at is not None
    assert updated_at is not None
    await db_pool.execute(
        "INSERT INTO linear_sync_comments ("
        "comment_id, issue_id, project_id, user_id, user_name, body, url, "
        "content_text, content_hash, source_created_at, source_updated_at, "
        "raw_payload, source_run_id, updated_at"
        ") VALUES ("
        "$1, $2, 'project-1', $3, $4, $5, $6, $5, 'comment-hash', "
        "$7, $8, $9::jsonb, 'run-linear', $10"
        ")",
        comment_id,
        issue_id,
        user_id,
        user_name,
        body,
        f"https://linear.app/acme/comment/{comment_id}",
        source_created_at,
        source_updated_at,
        json.dumps({"id": comment_id}),
        updated_at,
    )


@pytest.mark.asyncio
async def test_projects_channel_day_and_thread_documents(db_pool):
    from workflows import company_context_documents

    await _seed_slack_basics(db_pool)
    base = dt.datetime(2026, 5, 6, 12, 0, tzinfo=dt.timezone.utc)
    updated = dt.datetime(2026, 5, 7, 10, 0, tzinfo=dt.timezone.utc)
    thread_ts = "1770000000.000000"
    messages = [
        ("1770000000.000000", "U1", "Root asks <@U2> about BM25", None, 4),
        ("1770000001.000000", "U2", "We should use hybrid search", thread_ts, 0),
        ("1770000002.000000", "U3", "Decision: keep docs in Postgres", thread_ts, 0),
        ("1770000003.000000", "U1", "Use #team-eng context", thread_ts, 0),
        ("1770000004.000000", "U2", "Ship pg_search separately", thread_ts, 0),
    ]
    for offset, (message_ts, user_id, text, parent_ts, reply_count) in enumerate(
        messages
    ):
        await _insert_message(
            db_pool,
            message_ts=message_ts,
            occurred_at=base + dt.timedelta(minutes=offset),
            updated_at=updated + dt.timedelta(seconds=offset),
            user_id=user_id,
            text=text,
            thread_ts=thread_ts,
            parent_message_ts=parent_ts,
            reply_count=reply_count,
        )

    result = await company_context_documents.handler(
        company_context_documents.Input(watermark_overlap_seconds=0),
        FakeCtx(db_pool),
    )

    assert result["status"] == "completed"
    assert result["changed_messages"] == 5
    assert result["documents_upserted"] == 2
    assert result["channel_day_documents"] == 1
    assert result["thread_candidates"] == 1

    rows = await db_pool.fetch(
        "SELECT document_id, source_type, title, body, url, author_name, metadata "
        "FROM company_context_documents ORDER BY source_type",
    )
    assert [row["source_type"] for row in rows] == ["slack_channel_day", "slack_thread"]

    channel_day = rows[0]
    assert channel_day["document_id"] == "slack:channel_day:C_PUBLIC:2026-05-06"
    assert channel_day["title"] == "#team-eng - 2026-05-06"
    assert "Alice Example - 2026-05-06 12:00:00 UTC - 4 replies" in channel_day["body"]
    assert "@Bob Example" in channel_day["body"]
    assert json.loads(channel_day["metadata"])["aggregation"] == "channel_day"

    thread = rows[1]
    assert thread["document_id"] == f"slack:thread:C_PUBLIC:{thread_ts}"
    assert thread["title"] == "Root asks @Bob Example about BM25"
    assert thread["author_name"] == "Alice Example"
    assert thread["url"] == "https://slack.com/archives/C_PUBLIC/p1770000000000000"
    assert "Participants: Alice Example, Bob Example, Carol Example" in thread["body"]
    assert json.loads(thread["metadata"])["reply_count"] == 4


@pytest.mark.asyncio
async def test_projects_documents_without_user_rows(db_pool):
    from workflows import company_context_documents

    await db_pool.execute(
        "INSERT INTO slack_sync_channels (channel_id, channel_name, is_syncable) "
        "VALUES ('C_PUBLIC', 'team-eng', TRUE)",
    )
    base = dt.datetime(2026, 5, 6, 12, 0, tzinfo=dt.timezone.utc)
    updated = dt.datetime(2026, 5, 7, 10, 0, tzinfo=dt.timezone.utc)
    thread_ts = "1770000000.000000"
    messages = [
        ("1770000000.000000", "UMISSING1", "Root mentions <@UMISSING2>", None, 4),
        ("1770000001.000000", "UMISSING2", "Reply one", thread_ts, 0),
        ("1770000002.000000", "UMISSING3", "Reply two", thread_ts, 0),
        ("1770000003.000000", "UMISSING1", "Reply three", thread_ts, 0),
        ("1770000004.000000", "UMISSING2", "Reply four", thread_ts, 0),
    ]
    for offset, (message_ts, user_id, text, parent_ts, reply_count) in enumerate(
        messages
    ):
        await _insert_message(
            db_pool,
            message_ts=message_ts,
            occurred_at=base + dt.timedelta(minutes=offset),
            updated_at=updated + dt.timedelta(seconds=offset),
            user_id=user_id,
            text=text,
            thread_ts=thread_ts,
            parent_message_ts=parent_ts,
            reply_count=reply_count,
        )

    result = await company_context_documents.handler(
        company_context_documents.Input(watermark_overlap_seconds=0),
        FakeCtx(db_pool),
    )

    assert result["status"] == "completed"
    assert result["documents_upserted"] == 2

    rows = await db_pool.fetch(
        "SELECT source_type, title, body, author_name, metadata "
        "FROM company_context_documents ORDER BY source_type",
    )
    assert [row["source_type"] for row in rows] == ["slack_channel_day", "slack_thread"]
    assert "@UMISSING2" in rows[0]["body"]
    assert "UMISSING1 - 2026-05-06 12:00:00 UTC - 4 replies" in rows[0]["body"]
    assert rows[1]["title"] == "Root mentions @UMISSING2"
    assert rows[1]["author_name"] == "UMISSING1"
    assert json.loads(rows[1]["metadata"])["participants"] == [
        "UMISSING1",
        "UMISSING2",
        "UMISSING3",
    ]


@pytest.mark.asyncio
async def test_uses_previous_successful_watermark_for_incremental_projection(db_pool):
    from workflows import company_context_documents

    await _seed_slack_basics(db_pool)
    watermark = dt.datetime(2026, 5, 7, 10, 0, tzinfo=dt.timezone.utc)
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, status, "
        "output_json, completed_at"
        ") VALUES ("
        "'wfr-previous', 'company_context_documents', 'test', 'hash', 'wfr-previous', "
        "'completed', $1::jsonb, $2"
        ")",
        json.dumps({"watermark": watermark.isoformat()}),
        watermark,
    )
    await _insert_message(
        db_pool,
        message_ts="1769900000.000000",
        occurred_at=dt.datetime(2026, 5, 1, 12, 0, tzinfo=dt.timezone.utc),
        updated_at=watermark - dt.timedelta(minutes=10),
        user_id="U1",
        text="Old message",
    )
    await _insert_message(
        db_pool,
        message_ts="1770000000.000000",
        occurred_at=dt.datetime(2026, 5, 6, 12, 0, tzinfo=dt.timezone.utc),
        updated_at=watermark + dt.timedelta(minutes=5),
        user_id="U2",
        text="New message",
    )

    result = await company_context_documents.handler(
        company_context_documents.Input(watermark_overlap_seconds=0),
        FakeCtx(db_pool, run_id="wfr-current"),
    )

    assert result["changed_messages"] == 1
    assert result["documents_upserted"] == 1
    assert (
        await db_pool.fetchval(
            "SELECT COUNT(*) FROM company_context_documents",
        )
        == 1
    )
    doc = await db_pool.fetchrow(
        "SELECT document_id, body FROM company_context_documents",
    )
    assert doc["document_id"] == "slack:channel_day:C_PUBLIC:2026-05-06"
    assert "New message" in doc["body"]
    assert "Old message" not in doc["body"]


@pytest.mark.asyncio
async def test_projects_google_drive_documents(db_pool, monkeypatch):
    monkeypatch.setenv("SLACK_ETL_ENABLED", "false")
    monkeypatch.setenv("GOOGLE_DRIVE_ETL_ENABLED", "true")

    from workflows import company_context_documents

    created_at = dt.datetime(2026, 5, 1, 12, 0, tzinfo=dt.timezone.utc)
    modified_at = dt.datetime(2026, 5, 2, 12, 0, tzinfo=dt.timezone.utc)
    updated_at = dt.datetime(2026, 5, 3, 12, 0, tzinfo=dt.timezone.utc)
    await db_pool.execute(
        "INSERT INTO google_drive_sync_files ("
        "file_id, name, mime_type, web_view_link, drive_id, parent_ids, owners, "
        "last_modifying_user, source_created_at, source_modified_at, text_content, "
        "text_hash, raw_payload, last_error, updated_at"
        ") VALUES ("
        "'doc-1', 'Investment memo', 'application/vnd.google-apps.document', "
        "'https://docs.google.com/document/d/doc-1/edit', 'shared-drive-1', "
        "$1::jsonb, $2::jsonb, $3::jsonb, $4, $5, $6, 'hash', $7::jsonb, '', $8"
        ")",
        json.dumps(["folder-1"]),
        json.dumps(
            [
                {
                    "emailAddress": "owner@example.com",
                    "displayName": "Owner Example",
                }
            ]
        ),
        json.dumps(
            {
                "emailAddress": "editor@example.com",
                "displayName": "Editor Example",
            }
        ),
        created_at,
        modified_at,
        "Doc body\nWith details",
        json.dumps({"id": "doc-1"}),
        updated_at,
    )

    result = await company_context_documents.handler(
        company_context_documents.Input(watermark_overlap_seconds=0),
        FakeCtx(db_pool),
    )

    assert result["status"] == "completed"
    assert result["changed_messages"] == 0
    assert result["changed_drive_files"] == 1
    assert result["drive_documents"] == 1
    assert result["documents_upserted"] == 1
    assert result["watermark"] == updated_at.isoformat()

    row = await db_pool.fetchrow(
        "SELECT document_id, source, source_type, title, body, url, author_id, "
        "author_name, occurred_at, source_updated_at, metadata "
        "FROM company_context_documents",
    )
    assert row["document_id"] == "google_drive:doc:doc-1"
    assert row["source"] == "google_drive"
    assert row["source_type"] == "google_doc"
    assert row["title"] == "Investment memo"
    assert "Doc body\nWith details" in row["body"]
    assert "Modified: 2026-05-02 12:00:00 UTC" in row["body"]
    assert row["url"] == "https://docs.google.com/document/d/doc-1/edit"
    assert row["author_id"] == "owner@example.com"
    assert row["author_name"] == "Owner Example"
    assert row["occurred_at"] == created_at
    assert row["source_updated_at"] == modified_at
    metadata = json.loads(row["metadata"])
    assert metadata["file_id"] == "doc-1"
    assert metadata["parent_ids"] == ["folder-1"]
    assert metadata["owners"][0]["emailAddress"] == "owner@example.com"


@pytest.mark.asyncio
async def test_projects_google_calendar_events(db_pool, monkeypatch):
    monkeypatch.setenv("SLACK_ETL_ENABLED", "false")
    monkeypatch.setenv("GOOGLE_DRIVE_ETL_ENABLED", "false")
    monkeypatch.setenv("GOOGLE_CALENDAR_ETL_ENABLED", "true")

    from workflows import company_context_documents

    created_at = dt.datetime(2026, 5, 1, 12, 0, tzinfo=dt.timezone.utc)
    updated_at = dt.datetime(2026, 5, 2, 12, 0, tzinfo=dt.timezone.utc)
    synced_at = dt.datetime(2026, 5, 3, 12, 0, tzinfo=dt.timezone.utc)
    starts_at = dt.datetime(2026, 5, 6, 16, 0, tzinfo=dt.timezone.utc)
    ends_at = dt.datetime(2026, 5, 6, 17, 0, tzinfo=dt.timezone.utc)
    await db_pool.execute(
        "INSERT INTO google_calendar_sync_runs (run_id, status) "
        "VALUES ('run-calendar', 'completed')",
    )
    await db_pool.execute(
        "INSERT INTO google_calendar_sync_calendars (calendar_id, summary) "
        "VALUES ('primary@example.com', 'Primary')",
    )
    await db_pool.execute(
        "INSERT INTO google_calendar_sync_events ("
        "calendar_id, event_id, i_cal_uid, status, summary, description, location, "
        "html_link, creator, organizer, attendees, start_payload, end_payload, "
        "start_at, end_at, source_created_at, source_updated_at, content_text, "
        "content_hash, raw_payload, updated_at"
        ") VALUES ("
        "'primary@example.com', 'event-1', 'ical-1', 'confirmed', "
        "'Partner diligence sync', 'Review roadmap and diligence notes', "
        "'Zoom', 'https://calendar.google.com/event?eid=event-1', "
        "$1::jsonb, $2::jsonb, $3::jsonb, $4::jsonb, $5::jsonb, "
        "$6, $7, $8, $9, 'Partner diligence sync Review roadmap', "
        "'hash-1', $10::jsonb, $11"
        ")",
        json.dumps({"email": "creator@example.com", "displayName": "Creator"}),
        json.dumps({"email": "organizer@example.com", "displayName": "Organizer"}),
        json.dumps(
            [
                {"email": "alice@example.com", "displayName": "Alice Example"},
                {"email": "bob@example.com"},
            ]
        ),
        json.dumps({"dateTime": starts_at.isoformat()}),
        json.dumps({"dateTime": ends_at.isoformat()}),
        starts_at,
        ends_at,
        created_at,
        updated_at,
        json.dumps({"id": "event-1"}),
        synced_at,
    )

    result = await company_context_documents.handler(
        company_context_documents.Input(watermark_overlap_seconds=0),
        FakeCtx(db_pool),
    )

    assert result["status"] == "completed"
    assert result["changed_calendar_events"] == 1
    assert result["calendar_event_documents"] == 1
    assert result["documents_upserted"] == 1
    assert result["watermark"] == synced_at.isoformat()

    row = await db_pool.fetchrow(
        "SELECT document_id, source, source_type, title, body, url, author_id, "
        "author_name, occurred_at, source_updated_at, metadata "
        "FROM company_context_documents",
    )
    assert row["document_id"] == "google_calendar:event:primary@example.com:event-1"
    assert row["source"] == "google_calendar"
    assert row["source_type"] == "calendar_event"
    assert row["title"] == "Partner diligence sync"
    assert "Calendar: Primary" in row["body"]
    assert "Attendees: Alice Example, bob@example.com" in row["body"]
    assert "Review roadmap and diligence notes" in row["body"]
    assert row["url"] == "https://calendar.google.com/event?eid=event-1"
    assert row["author_id"] == "organizer@example.com"
    assert row["author_name"] == "Organizer"
    assert row["occurred_at"] == starts_at
    assert row["source_updated_at"] == updated_at
    metadata = json.loads(row["metadata"])
    assert metadata["calendar_id"] == "primary@example.com"
    assert metadata["event_id"] == "event-1"
    assert metadata["attendees"][0]["displayName"] == "Alice Example"


@pytest.mark.asyncio
async def test_projects_linear_issue_documents_with_comments(db_pool, monkeypatch):
    monkeypatch.setenv("SLACK_ETL_ENABLED", "false")
    monkeypatch.setenv("GOOGLE_DRIVE_ETL_ENABLED", "false")
    monkeypatch.setenv("GOOGLE_CALENDAR_ETL_ENABLED", "false")
    monkeypatch.setenv("LINEAR_ETL_ENABLED", "true")

    from workflows import company_context_documents

    await _seed_linear_run(db_pool)
    created_at = dt.datetime(2026, 6, 1, 12, 0, tzinfo=dt.timezone.utc)
    issue_updated_at = dt.datetime(2026, 6, 1, 13, 0, tzinfo=dt.timezone.utc)
    comment_created_at = dt.datetime(2026, 6, 1, 14, 0, tzinfo=dt.timezone.utc)
    comment_updated_at = dt.datetime(2026, 6, 1, 14, 30, tzinfo=dt.timezone.utc)
    synced_at = dt.datetime(2026, 6, 1, 15, 0, tzinfo=dt.timezone.utc)
    await _insert_linear_issue(
        db_pool,
        source_created_at=created_at,
        source_updated_at=issue_updated_at,
        updated_at=synced_at,
    )
    await _insert_linear_comment(
        db_pool,
        comment_id="comment-1",
        user_name="Sam",
        body="I found the missing partition in the warehouse load.",
        source_created_at=comment_created_at,
        source_updated_at=comment_updated_at,
        updated_at=synced_at + dt.timedelta(minutes=1),
    )
    await _insert_linear_comment(
        db_pool,
        comment_id="comment-2",
        user_name="Mira",
        body="Let's backfill after the deploy finishes.",
        source_created_at=comment_created_at + dt.timedelta(minutes=15),
        source_updated_at=comment_updated_at + dt.timedelta(minutes=15),
        updated_at=synced_at + dt.timedelta(minutes=2),
    )

    result = await company_context_documents.handler(
        company_context_documents.Input(watermark_overlap_seconds=0),
        FakeCtx(db_pool),
    )

    assert result["status"] == "completed"
    assert result["changed_linear_issues"] == 1
    assert result["linear_issue_documents"] == 1
    assert result["documents_upserted"] == 1
    assert result["watermark"] == (synced_at + dt.timedelta(minutes=2)).isoformat()

    row = await db_pool.fetchrow(
        "SELECT document_id, source, source_type, source_document_id, title, body, "
        "url, author_id, author_name, occurred_at, source_updated_at, metadata "
        "FROM company_context_documents",
    )
    assert row["document_id"] == "linear:issue:issue-1"
    assert row["source"] == "linear"
    assert row["source_type"] == "linear_issue"
    assert row["source_document_id"] == "issue-1"
    assert row["title"] == "DATA-123: Fix warehouse sync"
    assert "Team: Data (DATA)" in row["body"]
    assert "Project: Data Platform" in row["body"]
    assert "Status: In Progress (started)" in row["body"]
    assert "Assignee: Akshaan" in row["body"]
    assert "Warehouse rows are missing after backfill." in row["body"]
    assert "Sam - 2026-06-01 14:00:00 UTC" in row["body"]
    assert "I found the missing partition" in row["body"]
    assert "Mira - 2026-06-01 14:15:00 UTC" in row["body"]
    assert row["url"] == "https://linear.app/acme/issue/DATA-123/fix-warehouse-sync"
    assert row["author_id"] == "user-creator"
    assert row["author_name"] == "Jane"
    assert row["occurred_at"] == created_at
    assert row["source_updated_at"] == comment_updated_at + dt.timedelta(minutes=15)
    metadata = json.loads(row["metadata"])
    assert metadata["identifier"] == "DATA-123"
    assert metadata["team_key"] == "DATA"
    assert metadata["project_name"] == "Data Platform"
    assert metadata["state_type"] == "started"
    assert metadata["comment_count"] == 2


@pytest.mark.asyncio
async def test_projects_linear_issue_document_when_comment_changes(
    db_pool,
    monkeypatch,
):
    monkeypatch.setenv("SLACK_ETL_ENABLED", "false")
    monkeypatch.setenv("GOOGLE_DRIVE_ETL_ENABLED", "false")
    monkeypatch.setenv("GOOGLE_CALENDAR_ETL_ENABLED", "false")
    monkeypatch.setenv("LINEAR_ETL_ENABLED", "true")

    from workflows import company_context_documents

    await _seed_linear_run(db_pool)
    watermark = dt.datetime(2026, 6, 1, 15, 0, tzinfo=dt.timezone.utc)
    await db_pool.execute(
        "INSERT INTO workflow_runs ("
        "run_id, workflow_name, workflow_version, request_hash, root_run_id, status, "
        "output_json, completed_at"
        ") VALUES ("
        "'wfr-previous', 'company_context_documents', 'test', 'hash', "
        "'wfr-previous', 'completed', $1::jsonb, $2"
        ")",
        json.dumps({"watermark": watermark.isoformat()}),
        watermark,
    )
    await _insert_linear_issue(
        db_pool,
        source_created_at=dt.datetime(2026, 6, 1, 12, 0, tzinfo=dt.timezone.utc),
        source_updated_at=dt.datetime(2026, 6, 1, 13, 0, tzinfo=dt.timezone.utc),
        updated_at=watermark - dt.timedelta(minutes=10),
    )
    await _insert_linear_comment(
        db_pool,
        comment_id="comment-new",
        user_name="Sam",
        body="The comment changed after the issue row was already projected.",
        source_created_at=watermark + dt.timedelta(minutes=5),
        source_updated_at=watermark + dt.timedelta(minutes=6),
        updated_at=watermark + dt.timedelta(minutes=7),
    )

    result = await company_context_documents.handler(
        company_context_documents.Input(watermark_overlap_seconds=0),
        FakeCtx(db_pool, run_id="wfr-current"),
    )

    assert result["changed_linear_issues"] == 1
    assert result["linear_issue_documents"] == 1
    assert result["documents_upserted"] == 1
    assert result["watermark"] == (watermark + dt.timedelta(minutes=7)).isoformat()

    row = await db_pool.fetchrow(
        "SELECT document_id, body, source_updated_at FROM company_context_documents",
    )
    assert row["document_id"] == "linear:issue:issue-1"
    assert (
        "The comment changed after the issue row was already projected." in row["body"]
    )
    assert row["source_updated_at"] == watermark + dt.timedelta(minutes=6)


@pytest.mark.asyncio
async def test_cancelled_google_calendar_events_delete_projected_documents(
    db_pool,
    monkeypatch,
):
    monkeypatch.setenv("SLACK_ETL_ENABLED", "false")
    monkeypatch.setenv("GOOGLE_DRIVE_ETL_ENABLED", "false")
    monkeypatch.setenv("GOOGLE_CALENDAR_ETL_ENABLED", "true")

    from workflows import company_context_documents

    updated_at = dt.datetime(2026, 5, 3, 12, 0, tzinfo=dt.timezone.utc)
    await db_pool.execute(
        "INSERT INTO google_calendar_sync_runs (run_id, status) "
        "VALUES ('run-calendar', 'completed')",
    )
    await db_pool.execute(
        "INSERT INTO google_calendar_sync_calendars (calendar_id, summary) "
        "VALUES ('primary@example.com', 'Primary')",
    )
    await db_pool.execute(
        "INSERT INTO google_calendar_sync_events ("
        "calendar_id, event_id, status, source_updated_at, updated_at"
        ") VALUES ('primary@example.com', 'event-1', 'cancelled', $1, $1)",
        updated_at,
    )
    await db_pool.execute(
        "INSERT INTO company_context_documents ("
        "document_id, source, source_type, source_document_id, title, body, "
        "content_hash"
        ") VALUES ("
        "'google_calendar:event:primary@example.com:event-1', 'google_calendar', "
        "'calendar_event', 'primary@example.com:event-1', 'Old event', 'Old body', "
        "'old-hash'"
        ")",
    )

    result = await company_context_documents.handler(
        company_context_documents.Input(watermark_overlap_seconds=0),
        FakeCtx(db_pool),
    )

    assert result["changed_calendar_events"] == 1
    assert result["calendar_event_documents"] == 1
    assert result["documents_deleted"] == 1
    assert (
        await db_pool.fetchval(
            "SELECT COUNT(*) FROM company_context_documents",
        )
        == 0
    )
