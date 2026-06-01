from __future__ import annotations

import importlib
import re
import datetime as dt
import json
from typing import Any
from unittest.mock import AsyncMock, patch

import pytest
import pytest_asyncio


class FakeCtx:
    def __init__(self, db_pool, run_id: str = "wfr-test-slack-sync"):
        self._pool = db_pool
        self.run_id = run_id
        self.logs: list[tuple[str, dict[str, Any]]] = []

    def log(self, msg: str, **kwargs: Any) -> None:
        self.logs.append((msg, kwargs))


class FakeSlackClient:
    def __init__(
        self,
        *,
        channels: list[dict[str, Any]] | None = None,
        users: list[dict[str, Any]] | None = None,
        messages: list[dict[str, Any]] | None = None,
        history_pages: dict[str, list[dict[str, Any]]] | None = None,
        replies: dict[str, list[dict[str, Any]]] | None = None,
        reply_pages: dict[str, list[dict[str, Any]]] | None = None,
        sync_state: dict[str, Any] | None = None,
    ) -> None:
        self.channels = channels or []
        self.users = users or []
        self.messages = messages or []
        self.history_pages = history_pages or {}
        self.replies = replies or {}
        self.reply_pages = reply_pages or {}
        self.sync_state = sync_state or {
            "cursor": None,
            "watermark": "3000000.000000",
            "oldest": None,
            "latest": None,
        }
        self.history_calls: list[dict[str, Any]] = []
        self.reply_calls: list[dict[str, Any]] = []
        self.list_bot_channels_calls = 0
        self.list_etl_channels_calls = 0
        self.list_users_calls = 0
        self.list_etl_users_calls = 0

    def list_bot_channels(
        self, limit: int = 200, force_refresh: bool = False
    ) -> list[dict]:
        self.list_bot_channels_calls += 1
        return [
            ch
            for ch in self.channels
            if not ch.get("is_private") and ch.get("is_syncable", True)
        ][:limit]

    def _etl_access_mode(self) -> str:
        return "user_token"

    def _list_etl_channels(
        self, limit: int = 200, force_refresh: bool = False
    ) -> list[dict]:
        self.list_etl_channels_calls += 1
        return [ch for ch in self.channels if not ch.get("is_private")][:limit]

    def list_users(self, limit: int = 200) -> list[dict]:
        self.list_users_calls += 1
        return self.users[:limit]

    def _list_etl_users(self, limit: int = 200) -> list[dict]:
        self.list_etl_users_calls += 1
        return self.users[:limit]

    def _sync_etl_channel_history(
        self,
        channel: str,
        state: dict[str, Any] | None = None,
        limit: int = 200,
        lookback_days: int = 30,
        oldest: str | int | float | None = None,
        latest: str | int | float | None = None,
    ) -> dict[str, Any]:
        self.history_calls.append(
            {
                "channel": channel,
                "state": state,
                "limit": limit,
                "lookback_days": lookback_days,
                "oldest": oldest,
                "latest": latest,
            }
        )
        if channel in self.history_pages:
            page = self.history_pages[channel].pop(0)
            messages = page.get("messages", [])
            return {
                "channel": channel,
                "channel_id": channel,
                "messages": messages,
                "count": len(messages),
                "has_more": bool(page.get("has_more")),
                "next_cursor": page.get("next_cursor"),
                "sync_state": page.get("sync_state", self.sync_state),
            }
        return {
            "channel": channel,
            "channel_id": channel,
            "messages": self.messages,
            "count": len(self.messages),
            "has_more": False,
            "next_cursor": None,
            "sync_state": self.sync_state,
        }

    def _get_etl_thread_replies_page(
        self,
        channel: str,
        thread_ts: str,
        limit: int = 200,
        cursor: str | None = None,
        oldest: str | int | float | None = None,
        latest: str | int | float | None = None,
        inclusive: bool = True,
    ) -> dict[str, Any]:
        self.reply_calls.append(
            {
                "channel": channel,
                "thread_ts": thread_ts,
                "limit": limit,
                "cursor": cursor,
                "oldest": oldest,
                "latest": latest,
                "inclusive": inclusive,
            }
        )
        if thread_ts in self.reply_pages:
            page = self.reply_pages[thread_ts].pop(0)
            messages = page.get("messages", [])
            return {
                "channel_id": channel,
                "thread_ts": thread_ts,
                "messages": messages,
                "count": len(messages),
                "has_more": bool(page.get("has_more")),
                "next_cursor": page.get("next_cursor"),
            }

        messages = self.replies.get(thread_ts, [])
        return {
            "channel_id": channel,
            "thread_ts": thread_ts,
            "messages": messages,
            "count": len(messages),
            "has_more": False,
            "next_cursor": None,
        }


@pytest_asyncio.fixture(autouse=True)
async def _clear_slack_sync_tables(db_pool, monkeypatch):
    monkeypatch.setenv("SLACK_ETL_ENABLED", "true")
    monkeypatch.delenv("SLACK_ETL_EXCLUDED_CHANNEL_PATTERNS", raising=False)
    await db_pool.execute(
        "TRUNCATE TABLE company_context_documents, google_drive_sync_checkpoints, "
        "google_drive_sync_files, google_drive_sync_runs, google_calendar_sync_checkpoints, "
        "google_calendar_sync_events, google_calendar_sync_calendars, google_calendar_sync_runs, "
        "slack_sync_backfill_jobs, slack_sync_checkpoints, slack_sync_messages, "
        "slack_sync_runs, slack_sync_users, slack_sync_channels CASCADE",
    )
    yield


def _public_channel() -> dict[str, Any]:
    return {
        "id": "C_PUBLIC",
        "name": "ai-agent",
        "is_private": False,
        "is_archived": False,
        "is_syncable": True,
        "topic": "Agents",
        "purpose": "Testing",
        "member_count": 10,
    }


def _private_channel() -> dict[str, Any]:
    return {
        "id": "G_PRIVATE",
        "name": "private-room",
        "is_private": True,
        "is_archived": False,
        "is_syncable": True,
    }


def _other_public_channel() -> dict[str, Any]:
    return {
        "id": "C_OTHER",
        "name": "other-channel",
        "is_private": False,
        "is_archived": False,
        "is_syncable": False,
        "topic": "Other",
        "purpose": "Also public",
        "member_count": 5,
    }


def _alert_channel() -> dict[str, Any]:
    return {
        "id": "C_ALERTS",
        "name": "eng-cyclops-alerts",
        "is_private": False,
        "is_archived": False,
        "is_syncable": True,
        "topic": "Alerts",
        "purpose": "Monitoring noise",
        "member_count": 3,
    }


def _root_message() -> dict[str, Any]:
    return {
        "channel_id": "C_PUBLIC",
        "timestamp": "3000000.000000",
        "thread_ts": "3000000.000000",
        "user_id": "U1",
        "user": "alice",
        "text": "root",
        "permalink": "https://slack.com/archives/C_PUBLIC/p3000000000000",
        "reply_count": 1,
        "reply_users": ["U2"],
        "latest_reply": "3000001.000000",
        "type": "message",
    }


def _reply_message() -> dict[str, Any]:
    return {
        "channel_id": "C_PUBLIC",
        "timestamp": "3000001.000000",
        "thread_ts": "3000000.000000",
        "user_id": "U2",
        "user": "bob",
        "text": "reply",
        "permalink": "https://slack.com/archives/C_PUBLIC/p3000001000000",
        "reply_count": 0,
        "type": "message",
    }


def test_schedule_defaults_disabled(monkeypatch):
    monkeypatch.delenv("SLACK_ETL_ENABLED", raising=False)
    monkeypatch.delenv("SLACK_SYNC_INTERVAL_SECONDS", raising=False)

    from workflows import slack_sync

    reloaded = importlib.reload(slack_sync)

    assert reloaded.SCHEDULE == {
        "schedule_id": "slack_sync",
        "interval_seconds": 3600,
        "enabled": False,
        "no_delivery": True,
    }


def test_schedule_respects_env_overrides(monkeypatch):
    monkeypatch.setenv("SLACK_ETL_ENABLED", "false")
    monkeypatch.setenv("SLACK_SYNC_INTERVAL_SECONDS", "900")

    from workflows import slack_sync

    reloaded = importlib.reload(slack_sync)

    assert reloaded.SCHEDULE["enabled"] is False
    assert reloaded.SCHEDULE["interval_seconds"] == 900


def test_backfill_schedule_defaults_disabled(monkeypatch):
    monkeypatch.delenv("SLACK_ETL_ENABLED", raising=False)
    monkeypatch.delenv("SLACK_BACKFILL_ENABLED", raising=False)
    monkeypatch.delenv("SLACK_BACKFILL_INTERVAL_SECONDS", raising=False)
    monkeypatch.delenv("SLACK_BACKFILL_CHANNEL_BATCH_LIMIT", raising=False)

    from workflows import slack_backfill

    reloaded = importlib.reload(slack_backfill)

    assert reloaded.DEFAULT_CHANNEL_BATCH_LIMIT == 50
    assert reloaded.DEFAULT_CHANNEL_PAGES_PER_JOB == 5
    assert reloaded.SCHEDULE == {
        "schedule_id": "slack_backfill",
        "interval_seconds": 60,
        "enabled": False,
        "no_delivery": True,
    }


def test_backfill_schedule_respects_env_overrides(monkeypatch):
    monkeypatch.setenv("SLACK_ETL_ENABLED", "true")
    monkeypatch.setenv("SLACK_BACKFILL_ENABLED", "false")
    monkeypatch.setenv("SLACK_BACKFILL_INTERVAL_SECONDS", "120")
    monkeypatch.setenv("SLACK_BACKFILL_CHANNEL_BATCH_LIMIT", "12")
    monkeypatch.setenv("SLACK_BACKFILL_CHANNEL_PAGES_PER_JOB", "3")

    from workflows import slack_backfill

    reloaded = importlib.reload(slack_backfill)

    assert reloaded.SCHEDULE["enabled"] is False
    assert reloaded.SCHEDULE["interval_seconds"] == 120
    assert reloaded.DEFAULT_CHANNEL_BATCH_LIMIT == 12
    assert reloaded.DEFAULT_CHANNEL_PAGES_PER_JOB == 3


@pytest.mark.asyncio
async def test_slack_etl_disabled_by_default_noops_without_run_row(
    db_pool, monkeypatch
):
    from workflows import slack_sync

    await db_pool.execute(
        "INSERT INTO slack_sync_channels (channel_id, channel_name, is_syncable) "
        "VALUES ('C_OLD', 'old-channel', TRUE)",
    )
    fake = FakeSlackClient(channels=[_public_channel()])
    ctx = FakeCtx(db_pool)
    monkeypatch.delenv("SLACK_ETL_ENABLED", raising=False)

    with patch.object(slack_sync, "_client", return_value=fake):
        result = await slack_sync.handler(slack_sync.Input(), ctx)

    assert result["status"] == "skipped"
    assert result["reason"] == "slack_etl_disabled"
    assert fake.list_etl_channels_calls == 0
    assert fake.list_bot_channels_calls == 0
    assert fake.list_users_calls == 0
    assert fake.list_etl_users_calls == 0
    assert await db_pool.fetchval("SELECT COUNT(*) FROM slack_sync_runs") == 0
    assert (
        await db_pool.fetchval(
            "SELECT is_syncable FROM slack_sync_channels WHERE channel_id = 'C_OLD'",
        )
        is True
    )


@pytest.mark.asyncio
async def test_no_public_channels_noops_without_run_row(db_pool):
    from workflows import slack_sync

    await db_pool.execute(
        "INSERT INTO slack_sync_channels (channel_id, channel_name, is_syncable) "
        "VALUES ('C_OLD', 'old-channel', TRUE)",
    )
    fake = FakeSlackClient(channels=[])
    ctx = FakeCtx(db_pool)

    with patch.object(slack_sync, "_client", return_value=fake):
        result = await slack_sync.handler(slack_sync.Input(), ctx)

    assert result["status"] == "skipped"
    assert result["reason"] == "no_public_channels"
    assert fake.list_etl_channels_calls == 1
    assert fake.list_bot_channels_calls == 0
    assert await db_pool.fetchval("SELECT COUNT(*) FROM slack_sync_runs") == 0
    assert (
        await db_pool.fetchval(
            "SELECT is_syncable FROM slack_sync_channels WHERE channel_id = 'C_OLD'",
        )
        is False
    )


@pytest.mark.asyncio
async def test_syncs_all_public_channels_by_default(db_pool):
    from workflows import slack_sync

    fake = FakeSlackClient(
        channels=[_public_channel(), _other_public_channel(), _private_channel()]
    )
    ctx = FakeCtx(db_pool)

    with patch.object(slack_sync, "_client", return_value=fake):
        result = await slack_sync.handler(slack_sync.Input(), ctx)

    assert result["status"] == "completed"
    assert result["channels_synced"] == 2
    assert [call["channel"] for call in fake.history_calls] == ["C_PUBLIC", "C_OTHER"]
    assert [call["limit"] for call in fake.history_calls] == [100, 100]
    assert (
        await db_pool.fetchval(
            "SELECT COUNT(*) FROM slack_sync_channels WHERE channel_id = 'C_OTHER'",
        )
        == 1
    )
    assert (
        await db_pool.fetchval(
            "SELECT COUNT(*) FROM slack_sync_channels WHERE channel_id = 'G_PRIVATE'",
        )
        == 0
    )

    run = await db_pool.fetchrow(
        "SELECT channels_requested, metadata FROM slack_sync_runs WHERE run_id = $1",
        result["run_id"],
    )
    assert run is not None
    assert json.loads(run["channels_requested"]) == [
        {"channel_id": "C_PUBLIC", "channel_name": "ai-agent"},
        {"channel_id": "C_OTHER", "channel_name": "other-channel"},
    ]
    assert json.loads(run["metadata"])["slack_access_mode"] == "user_token"


@pytest.mark.asyncio
async def test_excludes_channels_matching_configured_patterns(db_pool, monkeypatch):
    from workflows import slack_sync

    fake = FakeSlackClient(channels=[_public_channel(), _alert_channel()])
    ctx = FakeCtx(db_pool)
    monkeypatch.setenv(
        "SLACK_ETL_EXCLUDED_CHANNEL_PATTERNS", "#eng-*-alerts, *-monitor-*"
    )

    with patch.object(slack_sync, "_client", return_value=fake):
        result = await slack_sync.handler(slack_sync.Input(), ctx)

    assert result["status"] == "completed"
    assert result["channels_synced"] == 1
    assert result["channels_skipped"] == 1
    assert [call["channel"] for call in fake.history_calls] == ["C_PUBLIC"]
    assert (
        await db_pool.fetchval(
            "SELECT COUNT(*) FROM slack_sync_channels WHERE channel_id = 'C_ALERTS'",
        )
        == 0
    )

    run = await db_pool.fetchrow(
        "SELECT channels_requested, channels_skipped, metadata FROM slack_sync_runs WHERE run_id = $1",
        result["run_id"],
    )
    assert json.loads(run["channels_requested"]) == [
        {"channel_id": "C_PUBLIC", "channel_name": "ai-agent"},
    ]
    assert json.loads(run["channels_skipped"]) == [
        {
            "channel_id": "C_ALERTS",
            "channel_name": "eng-cyclops-alerts",
            "reason": "excluded_by_config:eng-*-alerts",
        },
    ]
    assert json.loads(run["metadata"])["excluded_channel_patterns"] == [
        "eng-*-alerts",
        "*-monitor-*",
    ]
    assert any(log[0] == "slack_sync_channels_excluded" for log in ctx.logs)


@pytest.mark.asyncio
async def test_all_channels_excluded_noops_without_run_row(db_pool, monkeypatch):
    from workflows import slack_sync

    await db_pool.execute(
        "INSERT INTO slack_sync_channels (channel_id, channel_name, is_syncable) "
        "VALUES ('C_OLD', 'old-channel', TRUE)",
    )
    fake = FakeSlackClient(channels=[_alert_channel()])
    ctx = FakeCtx(db_pool)
    monkeypatch.setenv("SLACK_ETL_EXCLUDED_CHANNEL_PATTERNS", "*-alerts")

    with patch.object(slack_sync, "_client", return_value=fake):
        result = await slack_sync.handler(slack_sync.Input(), ctx)

    assert result["status"] == "skipped"
    assert result["reason"] == "all_channels_excluded"
    assert result["channels_skipped"] == [
        {
            "channel_id": "C_ALERTS",
            "channel_name": "eng-cyclops-alerts",
            "reason": "excluded_by_config:*-alerts",
        },
    ]
    assert fake.history_calls == []
    assert fake.list_etl_users_calls == 0
    assert await db_pool.fetchval("SELECT COUNT(*) FROM slack_sync_runs") == 0
    assert (
        await db_pool.fetchval(
            "SELECT is_syncable FROM slack_sync_channels WHERE channel_id = 'C_OLD'",
        )
        is False
    )


@pytest.mark.asyncio
async def test_replayed_workflow_reuses_sync_run_row(db_pool):
    from workflows import slack_sync

    first_client = FakeSlackClient(channels=[_public_channel()])
    second_client = FakeSlackClient(channels=[_public_channel()])
    ctx = FakeCtx(db_pool)

    with patch.object(slack_sync, "_client", return_value=first_client):
        first_result = await slack_sync.handler(slack_sync.Input(), ctx)
    with patch.object(slack_sync, "_client", return_value=second_client):
        second_result = await slack_sync.handler(slack_sync.Input(), ctx)

    assert first_result["run_id"] == second_result["run_id"]
    assert first_result["run_id"] == "slack_sync_wfr_test_slack_sync"
    assert await db_pool.fetchval("SELECT COUNT(*) FROM slack_sync_runs") == 1


@pytest.mark.asyncio
async def test_syncs_user_token_public_channels(
    db_pool,
):
    from workflows import slack_sync

    fake = FakeSlackClient(
        channels=[_public_channel(), _private_channel()],
        users=[
            {
                "id": "U1",
                "name": "alice",
                "real_name": "Alice Example",
                "display_name": "Alice",
                "is_bot": False,
            }
        ],
        messages=[_root_message()],
        replies={"3000000.000000": [_root_message(), _reply_message()]},
    )
    ctx = FakeCtx(db_pool)

    with patch.object(slack_sync, "_client", return_value=fake):
        result = await slack_sync.handler(slack_sync.Input(), ctx)

    assert result["status"] == "completed"
    assert result["channels_synced"] == 1
    assert result["channels_skipped"] == 0
    assert result["messages_upserted"] == 1
    assert result["replies_upserted"] == 0
    assert fake.list_etl_channels_calls == 1
    assert fake.list_bot_channels_calls == 0
    assert fake.list_etl_users_calls == 1
    assert fake.list_users_calls == 0

    channel = await db_pool.fetchrow(
        "SELECT channel_name, is_syncable FROM slack_sync_channels WHERE channel_id = 'C_PUBLIC'",
    )
    assert channel is not None
    assert channel["channel_name"] == "ai-agent"
    assert channel["is_syncable"] is True
    assert (
        await db_pool.fetchval(
            "SELECT COUNT(*) FROM slack_sync_channels WHERE channel_id = 'G_PRIVATE'",
        )
        == 0
    )

    user = await db_pool.fetchrow(
        "SELECT real_name, display_name FROM slack_sync_users WHERE user_id = 'U1'",
    )
    assert user is not None
    assert user["real_name"] == "Alice Example"
    assert user["display_name"] == "Alice"

    messages = await db_pool.fetch(
        "SELECT message_ts, thread_ts, parent_message_ts, text FROM slack_sync_messages "
        "ORDER BY message_ts",
    )
    assert [row["message_ts"] for row in messages] == ["3000000.000000"]
    assert messages[0]["thread_ts"] == "3000000.000000"
    assert messages[0]["parent_message_ts"] is None

    checkpoint = await db_pool.fetchrow(
        "SELECT watermark_ts, last_success_at, last_error FROM slack_sync_checkpoints "
        "WHERE channel_id = 'C_PUBLIC'",
    )
    assert checkpoint is not None
    assert checkpoint["watermark_ts"] == "3000000.000000"
    assert checkpoint["last_success_at"] is not None
    assert checkpoint["last_error"] == ""

    run = await db_pool.fetchrow(
        "SELECT status, channels_requested, channels_skipped, metadata "
        "FROM slack_sync_runs WHERE run_id = $1",
        result["run_id"],
    )
    assert run is not None
    assert run["status"] == "completed"
    assert json.loads(run["channels_requested"])[0]["channel_id"] == "C_PUBLIC"
    assert json.loads(run["channels_skipped"]) == []
    assert json.loads(run["metadata"])["slack_access_mode"] == "user_token"


@pytest.mark.asyncio
async def test_sync_enqueues_thread_refresh_for_recent_thread_roots(db_pool):
    from workflows import slack_sync

    await db_pool.execute(
        "INSERT INTO slack_sync_channels (channel_id, channel_name, is_syncable) "
        "VALUES ('C_PUBLIC', 'ai-agent', TRUE)",
    )
    await db_pool.execute(
        "INSERT INTO slack_sync_checkpoints (channel_id, watermark_ts) "
        "VALUES ('C_PUBLIC', '3000000.000000')",
    )
    fake = FakeSlackClient(
        channels=[_public_channel()],
        messages=[_root_message()],
    )
    ctx = FakeCtx(db_pool)

    with (
        patch.object(slack_sync, "_client", return_value=fake),
        patch.object(slack_sync, "_ts_within_days", return_value=True),
    ):
        result = await slack_sync.handler(slack_sync.Input(), ctx)

    assert result["status"] == "completed"
    assert result["replies_upserted"] == 0
    assert fake.reply_calls == []
    thread_job = await db_pool.fetchrow(
        "SELECT job_type, payload_json, status "
        "FROM slack_sync_backfill_jobs "
        "WHERE job_key = 'thread_refresh:C_PUBLIC:3000000.000000'",
    )
    assert thread_job is not None
    payload = json.loads(str(thread_job["payload_json"]))
    assert thread_job["job_type"] == "thread_refresh"
    assert payload["thread_ts"] == "3000000.000000"
    assert thread_job["status"] == "pending"

    checkpoint = await db_pool.fetchrow(
        "SELECT watermark_ts, last_error FROM slack_sync_checkpoints "
        "WHERE channel_id = 'C_PUBLIC'",
    )
    assert checkpoint is not None
    assert checkpoint["watermark_ts"] == "3000000.000000"
    assert checkpoint["last_error"] == ""


@pytest.mark.asyncio
async def test_sync_skips_thread_refresh_when_recently_refreshed(db_pool):
    from workflows import slack_sync

    await db_pool.execute(
        "INSERT INTO slack_sync_channels (channel_id, channel_name, is_syncable) "
        "VALUES ('C_PUBLIC', 'ai-agent', TRUE)",
    )
    await db_pool.execute(
        "INSERT INTO slack_sync_checkpoints (channel_id, watermark_ts) "
        "VALUES ('C_PUBLIC', '3000000.000000')",
    )
    await db_pool.execute(
        "INSERT INTO slack_sync_messages ("
        "channel_id, message_ts, occurred_at, thread_ts, parent_message_ts, is_thread_root, "
        "user_id, text, reply_count, raw_payload, thread_refreshed_at, updated_at, last_seen_at"
        ") VALUES ("
        "'C_PUBLIC', '3000000.000000', NOW(), '3000000.000000', NULL, TRUE, "
        "'U123', 'root message', 1, '{}'::jsonb, NOW(), NOW(), NOW()"
        ")",
    )
    fake = FakeSlackClient(
        channels=[_public_channel()],
        messages=[_root_message()],
    )
    ctx = FakeCtx(db_pool)

    with (
        patch.object(slack_sync, "_client", return_value=fake),
        patch.object(slack_sync, "_ts_within_days", return_value=True),
    ):
        result = await slack_sync.handler(slack_sync.Input(), ctx)

    assert result["status"] == "completed"
    assert (
        await db_pool.fetchval(
            "SELECT COUNT(*) FROM slack_sync_backfill_jobs "
            "WHERE job_key = 'thread_refresh:C_PUBLIC:3000000.000000'",
        )
        == 0
    )


@pytest.mark.asyncio
async def test_backfill_thread_refresh_replaces_replies_and_marks_root(db_pool):
    from workflows import slack_backfill

    await db_pool.execute(
        "INSERT INTO slack_sync_channels (channel_id, channel_name, is_syncable) "
        "VALUES ('C_PUBLIC', 'ai-agent', TRUE)",
    )
    await db_pool.execute(
        "INSERT INTO slack_sync_messages ("
        "channel_id, message_ts, occurred_at, thread_ts, parent_message_ts, is_thread_root, "
        "user_id, text, reply_count, raw_payload, updated_at, last_seen_at"
        ") VALUES ("
        "'C_PUBLIC', '3000000.000000', NOW(), '3000000.000000', NULL, TRUE, "
        "'U123', 'root message', 1, '{}'::jsonb, NOW(), NOW()"
        ")",
    )
    await db_pool.execute(
        "INSERT INTO slack_sync_messages ("
        "channel_id, message_ts, occurred_at, thread_ts, parent_message_ts, is_thread_root, "
        "user_id, text, reply_count, raw_payload, updated_at, last_seen_at"
        ") VALUES ("
        "'C_PUBLIC', '3000002.000000', NOW(), '3000000.000000', '3000000.000000', FALSE, "
        "'U123', 'stale reply', 0, '{}'::jsonb, NOW(), NOW()"
        ")",
    )
    await db_pool.execute(
        "INSERT INTO slack_sync_backfill_jobs ("
        "job_key, job_type, payload_version, channel_id, payload_json, status"
        ") VALUES ("
        "'thread_refresh:C_PUBLIC:3000000.000000', 'thread_refresh', 1, 'C_PUBLIC', "
        "$1::jsonb, 'pending')",
        json.dumps({"thread_ts": "3000000.000000"}),
    )
    fake = FakeSlackClient(
        channels=[_public_channel()],
        replies={"3000000.000000": [_root_message(), _reply_message()]},
    )
    ctx = FakeCtx(db_pool, run_id="wfr-test-slack-backfill-thread-refresh")

    with patch.object(slack_backfill, "shared_client", return_value=fake):
        result = await slack_backfill.handler(
            slack_backfill.Input(channel_batch_limit=1), ctx
        )

    assert result["status"] == "completed"
    assert fake.history_calls == []
    assert [call["cursor"] for call in fake.reply_calls] == [None]
    assert (
        await db_pool.fetchval(
            "SELECT COUNT(*) FROM slack_sync_messages WHERE parent_message_ts = '3000000.000000'",
        )
        == 1
    )
    assert (
        await db_pool.fetchval(
            "SELECT text FROM slack_sync_messages "
            "WHERE channel_id = 'C_PUBLIC' AND message_ts = '3000001.000000'",
        )
        == "reply"
    )
    assert (
        await db_pool.fetchval(
            "SELECT COUNT(*) FROM slack_sync_messages "
            "WHERE channel_id = 'C_PUBLIC' AND message_ts = '3000002.000000'",
        )
        == 0
    )
    assert (
        await db_pool.fetchval(
            "SELECT thread_refreshed_at IS NOT NULL FROM slack_sync_messages "
            "WHERE channel_id = 'C_PUBLIC' AND message_ts = '3000000.000000'",
        )
        is True
    )


@pytest.mark.asyncio
async def test_backfill_disabled_noops_without_claiming_jobs(db_pool, monkeypatch):
    from workflows import slack_backfill

    await db_pool.execute(
        "INSERT INTO slack_sync_backfill_jobs ("
        "job_key, job_type, payload_version, channel_id, payload_json, status"
        ") VALUES ("
        "'continuation:C_PUBLIC:400000.000000:', 'channel_continuation', 1, "
        "'C_PUBLIC', $1::jsonb, 'pending')",
        json.dumps({"cursor": "cursor-2"}),
    )
    monkeypatch.setenv("SLACK_BACKFILL_ENABLED", "false")
    ctx = FakeCtx(db_pool, run_id="wfr-test-backfill-disabled")

    result = await slack_backfill.handler(slack_backfill.Input(), ctx)

    assert result == {"status": "skipped", "reason": "slack_backfill_disabled"}
    assert (
        await db_pool.fetchval(
            "SELECT status FROM slack_sync_backfill_jobs WHERE job_key = $1",
            "continuation:C_PUBLIC:400000.000000:",
        )
        == "pending"
    )
    assert await db_pool.fetchval("SELECT COUNT(*) FROM slack_sync_runs") == 0


@pytest.mark.asyncio
async def test_backfill_no_pending_jobs_noops_without_run_row(db_pool):
    from workflows import slack_backfill

    ctx = FakeCtx(db_pool, run_id="wfr-test-backfill-empty")

    result = await slack_backfill.handler(slack_backfill.Input(), ctx)

    assert result == {"status": "skipped", "reason": "no_pending_backfills"}
    assert await db_pool.fetchval("SELECT COUNT(*) FROM slack_sync_runs") == 0


@pytest.mark.asyncio
async def test_backfill_invalid_payload_marks_job_failed(db_pool):
    from workflows import slack_backfill

    await db_pool.execute(
        "INSERT INTO slack_sync_backfill_jobs ("
        "job_key, job_type, payload_version, channel_id, payload_json, status"
        ") VALUES ("
        "'continuation:C_PUBLIC:400000.000000:', 'channel_continuation', 2, "
        "'C_PUBLIC', $1::jsonb, 'pending')",
        json.dumps({"cursor": "cursor-2"}),
    )
    fake = FakeSlackClient(channels=[_public_channel()])
    ctx = FakeCtx(db_pool, run_id="wfr-test-backfill-invalid")

    with patch.object(slack_backfill, "shared_client", return_value=fake):
        result = await slack_backfill.handler(
            slack_backfill.Input(channel_batch_limit=1), ctx
        )

    assert result["status"] == "failed"
    job = await db_pool.fetchrow(
        "SELECT status, attempt_count, last_error "
        "FROM slack_sync_backfill_jobs WHERE job_key = $1",
        "continuation:C_PUBLIC:400000.000000:",
    )
    assert job is not None
    assert job["status"] == "failed"
    assert job["attempt_count"] == 1
    assert "unsupported payload version" in job["last_error"]


@pytest.mark.asyncio
async def test_backfill_thread_refresh_paginates_replies(db_pool):
    from workflows import slack_backfill

    second_reply = {
        **_reply_message(),
        "timestamp": "3000002.000000",
        "text": "reply 2",
    }
    await db_pool.execute(
        "INSERT INTO slack_sync_channels (channel_id, channel_name, is_syncable) "
        "VALUES ('C_PUBLIC', 'ai-agent', TRUE)",
    )
    await db_pool.execute(
        "INSERT INTO slack_sync_messages ("
        "channel_id, message_ts, occurred_at, thread_ts, parent_message_ts, is_thread_root, "
        "user_id, text, reply_count, raw_payload, updated_at, last_seen_at"
        ") VALUES ("
        "'C_PUBLIC', '3000000.000000', NOW(), '3000000.000000', NULL, TRUE, "
        "'U123', 'root message', 2, '{}'::jsonb, NOW(), NOW()"
        ")",
    )
    await db_pool.execute(
        "INSERT INTO slack_sync_backfill_jobs ("
        "job_key, job_type, payload_version, channel_id, payload_json, status"
        ") VALUES ("
        "'thread_refresh:C_PUBLIC:3000000.000000', 'thread_refresh', 1, 'C_PUBLIC', "
        "$1::jsonb, 'pending')",
        json.dumps({"thread_ts": "3000000.000000"}),
    )
    fake = FakeSlackClient(
        channels=[_public_channel()],
        reply_pages={
            "3000000.000000": [
                {
                    "messages": [_root_message(), _reply_message()],
                    "has_more": True,
                    "next_cursor": "cursor-2",
                },
                {
                    "messages": [second_reply],
                    "has_more": False,
                    "next_cursor": None,
                },
            ],
        },
    )
    ctx = FakeCtx(db_pool, run_id="wfr-test-backfill-reply-pages")

    with patch.object(slack_backfill, "shared_client", return_value=fake):
        result = await slack_backfill.handler(
            slack_backfill.Input(channel_batch_limit=1, thread_reply_limit=1), ctx
        )

    assert result["status"] == "completed"
    assert [call["cursor"] for call in fake.reply_calls] == [None, "cursor-2"]
    assert result["replies_fetched"] == 2
    assert (
        await db_pool.fetchval(
            "SELECT COUNT(*) FROM slack_sync_messages "
            "WHERE channel_id = 'C_PUBLIC' AND parent_message_ts = '3000000.000000'",
        )
        == 2
    )


@pytest.mark.asyncio
async def test_backfill_thread_refresh_deletes_all_stale_replies_when_slack_returns_none(
    db_pool,
):
    from workflows import slack_backfill

    await db_pool.execute(
        "INSERT INTO slack_sync_messages ("
        "channel_id, message_ts, occurred_at, thread_ts, parent_message_ts, is_thread_root, "
        "user_id, text, reply_count, raw_payload, updated_at, last_seen_at"
        ") VALUES "
        "('C_PUBLIC', '3000000.000000', NOW(), '3000000.000000', NULL, TRUE, "
        "'U123', 'root message', 1, '{}'::jsonb, NOW(), NOW()), "
        "('C_PUBLIC', '3000002.000000', NOW(), '3000000.000000', '3000000.000000', FALSE, "
        "'U123', 'stale reply', 0, '{}'::jsonb, NOW(), NOW())",
    )
    await db_pool.execute(
        "INSERT INTO slack_sync_backfill_jobs ("
        "job_key, job_type, payload_version, channel_id, payload_json, status"
        ") VALUES ("
        "'thread_refresh:C_PUBLIC:3000000.000000', 'thread_refresh', 1, 'C_PUBLIC', "
        "$1::jsonb, 'pending')",
        json.dumps({"thread_ts": "3000000.000000"}),
    )
    fake = FakeSlackClient(
        channels=[_public_channel()],
        replies={"3000000.000000": [_root_message()]},
    )
    ctx = FakeCtx(db_pool, run_id="wfr-test-backfill-zero-replies")

    with patch.object(slack_backfill, "shared_client", return_value=fake):
        result = await slack_backfill.handler(
            slack_backfill.Input(channel_batch_limit=1), ctx
        )

    assert result["status"] == "completed"
    assert result["replies_fetched"] == 0
    assert (
        await db_pool.fetchval(
            "SELECT COUNT(*) FROM slack_sync_messages "
            "WHERE channel_id = 'C_PUBLIC' AND parent_message_ts = '3000000.000000'",
        )
        == 0
    )


@pytest.mark.asyncio
async def test_incremental_oldest_uses_thread_lookback(db_pool):
    from workflows import slack_sync

    await db_pool.execute(
        "INSERT INTO slack_sync_channels (channel_id, channel_name, is_syncable) "
        "VALUES ('C_PUBLIC', 'ai-agent', TRUE)",
    )
    await db_pool.execute(
        "INSERT INTO slack_sync_checkpoints (channel_id, watermark_ts) "
        "VALUES ('C_PUBLIC', '3000000.000000')",
    )
    fake = FakeSlackClient(channels=[_public_channel()], messages=[], replies={})
    ctx = FakeCtx(db_pool)

    with patch.object(slack_sync, "_client", return_value=fake):
        await slack_sync.handler(slack_sync.Input(), ctx)

    assert fake.history_calls[0]["oldest"] == "2740800.000000"


@pytest.mark.asyncio
async def test_first_incremental_run_seeds_historical_backfill_job(db_pool):
    from workflows import slack_sync

    fake = FakeSlackClient(channels=[_public_channel()], messages=[], replies={})
    ctx = FakeCtx(db_pool)

    with (
        patch.object(slack_sync, "_client", return_value=fake),
        patch.object(slack_sync, "_ts_now_minus_hours", return_value="900000.000000"),
        patch.object(slack_sync, "_ts_now_minus_days", return_value="100000.000000"),
    ):
        await slack_sync.handler(slack_sync.Input(), ctx)

    call = fake.history_calls[0]
    assert call["state"]["cursor"] is None
    assert call["oldest"] == "900000.000000"

    backfill = await db_pool.fetchrow(
        "SELECT job_key, job_type, payload_version, payload_json, status "
        "FROM slack_sync_backfill_jobs "
        "WHERE channel_id = 'C_PUBLIC' AND job_key = 'bootstrap:C_PUBLIC'",
    )
    assert backfill is not None
    payload = json.loads(str(backfill["payload_json"]))
    assert backfill["job_type"] == "channel_bootstrap"
    assert backfill["payload_version"] == 1
    assert backfill["job_key"] == "bootstrap:C_PUBLIC"
    assert payload["cursor"] is None
    assert payload["window_oldest"] == "100000.000000"
    assert payload["window_latest"] == call["oldest"]
    assert backfill["status"] == "pending"

    checkpoint = await db_pool.fetchrow(
        "SELECT watermark_ts, last_error FROM slack_sync_checkpoints WHERE channel_id = 'C_PUBLIC'",
    )
    assert checkpoint is not None
    assert checkpoint["watermark_ts"] == "3000000.000000"
    assert checkpoint["last_error"] == ""


@pytest.mark.asyncio
async def test_bootstrap_seed_is_one_row_per_channel_even_without_watermark(db_pool):
    from workflows import slack_sync

    sync_state = {
        "cursor": None,
        "watermark": None,
        "oldest": None,
        "latest": None,
    }
    first_client = FakeSlackClient(
        channels=[_public_channel()], messages=[], sync_state=sync_state
    )
    second_client = FakeSlackClient(
        channels=[_public_channel()], messages=[], sync_state=sync_state
    )

    with (
        patch.object(slack_sync, "_client", return_value=first_client),
        patch.object(slack_sync, "_ts_now_minus_hours", return_value="900000.000000"),
        patch.object(slack_sync, "_ts_now_minus_days", return_value="100000.000000"),
    ):
        await slack_sync.handler(
            slack_sync.Input(), FakeCtx(db_pool, run_id="wfr-bootstrap-1")
        )

    with (
        patch.object(slack_sync, "_client", return_value=second_client),
        patch.object(slack_sync, "_ts_now_minus_hours", return_value="910000.000000"),
        patch.object(slack_sync, "_ts_now_minus_days", return_value="110000.000000"),
    ):
        await slack_sync.handler(
            slack_sync.Input(), FakeCtx(db_pool, run_id="wfr-bootstrap-2")
        )

    assert (
        await db_pool.fetchval(
            "SELECT COUNT(*) FROM slack_sync_backfill_jobs "
            "WHERE channel_id = 'C_PUBLIC' AND job_type = 'channel_bootstrap'",
        )
        == 1
    )
    backfill = await db_pool.fetchrow(
        "SELECT job_key, payload_json, status FROM slack_sync_backfill_jobs "
        "WHERE channel_id = 'C_PUBLIC' AND job_type = 'channel_bootstrap'",
    )
    assert backfill["job_key"] == "bootstrap:C_PUBLIC"
    assert backfill["status"] == "pending"
    payload = json.loads(str(backfill["payload_json"]))
    assert payload["window_oldest"] == "100000.000000"
    assert payload["window_latest"] == "900000.000000"


@pytest.mark.asyncio
async def test_incremental_enqueues_backfill_when_history_page_has_more(db_pool):
    from workflows import slack_sync

    await db_pool.execute(
        "INSERT INTO slack_sync_channels (channel_id, channel_name, is_syncable) "
        "VALUES ('C_PUBLIC', 'ai-agent', TRUE)",
    )
    await db_pool.execute(
        "INSERT INTO slack_sync_checkpoints (channel_id, watermark_ts) "
        "VALUES ('C_PUBLIC', '2500000.000000')",
    )

    fake = FakeSlackClient(
        channels=[_public_channel()],
        history_pages={
            "C_PUBLIC": [
                {
                    "messages": [_root_message()],
                    "has_more": True,
                    "next_cursor": "cursor-2",
                    "sync_state": {
                        "cursor": "cursor-2",
                        "watermark": "3000000.000000",
                        "oldest": "2000000.000000",
                        "latest": None,
                    },
                }
            ],
        },
        replies={"3000000.000000": [_root_message(), _reply_message()]},
    )
    ctx = FakeCtx(db_pool)

    with patch.object(slack_sync, "_client", return_value=fake):
        result = await slack_sync.handler(slack_sync.Input(), ctx)

    assert result["status"] == "completed"
    backfill = await db_pool.fetchrow(
        "SELECT job_key, job_type, payload_version, payload_json, status "
        "FROM slack_sync_backfill_jobs "
        "WHERE channel_id = 'C_PUBLIC' AND job_key LIKE 'continuation:%'",
    )
    assert backfill is not None
    payload = json.loads(str(backfill["payload_json"]))
    assert backfill["job_type"] == "channel_continuation"
    assert backfill["payload_version"] == 1
    assert payload["cursor"] == "cursor-2"
    assert payload["oldest"] == "2000000.000000"
    assert payload["latest"] is None
    assert backfill["status"] == "pending"

    checkpoint = await db_pool.fetchrow(
        "SELECT watermark_ts, last_error FROM slack_sync_checkpoints "
        "WHERE channel_id = 'C_PUBLIC'",
    )
    assert checkpoint is not None
    assert checkpoint["watermark_ts"] == "3000000.000000"
    assert checkpoint["last_error"] == ""


@pytest.mark.asyncio
async def test_backfill_bootstrap_updates_cursor_on_same_row_and_queues_threads(
    db_pool,
):
    from workflows import slack_backfill

    await db_pool.execute(
        "INSERT INTO slack_sync_channels (channel_id, channel_name, is_syncable) "
        "VALUES ('C_PUBLIC', 'ai-agent', TRUE)",
    )
    await db_pool.execute(
        "INSERT INTO slack_sync_backfill_jobs ("
        "job_key, job_type, payload_version, channel_id, payload_json, status"
        ") VALUES ("
        "'bootstrap:C_PUBLIC', 'channel_bootstrap', 1, 'C_PUBLIC', "
        "$1::jsonb, 'pending')",
        json.dumps(
            {
                "cursor": None,
                "window_oldest": "100000.000000",
                "window_latest": "900000.000000",
                "lookback_days": 30,
                "thread_lookback_days": 3,
            }
        ),
    )
    fake = FakeSlackClient(
        channels=[_public_channel()],
        history_pages={
            "C_PUBLIC": [
                {
                    "messages": [_root_message()],
                    "has_more": True,
                    "next_cursor": "cursor-2",
                    "sync_state": {
                        "cursor": "cursor-2",
                        "watermark": "3000000.000000",
                        "oldest": "100000.000000",
                        "latest": "900000.000000",
                    },
                }
            ],
        },
    )
    ctx = FakeCtx(db_pool, run_id="wfr-test-bootstrap-progress")

    with patch.object(slack_backfill, "shared_client", return_value=fake):
        result = await slack_backfill.handler(
            slack_backfill.Input(channel_batch_limit=1, channel_pages_per_job=1),
            ctx,
        )

    assert result["status"] == "completed"
    assert fake.reply_calls == []
    assert fake.history_calls[0]["state"] == {
        "cursor": None,
        "oldest": "100000.000000",
        "latest": "900000.000000",
    }
    assert (
        await db_pool.fetchval(
            "SELECT COUNT(*) FROM slack_sync_backfill_jobs "
            "WHERE channel_id = 'C_PUBLIC' AND job_type = 'channel_bootstrap'",
        )
        == 1
    )
    backfill = await db_pool.fetchrow(
        "SELECT status, attempt_count, payload_json FROM slack_sync_backfill_jobs "
        "WHERE job_key = 'bootstrap:C_PUBLIC'",
    )
    assert backfill["status"] == "pending"
    assert backfill["attempt_count"] == 1
    payload = json.loads(str(backfill["payload_json"]))
    assert payload["cursor"] == "cursor-2"
    assert payload["window_oldest"] == "100000.000000"
    assert payload["window_latest"] == "900000.000000"
    assert (
        await db_pool.fetchval(
            "SELECT COUNT(*) FROM slack_sync_backfill_jobs "
            "WHERE job_key = 'thread_refresh:C_PUBLIC:3000000.000000'",
        )
        == 1
    )


@pytest.mark.asyncio
async def test_backfill_workflow_drains_pending_cursor_without_touching_incremental_checkpoint(
    db_pool,
):
    from workflows import slack_backfill

    await db_pool.execute(
        "INSERT INTO slack_sync_channels (channel_id, channel_name, is_syncable) "
        "VALUES ('C_PUBLIC', 'ai-agent', TRUE)",
    )
    await db_pool.execute(
        "INSERT INTO slack_sync_checkpoints (channel_id, watermark_ts) "
        "VALUES ('C_PUBLIC', '3000000.000000')",
    )
    await db_pool.execute(
        "INSERT INTO slack_sync_backfill_jobs ("
        "job_key, job_type, payload_version, channel_id, payload_json, status"
        ") VALUES ("
        "'continuation:C_PUBLIC:400000.000000:', 'channel_continuation', 1, 'C_PUBLIC', "
        "$1::jsonb, 'pending')",
        json.dumps(
            {
                "cursor": "cursor-2",
                "oldest": "400000.000000",
                "latest": None,
                "lookback_days": 30,
                "thread_lookback_days": 3,
            }
        ),
    )
    fake = FakeSlackClient(
        channels=[_public_channel()],
        history_pages={
            "C_PUBLIC": [
                {
                    "messages": [_root_message()],
                    "has_more": False,
                    "next_cursor": None,
                    "sync_state": {
                        "cursor": None,
                        "watermark": "3000000.000000",
                        "oldest": "400000.000000",
                        "latest": None,
                    },
                }
            ],
        },
        replies={"3000000.000000": [_root_message(), _reply_message()]},
    )
    ctx = FakeCtx(db_pool, run_id="wfr-test-slack-backfill")

    with patch.object(slack_backfill, "shared_client", return_value=fake):
        result = await slack_backfill.handler(
            slack_backfill.Input(channel_batch_limit=1), ctx
        )

    assert result["status"] == "completed"
    assert fake.history_calls[0]["state"]["cursor"] == "cursor-2"
    assert fake.history_calls[0]["state"]["oldest"] == "400000.000000"
    backfill = await db_pool.fetchrow(
        "SELECT status, last_completed_at FROM slack_sync_backfill_jobs WHERE channel_id = 'C_PUBLIC'",
    )
    assert backfill is not None
    assert backfill["status"] == "completed"
    assert backfill["last_completed_at"] is not None
    checkpoint = await db_pool.fetchrow(
        "SELECT watermark_ts, last_error FROM slack_sync_checkpoints "
        "WHERE channel_id = 'C_PUBLIC'",
    )
    assert checkpoint is not None
    assert checkpoint["watermark_ts"] == "3000000.000000"
    assert checkpoint["last_error"] == ""


@pytest.mark.asyncio
async def test_backfill_channel_job_does_not_reopen_completed_thread_refresh(db_pool):
    from workflows import slack_backfill

    await db_pool.execute(
        "INSERT INTO slack_sync_backfill_jobs ("
        "job_key, job_type, payload_version, channel_id, payload_json, status"
        ") VALUES ("
        "'continuation:C_PUBLIC:400000.000000:', 'channel_continuation', 1, "
        "'C_PUBLIC', $1::jsonb, 'pending')",
        json.dumps(
            {
                "cursor": "cursor-2",
                "oldest": "400000.000000",
                "latest": None,
                "lookback_days": 30,
                "thread_lookback_days": 3,
            }
        ),
    )
    await db_pool.execute(
        "INSERT INTO slack_sync_backfill_jobs ("
        "job_key, job_type, payload_version, channel_id, payload_json, status, "
        "last_completed_at"
        ") VALUES ("
        "'thread_refresh:C_PUBLIC:3000000.000000', 'thread_refresh', 1, 'C_PUBLIC', "
        "$1::jsonb, 'completed', NOW())",
        json.dumps({"thread_ts": "3000000.000000"}),
    )
    fake = FakeSlackClient(
        channels=[_public_channel()],
        history_pages={
            "C_PUBLIC": [
                {
                    "messages": [_root_message()],
                    "has_more": False,
                    "next_cursor": None,
                    "sync_state": {
                        "cursor": None,
                        "watermark": "3000000.000000",
                        "oldest": "400000.000000",
                        "latest": None,
                    },
                }
            ],
        },
    )
    ctx = FakeCtx(db_pool, run_id="wfr-test-completed-thread-job")

    with patch.object(slack_backfill, "shared_client", return_value=fake):
        result = await slack_backfill.handler(
            slack_backfill.Input(channel_batch_limit=1), ctx
        )

    assert result["status"] == "completed"
    thread_job = await db_pool.fetchrow(
        "SELECT status, last_completed_at, attempt_count "
        "FROM slack_sync_backfill_jobs WHERE job_key = $1",
        "thread_refresh:C_PUBLIC:3000000.000000",
    )
    assert thread_job is not None
    assert thread_job["status"] == "completed"
    assert thread_job["last_completed_at"] is not None
    assert thread_job["attempt_count"] == 0


@pytest.mark.asyncio
async def test_failed_write_does_not_advance_watermark(db_pool):
    from workflows import slack_sync

    fake = FakeSlackClient(channels=[_public_channel()], messages=[_root_message()])
    ctx = FakeCtx(db_pool)

    with (
        patch.object(slack_sync, "_client", return_value=fake),
        patch.object(
            slack_sync,
            "_upsert_messages",
            new=AsyncMock(side_effect=RuntimeError("write failed")),
        ),
    ):
        result = await slack_sync.handler(slack_sync.Input(), ctx)

    assert result["status"] == "failed"
    checkpoint = await db_pool.fetchrow(
        "SELECT watermark_ts, last_error FROM slack_sync_checkpoints "
        "WHERE channel_id = 'C_PUBLIC'",
    )
    assert checkpoint is not None
    assert checkpoint["watermark_ts"] is None
    assert checkpoint["last_error"] == "write failed"


@pytest.mark.asyncio
async def test_incremental_partial_failure_preserves_successful_channel_checkpoint(
    db_pool,
):
    from workflows import slack_sync

    fake = FakeSlackClient(channels=[_public_channel(), _other_public_channel()])

    def sync_history(
        channel: str,
        state: dict[str, Any] | None = None,
        limit: int = 200,
        lookback_days: int = 30,
        oldest: str | int | float | None = None,
        latest: str | int | float | None = None,
    ) -> dict[str, Any]:
        fake.history_calls.append(
            {
                "channel": channel,
                "state": state,
                "limit": limit,
                "lookback_days": lookback_days,
                "oldest": oldest,
                "latest": latest,
            }
        )
        if channel == "C_OTHER":
            raise RuntimeError("Slack API error: missing_scope")
        return {
            "channel": channel,
            "channel_id": channel,
            "messages": [],
            "count": 0,
            "has_more": False,
            "next_cursor": None,
            "sync_state": {
                "cursor": None,
                "watermark": "3000000.000000",
                "oldest": None,
                "latest": None,
            },
        }

    fake._sync_etl_channel_history = sync_history  # type: ignore[method-assign]
    ctx = FakeCtx(db_pool, run_id="wfr-test-partial-failure")

    with patch.object(slack_sync, "_client", return_value=fake):
        result = await slack_sync.handler(slack_sync.Input(), ctx)

    assert result["status"] == "partial_failed"
    assert result["channels_synced"] == 1
    assert result["channels_failed"] == 1
    public_checkpoint = await db_pool.fetchrow(
        "SELECT watermark_ts, last_error FROM slack_sync_checkpoints "
        "WHERE channel_id = 'C_PUBLIC'",
    )
    failed_checkpoint = await db_pool.fetchrow(
        "SELECT watermark_ts, last_error FROM slack_sync_checkpoints "
        "WHERE channel_id = 'C_OTHER'",
    )
    assert public_checkpoint is not None
    assert public_checkpoint["watermark_ts"] == "3000000.000000"
    assert public_checkpoint["last_error"] == ""
    assert failed_checkpoint is not None
    assert failed_checkpoint["watermark_ts"] is None
    assert failed_checkpoint["last_error"] == "Slack API error: missing_scope"

    run = await db_pool.fetchrow(
        "SELECT status, channels_synced, channels_failed, error_text "
        "FROM slack_sync_runs WHERE run_id = $1",
        result["run_id"],
    )
    assert run is not None
    assert run["status"] == "partial_failed"
    assert json.loads(run["channels_synced"]) == [
        {"channel_id": "C_PUBLIC", "channel_name": "ai-agent"},
    ]
    assert json.loads(run["channels_failed"]) == [
        {
            "channel_id": "C_OTHER",
            "channel_name": "other-channel",
            "reason": "Slack API error: missing_scope",
        }
    ]
    assert run["error_text"] == "1 channel(s) failed"


@pytest.mark.asyncio
async def test_etl_freshness_metrics_refresh_from_slack_sync_tables(db_pool):
    from api.vm_metrics import render_metrics

    now = dt.datetime.now(dt.timezone.utc)
    await db_pool.execute(
        "INSERT INTO slack_sync_channels (channel_id, channel_name, is_syncable) "
        "VALUES ('C_PUBLIC', 'ai-agent', TRUE), ('C_OTHER', 'other-channel', TRUE)",
    )
    await db_pool.execute(
        "INSERT INTO slack_sync_checkpoints (channel_id, watermark_ts, last_success_at, last_error) "
        "VALUES ($1, $2, $3, ''), ($4, $5, NULL, 'write_error')",
        "C_PUBLIC",
        f"{(now - dt.timedelta(seconds=60)).timestamp():.6f}",
        now - dt.timedelta(seconds=30),
        "C_OTHER",
        f"{(now - dt.timedelta(seconds=120)).timestamp():.6f}",
    )
    await db_pool.execute(
        "INSERT INTO slack_sync_backfill_jobs ("
        "job_key, job_type, payload_version, channel_id, status, payload_json, updated_at"
        ") VALUES "
        "('bootstrap:C_PUBLIC', 'channel_bootstrap', 1, 'C_PUBLIC', 'pending', $1::jsonb, $2), "
        "('continuation:C_OTHER::', 'channel_continuation', 1, 'C_OTHER', 'failed', $3::jsonb, $4), "
        "('thread_refresh:C_PUBLIC:3000000.000000', 'thread_refresh', 1, 'C_PUBLIC', 'running', $5::jsonb, $6)",
        json.dumps({"window_oldest": "100", "window_latest": "123"}),
        now - dt.timedelta(seconds=300),
        json.dumps({"cursor": "cursor-2"}),
        now - dt.timedelta(seconds=180),
        json.dumps({"thread_ts": "3000000.000000"}),
        now - dt.timedelta(seconds=60),
    )
    await db_pool.execute(
        "INSERT INTO google_drive_sync_checkpoints ("
        "scope_id, watermark_time, last_success_at, last_error, updated_at"
        ") VALUES ('all_visible', $1, $2, '', $2)",
        now - dt.timedelta(seconds=90),
        now - dt.timedelta(seconds=45),
    )
    await db_pool.execute(
        "INSERT INTO google_drive_sync_files ("
        "file_id, name, mime_type, source_modified_at, text_content, text_hash, updated_at"
        ") VALUES ("
        "'doc-1', 'Drive doc', 'application/vnd.google-apps.document', $1, "
        "'Drive body', 'hash', $2"
        ")",
        now - dt.timedelta(seconds=120),
        now - dt.timedelta(seconds=20),
    )
    await db_pool.execute(
        "INSERT INTO google_calendar_sync_calendars (calendar_id, summary, updated_at) "
        "VALUES ('primary@example.com', 'Primary', $1), ('team@example.com', 'Team', $2)",
        now - dt.timedelta(seconds=40),
        now - dt.timedelta(seconds=35),
    )
    await db_pool.execute(
        "INSERT INTO google_calendar_sync_checkpoints ("
        "calendar_id, sync_token, watermark_time, last_success_at, last_error, updated_at"
        ") VALUES "
        "('primary@example.com', 'token-1', $1, $2, '', $2), "
        "('team@example.com', 'token-2', $3, NULL, 'api_error', $4)",
        now - dt.timedelta(seconds=150),
        now - dt.timedelta(seconds=55),
        now - dt.timedelta(seconds=240),
        now - dt.timedelta(seconds=50),
    )
    await db_pool.execute(
        "INSERT INTO company_context_documents ("
        "document_id, source, source_type, source_document_id, title, body, "
        "source_updated_at, content_hash"
        ") VALUES ("
        "'google_drive:doc:doc-1', 'google_drive', 'google_doc', 'doc-1', "
        "'Drive doc', 'Drive body', $1, 'hash'"
        ")",
        now - dt.timedelta(seconds=70),
    )

    metrics = (await render_metrics(db_pool)).decode()

    assert 'etl_active_scopes{source="slack",source_type="channel"} 2' in metrics
    assert 'etl_failed_scopes{source="slack",source_type="channel"} 1' in metrics
    assert 'etl_active_scopes{source="google_drive",source_type="doc"} 1' in metrics
    assert 'etl_failed_scopes{source="google_drive",source_type="doc"} 0' in metrics
    assert 'etl_active_scopes{source="google_calendar",source_type="calendar"} 2' in metrics
    assert 'etl_failed_scopes{source="google_calendar",source_type="calendar"} 1' in metrics
    assert (
        'etl_backfill_jobs{job_type="channel_bootstrap",source="slack",status="pending"} 1'
        in metrics
    )
    assert (
        'etl_backfill_jobs{job_type="channel_continuation",source="slack",status="failed"} 1'
        in metrics
    )
    assert (
        'etl_backfill_jobs{job_type="thread_refresh",source="slack",status="running"} 1'
        in metrics
    )
    match = re.search(
        r'etl_source_cursor_lag_seconds\{source="slack",source_type="channel"\} ([0-9.]+)',
        metrics,
    )
    assert match is not None
    assert float(match.group(1)) >= 100
    freshness_match = re.search(
        r'etl_scope_sync_freshness_seconds\{source="slack",source_type="channel"\} ([0-9.]+)',
        metrics,
    )
    assert freshness_match is not None
    assert 25 <= float(freshness_match.group(1)) < 100
    drive_lag_match = re.search(
        r'etl_source_cursor_lag_seconds\{source="google_drive",source_type="doc"\} ([0-9.]+)',
        metrics,
    )
    assert drive_lag_match is not None
    assert float(drive_lag_match.group(1)) >= 80
    drive_freshness_match = re.search(
        r'etl_scope_sync_freshness_seconds\{source="google_drive",source_type="doc"\} ([0-9.]+)',
        metrics,
    )
    assert drive_freshness_match is not None
    assert 40 <= float(drive_freshness_match.group(1)) < 120
    calendar_lag_match = re.search(
        r'etl_source_cursor_lag_seconds\{source="google_calendar",source_type="calendar"\} ([0-9.]+)',
        metrics,
    )
    assert calendar_lag_match is not None
    assert float(calendar_lag_match.group(1)) >= 200
    calendar_freshness_match = re.search(
        r'etl_scope_sync_freshness_seconds\{source="google_calendar",source_type="calendar"\} ([0-9.]+)',
        metrics,
    )
    assert calendar_freshness_match is not None
    assert 50 <= float(calendar_freshness_match.group(1)) < 130
    drive_projection_lag_match = re.search(
        r'company_context_projection_lag_seconds\{source="google_drive"\} ([0-9.]+)',
        metrics,
    )
    assert drive_projection_lag_match is not None
    assert float(drive_projection_lag_match.group(1)) >= 45
    age_match = re.search(
        r'etl_backfill_job_age_seconds\{job_type="channel_bootstrap",source="slack",status="pending"\} ([0-9.]+)',
        metrics,
    )
    assert age_match is not None
    assert float(age_match.group(1)) >= 250
