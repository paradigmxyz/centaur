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

    centaur_sdk = types.ModuleType("centaur_sdk")
    centaur_sdk.secret = lambda _name, default=None: default
    sys.modules.setdefault("centaur_sdk", centaur_sdk)

    return importlib.import_module("workflows.slack.shared")


def _load_backfill():
    api_module = sys.modules.setdefault("api", types.ModuleType("api"))

    vm_metrics = types.ModuleType("api.vm_metrics")
    for name in (
        "record_etl_items_deleted",
        "record_etl_items_enqueued",
        "record_etl_items_failed",
        "record_etl_items_seen",
        "record_etl_items_upserted",
        "set_etl_backfill_job_age_seconds",
        "set_etl_backfill_jobs",
    ):
        setattr(vm_metrics, name, lambda *_args, **_kwargs: None)
    api_module.vm_metrics = vm_metrics
    sys.modules["api.vm_metrics"] = vm_metrics

    workflow_engine = types.ModuleType("api.workflow_engine")

    class WorkflowContext:
        pass

    workflow_engine.WorkflowContext = WorkflowContext
    api_module.workflow_engine = workflow_engine
    sys.modules["api.workflow_engine"] = workflow_engine

    return importlib.import_module("workflows.slack.backfill")


shared = _load_shared()


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
    monkeypatch.setattr(backfill, "claim_backfill_jobs", fake_claim_jobs)
    monkeypatch.setattr(backfill, "shared_client", lambda: FakeClient())
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
