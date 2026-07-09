from __future__ import annotations

import asyncio
import hashlib
import importlib
import json
import sys
import types
from pathlib import Path


def _load_shared():
    repo_root = Path(__file__).resolve().parents[3]
    if str(repo_root) not in sys.path:
        sys.path.insert(0, str(repo_root))

    api_module = types.ModuleType("api")
    runtime_control = types.ModuleType("api.runtime_control")
    runtime_control.canonical_json = lambda value: json.dumps(value, sort_keys=True)
    api_module.runtime_control = runtime_control
    sys.modules.setdefault("api", api_module)
    sys.modules.setdefault("api.runtime_control", runtime_control)

    slack_metrics = types.ModuleType("workflows.slack.metrics")
    slack_metrics.record_slack_etl_rate_limit = lambda *_args, **_kwargs: None
    sys.modules["workflows.slack.metrics"] = slack_metrics

    etl_metrics = types.ModuleType("workflows.etl_metrics")
    for name in (
        "set_etl_active_scopes",
        "set_etl_failed_scopes",
        "set_etl_scope_sync_freshness_seconds",
    ):
        setattr(etl_metrics, name, lambda *_args, **_kwargs: None)
    sys.modules["workflows.etl_metrics"] = etl_metrics

    centaur_sdk = types.ModuleType("centaur_sdk")
    centaur_sdk.secret = lambda _name, default=None: default
    sys.modules.setdefault("centaur_sdk", centaur_sdk)

    return importlib.import_module("workflows.slack.shared")


def _load_backfill():
    api_module = sys.modules.setdefault("api", types.ModuleType("api"))

    etl_metrics = types.ModuleType("workflows.etl_metrics")
    for name in (
        "record_etl_items_deleted",
        "record_etl_items_enqueued",
        "record_etl_items_failed",
        "record_etl_items_seen",
        "record_etl_items_upserted",
        "set_etl_active_scopes",
        "set_etl_backfill_job_age_seconds",
        "set_etl_backfill_jobs",
        "set_etl_failed_scopes",
        "set_etl_scope_sync_freshness_seconds",
    ):
        setattr(etl_metrics, name, lambda *_args, **_kwargs: None)
    sys.modules["workflows.etl_metrics"] = etl_metrics

    slack_metrics = types.ModuleType("workflows.slack.metrics")
    for name in (
        "observe_slack_retention_run_duration",
        "record_slack_etl_rate_limit",
        "record_slack_retention_api_rate_limited",
        "record_slack_retention_api_request",
        "record_slack_retention_backfill_job",
        "record_slack_retention_backfill_job_failure",
        "record_slack_retention_backfill_terminal_skip",
        "record_slack_retention_failure",
        "record_slack_retention_messages_processed",
        "record_slack_retention_run",
        "set_slack_retention_last_failure_timestamp",
        "set_slack_retention_watermark_lag_seconds",
    ):
        setattr(slack_metrics, name, lambda *_args, **_kwargs: None)
    sys.modules["workflows.slack.metrics"] = slack_metrics

    workflow_engine = types.ModuleType("api.workflow_engine")

    class WorkflowContext:
        pass

    workflow_engine.WorkflowContext = WorkflowContext
    api_module.workflow_engine = workflow_engine
    sys.modules["api.workflow_engine"] = workflow_engine

    return importlib.import_module("workflows.slack.backfill")


shared = _load_shared()


class _FakeSlackResponse(dict):
    def __init__(
        self,
        *,
        error: str = "ratelimited",
        headers: dict[str, str] | None = None,
        status_code: int = 429,
    ) -> None:
        super().__init__({"error": error})
        self.headers = headers or {}
        self.status_code = status_code


class _FakeSlackError(Exception):
    def __init__(self, response: _FakeSlackResponse) -> None:
        super().__init__("rate limited")
        self.response = response


def test_retry_on_ratelimit_records_slept_retry_by_workflow(monkeypatch):
    client = object.__new__(shared.SlackEtlClient)
    client._workflow_name = "slack_backfill"
    client._ratelimit_deadlines = {}
    calls = {"api": 0, "sleep": [], "metrics": []}
    clock = {"now": 1000.0}

    def fake_api_call():
        calls["api"] += 1
        if calls["api"] == 1:
            raise _FakeSlackError(_FakeSlackResponse(headers={"Retry-After": "2"}))
        return {"ok": True}

    monkeypatch.setattr(
        shared,
        "record_slack_etl_rate_limit",
        lambda *args: calls["metrics"].append(args),
    )

    def fake_sleep(seconds):
        calls["sleep"].append(seconds)
        clock["now"] += seconds

    monkeypatch.setattr(shared.time, "sleep", fake_sleep)
    monkeypatch.setattr(shared.time, "time", lambda: clock["now"])

    result = client._retry_on_ratelimit(
        fake_api_call,
        method_key="etl.conversations.history",
    )

    assert result == {"ok": True}
    assert calls["sleep"] == [2.25]
    assert calls["metrics"] == [
        ("slack_backfill", "etl.conversations.history", "slept_retry", 2.25)
    ]


def test_retry_on_ratelimit_records_failed_fast_by_workflow(monkeypatch):
    client = object.__new__(shared.SlackEtlClient)
    client._workflow_name = "slack_sync"
    client._ratelimit_deadlines = {}
    calls = []

    def fake_api_call():
        raise _FakeSlackError(_FakeSlackResponse(headers={"Retry-After": "45"}))

    monkeypatch.setattr(
        shared,
        "record_slack_etl_rate_limit",
        lambda *args: calls.append(args),
    )

    try:
        client._retry_on_ratelimit(
            fake_api_call,
            method_key="etl.conversations.replies",
        )
    except shared.SlackEtlRateLimitError as exc:
        assert exc.payload["retry_after_seconds"] == 45.25
    else:
        raise AssertionError("expected SlackEtlRateLimitError")

    assert calls == [
        ("slack_sync", "etl.conversations.replies", "failed_fast", 45.25)
    ]


def test_list_etl_channels_preserves_slack_created_timestamp():
    client = object.__new__(shared.SlackEtlClient)
    client._workflow_name = "slack_sync"
    retry_calls = []

    def fake_conversations_list(**_kwargs):
        raise AssertionError("wrapped Slack client call should not be used directly")

    client._client = types.SimpleNamespace(conversations_list=fake_conversations_list)

    def fake_retry(_func, **kwargs):
        retry_calls.append(kwargs)
        return {
            "channels": [
                {
                    "id": "C123",
                    "name": "eng-infra",
                    "created": 1718123456,
                    "purpose": {"value": "infra"},
                    "topic": {"value": "ops"},
                    "num_members": 42,
                    "is_archived": False,
                    "is_private": False,
                    "is_member": True,
                },
                {
                    "id": "G123",
                    "name": "private-room",
                    "created": 1718123000,
                    "is_private": True,
                },
            ],
            "response_metadata": {},
        }

    client._retry_on_ratelimit = fake_retry

    channels = client._list_etl_channels()

    assert retry_calls[0]["types"] == "public_channel"
    assert channels == [
        {
            "id": "C123",
            "name": "eng-infra",
            "created": 1718123456,
            "purpose": "infra",
            "topic": "ops",
            "member_count": 42,
            "is_archived": False,
            "is_private": False,
            "is_member": True,
        }
    ]


def test_list_etl_channels_can_include_private_channels():
    client = object.__new__(shared.SlackEtlClient)
    client._workflow_name = "slack_sync"
    retry_calls = []

    def fake_conversations_list(**_kwargs):
        raise AssertionError("wrapped Slack client call should not be used directly")

    client._client = types.SimpleNamespace(conversations_list=fake_conversations_list)

    def fake_retry(_func, **kwargs):
        retry_calls.append(kwargs)
        return {
            "channels": [
                {
                    "id": "C123",
                    "name": "eng-infra",
                    "is_archived": False,
                    "is_private": False,
                },
                {
                    "id": "G123",
                    "name": "private-room",
                    "is_archived": False,
                    "is_private": True,
                    "is_member": True,
                },
            ],
            "response_metadata": {},
        }

    client._retry_on_ratelimit = fake_retry

    channels = client._list_etl_channels(include_private_channels=True)

    assert retry_calls[0]["types"] == "public_channel,private_channel"
    assert [channel["id"] for channel in channels] == ["C123", "G123"]
    assert channels[1]["is_private"] is True


def test_serialize_message_downloads_slack_file_bytes(monkeypatch):
    monkeypatch.setenv("SLACK_ETL_ATTACHMENTS_ENABLED", "true")
    monkeypatch.setenv("SLACK_ETL_ATTACHMENT_MAX_BYTES", "100")
    client = object.__new__(shared.SlackEtlClient)
    client.token = "xoxp-test"

    def fake_download(url: str, *, max_bytes: int):
        assert url == "https://files.slack.com/files-pri/T/F-test/download/report.txt"
        assert max_bytes == 100
        return "text/plain", b"hello"

    monkeypatch.setattr(client, "_download_slack_file_bytes", fake_download)

    message = client._serialize_message(
        {
            "user": "U123",
            "text": "see attached",
            "ts": "1770000000.000100",
            "files": [
                {
                    "id": "F123",
                    "name": "report.txt",
                    "title": "Report",
                    "mimetype": "",
                    "filetype": "text",
                    "size": 5,
                    "url_private_download": (
                        "https://files.slack.com/files-pri/T/F-test/download/report.txt"
                    ),
                }
            ],
        },
        "C123",
        {"U123": "alice"},
    )

    assert message["files"][0]["download_status"] == "downloaded"
    assert message["files"][0]["content_bytes"] == b"hello"
    assert message["files"][0]["content_sha256"] == hashlib.sha256(b"hello").hexdigest()
    assert message["files"][0]["mimetype"] == "text/plain"


def test_serialize_message_skips_oversized_slack_file(monkeypatch):
    monkeypatch.setenv("SLACK_ETL_ATTACHMENTS_ENABLED", "true")
    monkeypatch.setenv("SLACK_ETL_ATTACHMENT_MAX_BYTES", "10")
    client = object.__new__(shared.SlackEtlClient)
    client.token = "xoxp-test"

    def fail_download(*_args, **_kwargs):
        raise AssertionError("oversized files should not be downloaded")

    monkeypatch.setattr(client, "_download_slack_file_bytes", fail_download)

    message = client._serialize_message(
        {
            "user": "U123",
            "text": "",
            "ts": "1770000000.000200",
            "files": [
                {
                    "id": "F-large",
                    "name": "large.mov",
                    "size": 11,
                    "url_private": "https://files.slack.com/files-pri/T/F-large",
                }
            ],
        },
        "C123",
        {},
    )

    assert message["files"][0]["download_status"] == "skipped_too_large"
    assert "SLACK_ETL_ATTACHMENT_MAX_BYTES" in message["files"][0]["download_error"]
    assert message["files"][0]["content_bytes"] is None


def test_serialize_message_preserves_bot_attachments_and_blocks():
    client = object.__new__(shared.SlackEtlClient)
    attachments = [
        {
            "fallback": "Deployment approved by <@U04ABCDEF>",
            "title": "unused when fallback is present",
            "color": "36a64f",
        }
    ]
    blocks = [{"type": "section", "text": {"type": "mrkdwn", "text": "hi"}}]

    message = client._serialize_message(
        {
            "bot_id": "B02T451BQRK",
            "text": "",
            "ts": "1770000000.000600",
            "attachments": attachments,
            "blocks": blocks,
        },
        "C123",
        {"U04ABCDEF": "artur"},
    )

    assert message["attachments"] == attachments
    assert message["blocks"] == blocks
    # Fallback text is mention-resolved, same as the primary text path.
    assert message["text"] == "Deployment approved by @artur"

    row = shared.message_row(message, "run_123")
    assert row["raw_payload"]["attachments"] == attachments
    assert row["raw_payload"]["blocks"] == blocks
    assert row["text"] == "Deployment approved by @artur"
    # Slack legacy attachments are message content, not file rows.
    assert row["attachments"] == []


def test_serialize_message_keeps_user_text_over_attachment_fallback():
    client = object.__new__(shared.SlackEtlClient)

    message = client._serialize_message(
        {
            "user": "U123",
            "text": "see the graph below",
            "ts": "1770000000.000700",
            "attachments": [{"fallback": "graph.png"}],
            "blocks": [{"type": "rich_text", "elements": []}],
        },
        "C123",
        {"U123": "alice"},
    )

    assert message["text"] == "see the graph below"
    assert message["attachments"] == [{"fallback": "graph.png"}]
    # Human rich_text blocks mirror the text; not persisted.
    assert message["blocks"] == []


def test_attachment_fallback_text_prefers_fallback_then_joins_fields():
    assert shared._attachment_fallback_text(None) == ""
    assert shared._attachment_fallback_text("not-a-list") == ""
    assert (
        shared._attachment_fallback_text(
            [
                "not-a-dict",
                {"fallback": "  first alert  "},
                {
                    "pretext": "PR merged",
                    "title": "moonwell-fi/mamo",
                    "text": "fix: handle empty originTx",
                },
                {"color": "dddddd"},
            ]
        )
        == "first alert\nPR merged\nmoonwell-fi/mamo\nfix: handle empty originTx"
    )


class FakeConn:
    """Records statements issued inside ``upsert_messages``/attachment batch."""

    def __init__(self) -> None:
        self.execute_calls: list[tuple] = []
        self.executemany_calls: list[tuple] = []

    async def execute(self, sql, *args):
        self.execute_calls.append((sql, args))

    async def executemany(self, sql, args_list):
        # Materialize so assertions are stable even if a generator is passed.
        self.executemany_calls.append((sql, list(args_list)))

    def transaction(self):
        conn = self

        class _Txn:
            async def __aenter__(self_):
                return conn

            async def __aexit__(self_, *_exc):
                return False

        return _Txn()


class FakePool:
    def __init__(self, conn: FakeConn) -> None:
        self._conn = conn

    def acquire(self):
        conn = self._conn

        class _Acquire:
            async def __aenter__(self_):
                return conn

            async def __aexit__(self_, *_exc):
                return False

        return _Acquire()


class FakeExecutePool:
    def __init__(self) -> None:
        self.execute_calls: list[tuple] = []

    async def execute(self, sql, *args):
        self.execute_calls.append((sql, args))


class FakeFetchRowPool:
    def __init__(self, row) -> None:
        self.row = row
        self.fetchrow_calls: list[tuple] = []

    async def fetchrow(self, sql, *args):
        self.fetchrow_calls.append((sql, args))
        return self.row


def test_emit_slack_checkpoint_metrics_reads_slack_checkpoints(monkeypatch):
    calls: dict[str, list] = {
        "active": [],
        "failed": [],
        "freshness": [],
    }
    monkeypatch.setattr(
        shared,
        "set_etl_active_scopes",
        lambda *args: calls["active"].append(args),
    )
    monkeypatch.setattr(
        shared,
        "set_etl_failed_scopes",
        lambda *args: calls["failed"].append(args),
    )
    monkeypatch.setattr(
        shared,
        "set_etl_scope_sync_freshness_seconds",
        lambda *args: calls["freshness"].append(args),
    )
    pool = FakeFetchRowPool(
        {
            "active_scopes": 42,
            "failed_scopes": 3,
            "freshness_seconds": 123.5,
        }
    )

    asyncio.run(shared.emit_slack_checkpoint_metrics(pool))

    assert len(pool.fetchrow_calls) == 1
    query = pool.fetchrow_calls[0][0]
    assert "FROM slack_sync_checkpoints c" in query
    assert "JOIN slack_sync_channels ch ON ch.channel_id = c.channel_id" in query
    assert "ch.is_syncable IS TRUE" in query
    assert "ch.is_archived IS FALSE" in query
    assert calls == {
        "active": [("slack", 42)],
        "failed": [("slack", 3)],
        "freshness": [("slack", 123.5)],
    }


def test_permanent_slack_backfill_error_classifier():
    assert shared.is_permanent_slack_backfill_error(
        "Slack API error: thread_not_found"
    )
    assert shared.is_permanent_slack_backfill_error(
        "Slack API error: channel_not_found"
    )
    assert not shared.is_permanent_slack_backfill_error(
        "Slack API error: rate_limited"
    )
    assert not shared.is_permanent_slack_backfill_error("database write failed")


def test_mark_backfill_job_terminal_skipped_completes_without_retry():
    pool = FakeExecutePool()

    asyncio.run(
        shared.mark_backfill_job_terminal_skipped(
            pool,
            job_id=123,
            run_id="run_123",
            error="Slack API error: thread_not_found",
        )
    )

    assert len(pool.execute_calls) == 1
    sql, args = pool.execute_calls[0]
    assert "status = 'completed'" in sql
    assert "terminal_skip_reason" in sql
    assert "terminal_skip_at" in sql
    assert "last_error = ''" in sql
    assert args == (123, "run_123", "Slack API error: thread_not_found")


def test_backfill_handler_terminally_skips_permanent_slack_errors(monkeypatch):
    monkeypatch.setenv("SLACK_ETL_ENABLED", "true")
    monkeypatch.setenv("SLACK_BACKFILL_ENABLED", "true")
    backfill = _load_backfill()
    calls: dict[str, list] = {
        "finish": [],
        "terminal_skip": [],
        "failure_metrics": [],
    }

    class FakeClient:
        def _etl_access_mode(self):
            return "test"

        def _get_etl_thread_replies_page(self, *_args, **_kwargs):
            raise RuntimeError("Slack API error: thread_not_found")

    class FakeContext:
        run_id = "wfr_123"
        _pool = object()

        def __init__(self) -> None:
            self.logs: list[tuple] = []

        def log(self, name, **fields):
            self.logs.append((name, fields))

    async def fake_claim_jobs(_pool, _limit):
        return [
            {
                "job_id": 123,
                "job_key": "thread_refresh:C123:1770000000.000100",
                "job_type": shared.BACKFILL_JOB_THREAD_REFRESH,
                "payload_version": shared.BACKFILL_JOB_PAYLOAD_VERSION,
                "channel_id": "C123",
                "payload_json": {"thread_ts": "1770000000.000100"},
                "priority": 200,
                "attempt_count": 1,
            }
        ]

    async def fake_noop(*_args, **_kwargs):
        return None

    async def fake_record_finish(_pool, **kwargs):
        calls["finish"].append(kwargs)

    async def fake_terminal_skip(_pool, **kwargs):
        calls["terminal_skip"].append(kwargs)

    monkeypatch.setattr(backfill, "_emit_backfill_job_metrics", fake_noop)
    monkeypatch.setattr(backfill, "emit_slack_checkpoint_metrics", fake_noop)
    monkeypatch.setattr(backfill, "claim_backfill_jobs", fake_claim_jobs)
    monkeypatch.setattr(backfill, "touch_backfill_job_started", fake_noop)
    monkeypatch.setattr(backfill, "shared_client", lambda **_kwargs: FakeClient())
    monkeypatch.setattr(backfill, "record_run_start", fake_noop)
    monkeypatch.setattr(backfill, "record_run_finish", fake_record_finish)
    monkeypatch.setattr(
        backfill, "mark_backfill_job_terminal_skipped", fake_terminal_skip
    )
    monkeypatch.setattr(
        backfill,
        "record_etl_items_failed",
        lambda *args, **kwargs: calls["failure_metrics"].append((args, kwargs)),
    )

    ctx = FakeContext()
    result = asyncio.run(backfill.handler(backfill.Input(channel_batch_limit=1), ctx))

    assert result["status"] == "completed"
    assert result["channels_skipped"] == 1
    assert result["channels_failed"] == 0
    assert calls["failure_metrics"] == []
    assert calls["terminal_skip"] == [
        {
            "job_id": 123,
            "run_id": "slack_sync_wfr_123",
            "error": "Slack API error: thread_not_found",
        }
    ]
    assert calls["finish"][0]["status"] == "completed"
    assert len(calls["finish"][0]["skipped"]) == 1
    assert calls["finish"][0]["failed"] == []
    assert any(name == "slack_backfill_job_terminal_skipped" for name, _ in ctx.logs)


def test_replace_message_attachments_batch_upserts_and_deletes_stale_rows():
    conn = FakeConn()
    row = shared.message_row(
        {
            "channel_id": "C123",
            "timestamp": "1770000000.000300",
            "files": [
                {
                    "id": "F123",
                    "name": "report.txt",
                    "title": "Report",
                    "mimetype": "text/plain",
                    "filetype": "text",
                    "size": 5,
                    "url_private": "https://files.slack.com/files-pri/T/F123",
                    "permalink": "https://example.slack.com/files/F123",
                    "download_status": "downloaded",
                    "content_sha256": hashlib.sha256(b"hello").hexdigest(),
                    "content_bytes": b"hello",
                }
            ],
        },
        "run_123",
    )

    assert "content_bytes" not in row["raw_payload"]["files"][0]
    asyncio.run(shared._replace_message_attachments_batch(conn, [row]))

    # One batched upsert for the attachment, one set-based stale delete.
    assert len(conn.executemany_calls) == 1
    upsert_sql, upsert_args_list = conn.executemany_calls[0]
    assert "INSERT INTO slack_sync_message_attachments" in upsert_sql
    assert len(upsert_args_list) == 1
    assert upsert_args_list[0][0:4] == (
        "C123",
        "1770000000.000300",
        "F123",
        "report.txt",
    )
    assert upsert_args_list[0][13] == b"hello"

    assert len(conn.execute_calls) == 1
    delete_sql, delete_args = conn.execute_calls[0]
    assert "DELETE FROM slack_sync_message_attachments" in delete_sql
    assert "NOT EXISTS" in delete_sql
    # (message keys) then (kept attachment keys) as parallel arrays.
    assert delete_args == (
        ["C123"],
        ["1770000000.000300"],
        ["C123"],
        ["1770000000.000300"],
        ["F123"],
    )


def test_replace_message_attachments_batch_deletes_all_when_no_attachments():
    conn = FakeConn()
    row = shared.message_row(
        {"channel_id": "C123", "timestamp": "1770000000.000400"},
        "run_123",
    )

    asyncio.run(shared._replace_message_attachments_batch(conn, [row]))

    # No attachments => no upsert, but the message's attachments are still
    # reconciled (all removed) via the single delete with an empty keep set.
    assert conn.executemany_calls == []
    assert len(conn.execute_calls) == 1
    _delete_sql, delete_args = conn.execute_calls[0]
    assert delete_args == (["C123"], ["1770000000.000400"], [], [], [])


def test_upsert_messages_batches_writes_in_one_executemany():
    conn = FakeConn()
    pool = FakePool(conn)
    rows = [
        shared.message_row(
            {"channel_id": "C123", "timestamp": "1770000000.000300"}, "run_123"
        ),
        shared.message_row(
            {"channel_id": "C123", "timestamp": "1770000000.000400"}, "run_123"
        ),
    ]

    count = asyncio.run(shared.upsert_messages(pool, rows))

    assert count == 2
    message_calls = [
        call for call in conn.executemany_calls if "slack_sync_messages" in call[0]
    ]
    assert len(message_calls) == 1
    # Both rows upserted in a single batched statement, not one per row.
    assert len(message_calls[0][1]) == 2
    assert message_calls[0][1][0][0:2] == ("C123", "1770000000.000300")
    assert message_calls[0][1][1][0:2] == ("C123", "1770000000.000400")


def test_upsert_messages_dedupes_duplicate_message_keys_last_row_wins():
    conn = FakeConn()
    pool = FakePool(conn)
    rows = [
        shared.message_row(
            {
                "channel_id": "C123",
                "timestamp": "1770000000.000500",
                "text": "old",
                "files": [{"id": "F-old", "name": "old.txt"}],
            },
            "run_123",
        ),
        shared.message_row(
            {
                "channel_id": "C123",
                "timestamp": "1770000000.000500",
                "text": "new",
            },
            "run_123",
        ),
    ]

    count = asyncio.run(shared.upsert_messages(pool, rows))

    assert count == 2
    message_calls = [
        call for call in conn.executemany_calls if "slack_sync_messages" in call[0]
    ]
    assert len(message_calls) == 1
    assert len(message_calls[0][1]) == 1
    assert message_calls[0][1][0][0:2] == ("C123", "1770000000.000500")
    assert message_calls[0][1][0][10] == "new"

    attachment_calls = [
        call
        for call in conn.executemany_calls
        if "slack_sync_message_attachments" in call[0]
    ]
    assert attachment_calls == []
    assert len(conn.execute_calls) == 1
    _delete_sql, delete_args = conn.execute_calls[0]
    assert delete_args == (["C123"], ["1770000000.000500"], [], [], [])


def test_enqueue_backfill_job_refresh_guards():
    pool = FakeExecutePool()
    base = {
        "job_key": "continuation:C123:1770000000.000100:",
        "job_type": shared.BACKFILL_JOB_CHANNEL_CONTINUATION,
        "channel_id": "C123",
        "payload": {"cursor": "abc"},
        "run_id": "run_123",
    }

    asyncio.run(shared.enqueue_backfill_job(pool, **base))
    asyncio.run(shared.enqueue_backfill_job(pool, **base, refresh_completed=False))
    asyncio.run(shared.enqueue_backfill_job(pool, **base, refresh_pending=False))
    asyncio.run(
        shared.enqueue_backfill_job(
            pool, **base, refresh_completed=False, refresh_pending=False
        )
    )

    sqls = [sql for sql, _args in pool.execute_calls]
    assert len(sqls) == 4
    # Default: unconditional refresh (the backfill worker saving its progress).
    assert "WHERE slack_sync_backfill_jobs.status" not in sqls[0]
    assert sqls[1].endswith("WHERE slack_sync_backfill_jobs.status <> 'completed'")
    # refresh_pending=False must never touch in-flight pending/running rows.
    assert sqls[2].endswith(
        "WHERE slack_sync_backfill_jobs.status IN ('completed', 'failed')"
    )
    assert sqls[3].endswith("WHERE slack_sync_backfill_jobs.status = 'failed'")


class FakeClaimConn(FakeConn):
    def __init__(self) -> None:
        super().__init__()
        self.fetch_calls: list[tuple] = []

    async def fetch(self, sql, *args):
        self.fetch_calls.append((sql, args))
        return []


def test_claim_backfill_jobs_reclaims_stale_running_rows():
    conn = FakeClaimConn()
    pool = FakePool(conn)

    result = asyncio.run(shared.claim_backfill_jobs(pool, 5))

    assert result == []
    assert len(conn.fetch_calls) == 1
    sql = " ".join(conn.fetch_calls[0][0].split())
    assert "status IN ('pending', 'failed')" in sql
    assert (
        "status = 'running' AND last_started_at < "
        f"NOW() - INTERVAL '{shared.BACKFILL_JOB_STALE_RUNNING_HOURS} hours'"
    ) in sql


def test_touch_backfill_job_started_restamps_running_row():
    pool = FakeExecutePool()

    asyncio.run(shared.touch_backfill_job_started(pool, 42))

    assert len(pool.execute_calls) == 1
    sql, args = pool.execute_calls[0]
    assert "SET last_started_at = NOW()" in sql
    assert "status = 'running'" in sql
    assert args == (42,)


def test_sync_etl_channel_history_watermark_never_regresses(monkeypatch):
    client = object.__new__(shared.SlackEtlClient)

    def fake_page(**kwargs):
        return {
            "channel": "busy",
            "channel_id": "C123",
            "messages": [{"timestamp": "100.000000"}],
            "count": 1,
            "has_more": False,
            "next_cursor": None,
            "window": {
                "oldest": kwargs.get("oldest"),
                "latest": kwargs.get("latest"),
                "inclusive": True,
            },
            "order": "desc",
        }

    monkeypatch.setattr(
        client, "_get_etl_channel_history_page", lambda **kw: fake_page(**kw)
    )

    out = client._sync_etl_channel_history(
        "C123", state={"watermark": "200.000000"}, limit=10
    )

    # An oldest-anchored page below the prior watermark must not regress it.
    assert out["sync_state"]["watermark"] == "200.000000"


def test_backfill_handler_drains_continuation_pages_then_completes(monkeypatch):
    monkeypatch.setenv("SLACK_ETL_ENABLED", "true")
    monkeypatch.setenv("SLACK_BACKFILL_ENABLED", "true")
    backfill = _load_backfill()
    calls: dict[str, list] = {"enqueued": [], "completed": [], "upserted": []}

    class FakeClient:
        def __init__(self) -> None:
            self.history_calls: list[dict] = []

        def _etl_access_mode(self):
            return "test"

        def _sync_etl_channel_history(self, channel_id, *, state, limit, lookback_days):
            self.history_calls.append(dict(state))
            if state.get("cursor") == "cursor-0":
                return {
                    "messages": [
                        {
                            "channel_id": channel_id,
                            "timestamp": "1770000001.000100",
                            "text": "page one",
                        }
                    ],
                    "sync_state": {
                        "cursor": "cursor-1",
                        "watermark": "1770000001.000100",
                        "oldest": state.get("oldest"),
                        "latest": None,
                    },
                }
            return {
                "messages": [
                    {
                        "channel_id": channel_id,
                        "timestamp": "1770000002.000100",
                        "text": "page two",
                    }
                ],
                "sync_state": {
                    "cursor": None,
                    "watermark": "1770000002.000100",
                    "oldest": None,
                    "latest": None,
                },
            }

    class FakeContext:
        run_id = "wfr_drain"
        _pool = object()

        def __init__(self) -> None:
            self.logs: list[tuple] = []

        def log(self, name, **fields):
            self.logs.append((name, fields))

    async def fake_claim_jobs(_pool, _limit):
        return [
            {
                "job_id": 586,
                "job_key": "continuation:C123:1770000000.000100:",
                "job_type": shared.BACKFILL_JOB_CHANNEL_CONTINUATION,
                "payload_version": shared.BACKFILL_JOB_PAYLOAD_VERSION,
                "channel_id": "C123",
                "payload_json": {
                    "cursor": "cursor-0",
                    "oldest": "1770000000.000100",
                    "latest": None,
                    "lookback_days": 30,
                    "thread_lookback_days": 3,
                },
                "priority": 100,
                "attempt_count": 1,
            }
        ]

    async def fake_noop(*_args, **_kwargs):
        return None

    async def fake_upsert_messages(_pool, rows):
        calls["upserted"].extend(rows)
        return len(rows)

    async def fake_enqueue(_pool, **kwargs):
        calls["enqueued"].append(kwargs)

    async def fake_completed(_pool, **kwargs):
        calls["completed"].append(kwargs)

    fake_client = FakeClient()
    monkeypatch.setattr(backfill, "_emit_backfill_job_metrics", fake_noop)
    monkeypatch.setattr(backfill, "emit_slack_checkpoint_metrics", fake_noop)
    monkeypatch.setattr(backfill, "claim_backfill_jobs", fake_claim_jobs)
    monkeypatch.setattr(backfill, "touch_backfill_job_started", fake_noop)
    monkeypatch.setattr(backfill, "shared_client", lambda **_kwargs: fake_client)
    monkeypatch.setattr(backfill, "upsert_messages", fake_upsert_messages)
    monkeypatch.setattr(backfill, "enqueue_backfill_job", fake_enqueue)
    monkeypatch.setattr(backfill, "mark_backfill_job_completed", fake_completed)
    monkeypatch.setattr(backfill, "record_run_start", fake_noop)
    monkeypatch.setattr(backfill, "record_run_finish", fake_noop)

    result = asyncio.run(backfill.handler(backfill.Input(channel_batch_limit=1), FakeContext()))

    assert result["status"] == "completed"
    # The drain advanced through the cursor chain instead of refetching page one.
    assert [call.get("cursor") for call in fake_client.history_calls] == [
        "cursor-0",
        "cursor-1",
    ]
    assert len(calls["upserted"]) == 2
    # Exhausted cursor => the job completes; nothing requeues it.
    assert calls["enqueued"] == []
    assert len(calls["completed"]) == 1
    assert calls["completed"][0]["job_id"] == 586
    assert calls["completed"][0]["payload"]["cursor"] is None
