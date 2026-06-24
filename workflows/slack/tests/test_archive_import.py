from __future__ import annotations

import asyncio
import importlib
import io
import json
import sys
import types
import urllib.request
import zipfile
from pathlib import Path


def _load_archive_import():
    repo_root = Path(__file__).resolve().parents[3]
    if str(repo_root) not in sys.path:
        sys.path.insert(0, str(repo_root))

    api_module = sys.modules.setdefault("api", types.ModuleType("api"))

    runtime_control = types.ModuleType("api.runtime_control")
    runtime_control.canonical_json = lambda value: json.dumps(value, sort_keys=True)
    api_module.runtime_control = runtime_control
    sys.modules["api.runtime_control"] = runtime_control

    workflow_engine = types.ModuleType("api.workflow_engine")
    workflow_engine.WorkflowContext = object
    api_module.workflow_engine = workflow_engine
    sys.modules["api.workflow_engine"] = workflow_engine

    vm_metrics = types.ModuleType("api.vm_metrics")
    for name in (
        "record_slack_etl_rate_limit",
        "set_etl_active_scopes",
        "set_etl_failed_scopes",
        "set_etl_scope_sync_freshness_seconds",
    ):
        setattr(vm_metrics, name, lambda *_args, **_kwargs: None)
    api_module.vm_metrics = vm_metrics
    sys.modules["api.vm_metrics"] = vm_metrics

    centaur_sdk = sys.modules.setdefault("centaur_sdk", types.ModuleType("centaur_sdk"))
    centaur_sdk.secret = lambda _name, default=None: default

    return importlib.import_module("workflows.slack.archive_import")


archive_import = _load_archive_import()


class FakeConn:
    def __init__(self) -> None:
        self.executemany_calls: list[tuple[str, list[tuple]]] = []

    async def executemany(self, sql, args_list):
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
    def __init__(self) -> None:
        self.conn = FakeConn()

    def acquire(self):
        conn = self.conn

        class _Acquire:
            async def __aenter__(self_):
                return conn

            async def __aexit__(self_, *_exc):
                return False

        return _Acquire()


def _write_dummy_archive(path: Path) -> None:
    channels = [
        {
            "id": "CENG",
            "name": "eng-test",
            "is_archived": False,
            "topic": {"value": "Archive topic"},
            "purpose": {"value": "Archive purpose"},
            "members": ["U123", "U234"],
        },
        {
            "id": "CJP",
            "name": "日本語",
            "is_archived": True,
            "topic": {"value": ""},
            "purpose": {"value": ""},
            "members": [],
        },
    ]
    users = [
        {
            "id": "U123",
            "team_id": "T123",
            "name": "alice",
            "real_name": "Alice Example",
            "deleted": False,
            "is_bot": False,
            "profile": {"display_name": "alice"},
        },
        {
            "id": "BUSER",
            "team_id": "T123",
            "name": "buildkite",
            "real_name": "Build Kite",
            "deleted": False,
            "is_bot": True,
            "profile": {"display_name": "buildkite"},
        },
    ]
    messages = [
        {
            "user": "U123",
            "type": "message",
            "ts": "1770000000.000100",
            "text": "thread root",
            "thread_ts": "1770000000.000100",
            "reply_count": 1,
            "reply_users": ["U234"],
            "reply_users_count": 1,
            "latest_reply": "1770000001.000200",
            "replies": [{"user": "U234", "ts": "1770000001.000200"}],
            "blocks": [{"type": "rich_text"}],
        },
        {
            "user": "U234",
            "type": "message",
            "ts": "1770000001.000200",
            "text": "thread reply",
            "thread_ts": "1770000000.000100",
            "parent_user_id": "U123",
            "blocks": [{"type": "rich_text"}],
        },
        {
            "type": "message",
            "user": "U123",
            "upload": False,
            "ts": "1770000003.000000",
            "text": "edited text",
            "subtype": "message_changed",
            "thread_ts": "1770000002.000300",
            "original": {
                "user": "U123",
                "type": "message",
                "ts": "1770000002.000300",
                "text": "old text",
                "thread_ts": "1770000002.000300",
            },
            "edited_by": "U123",
        },
        {
            "type": "message",
            "user": "U123",
            "upload": False,
            "ts": "1770000004.000000",
            "text": "",
            "subtype": "message_deleted",
            "thread_ts": "0000000000.000000",
            "original": {
                "user": "U123",
                "type": "message",
                "ts": "1770000004.000400",
                "text": "delete me",
            },
            "deleted_by": "U123",
        },
        {
            "user": "U123",
            "type": "message",
            "ts": "1770000005.000500",
            "text": "uploaded a file",
            "files": [
                {
                    "id": "F123",
                    "name": "diagram.png",
                    "title": "Diagram",
                    "mimetype": "image/png",
                    "filetype": "png",
                    "size": 1234,
                    "url_private": (
                        "https://files.slack.com/files-pri/T123-F123/diagram.png"
                        "?token=xoxe-secret"
                    ),
                    "url_private_download": (
                        "https://files.slack.com/files-pri/T123-F123/download/diagram.png"
                        "?token=xoxe-secret"
                    ),
                    "permalink": "https://example.slack.com/files/U123/F123/diagram.png",
                }
            ],
        },
        {
            "user": "U123",
            "type": "message",
            "ts": "1770000006.000600",
            "text": "link unfurl",
            "attachments": [
                {
                    "id": 1,
                    "from_url": "https://example.com/post",
                    "title": "Unfurl",
                    "text": "This is not a Slack file object.",
                }
            ],
        },
        {
            "subtype": "bot_message",
            "text": "build passed",
            "username": "CI",
            "type": "message",
            "ts": "1770000007.000700",
            "bot_id": "B123",
            "app_id": "A123",
            "blocks": [{"type": "section"}],
        },
    ]
    unknown_messages = [
        {
            "user": "U123",
            "type": "message",
            "ts": "1770000008.000800",
            "text": "unknown channel dir",
        }
    ]

    with zipfile.ZipFile(path, "w") as zip_file:
        zip_file.writestr("channels.json", json.dumps(channels))
        zip_file.writestr("users.json", json.dumps(users))
        zip_file.writestr("integration_logs.json", "[]")
        zip_file.writestr("eng-test/2026-06-12.json", json.dumps(messages))
        zip_file.writestr("garbled-name/2026-06-12.json", json.dumps(unknown_messages))


def _executemany_calls_for(
    pool: FakePool, table_name: str
) -> list[tuple[str, list[tuple]]]:
    return [
        call
        for call in pool.conn.executemany_calls
        if f"INSERT INTO {table_name}" in call[0]
    ]


def test_import_archive_path_parses_public_export_shape_and_edges(
    tmp_path, monkeypatch
):
    archive_path = tmp_path / "dummy-slack-export.zip"
    _write_dummy_archive(archive_path)
    pool = FakePool()
    run_starts = []
    run_finishes = []

    async def fake_record_run_start(_pool, **kwargs):
        run_starts.append(kwargs)

    async def fake_record_run_finish(_pool, **kwargs):
        run_finishes.append(kwargs)

    monkeypatch.setattr(archive_import, "record_run_start", fake_record_run_start)
    monkeypatch.setattr(archive_import, "record_run_finish", fake_record_run_finish)

    counts = asyncio.run(
        archive_import.import_archive_path(
            pool,
            archive_path,
            import_id="sai_dummy",
            workflow_run_id="wfr_dummy",
        )
    )

    assert counts == {
        "channels_imported": 2,
        "users_imported": 2,
        "messages_imported": 6,
        "messages_skipped": 1,
        "message_files_skipped": 1,
    }
    assert run_starts[0]["mode"] == "archive_import"
    assert run_starts[0]["run_id"] == "slack_archive_sai_dummy"
    assert run_finishes[0]["status"] == "completed"
    assert run_finishes[0]["counts"]["messages_upserted"] == 6

    channel_args = _executemany_calls_for(pool, "slack_sync_channels")[0][1]
    assert channel_args[0][0:6] == (
        "CENG",
        "eng-test",
        False,
        "Archive topic",
        "Archive purpose",
        2,
    )

    user_args = _executemany_calls_for(pool, "slack_sync_users")[0][1]
    assert user_args[0][0:4] == ("U123", "alice", "Alice Example", "alice")

    message_args = _executemany_calls_for(pool, "slack_sync_messages")[0][1]
    message_ts_values = {args[1] for args in message_args}
    assert "1770000002.000300" in message_ts_values
    assert "1770000004.000400" not in message_ts_values

    changed_args = next(args for args in message_args if args[1] == "1770000002.000300")
    assert changed_args[10] == "edited text"
    assert changed_args[9] == "message_changed"

    file_args = next(args for args in message_args if args[1] == "1770000005.000500")
    raw_payload = json.loads(file_args[15])
    assert raw_payload["files"][0]["url_private"].endswith("/diagram.png")
    assert "token=" not in raw_payload["files"][0]["url_private"]
    assert "token=" not in raw_payload["files"][0]["url_private_download"]

    attachment_args = _executemany_calls_for(pool, "slack_sync_message_attachments")[0][
        1
    ]
    assert len(attachment_args) == 1
    assert attachment_args[0][2] == "F123"
    assert attachment_args[0][8].endswith("/diagram.png")
    assert "token=" not in attachment_args[0][8]


def test_archive_upsert_sql_preserves_live_fields():
    message_update = archive_import._MESSAGE_UPSERT_SQL.split(
        "ON CONFLICT (channel_id, message_ts) DO UPDATE SET ",
        1,
    )[1]
    attachment_update = archive_import._ATTACHMENT_UPSERT_SQL.split(
        "ON CONFLICT (channel_id, message_ts, slack_file_id) DO UPDATE SET ",
        1,
    )[1]

    assert "permalink = CASE WHEN slack_sync_messages.permalink = ''" in message_update
    assert (
        "source_run_id = COALESCE(slack_sync_messages.source_run_id" in message_update
    )
    assert "thread_refreshed_at" not in message_update
    assert "content_bytes =" not in attachment_update
    assert "content_sha256 =" not in attachment_update
    assert "download_status = CASE WHEN" in attachment_update


def test_request_archive_download_url_uses_api_presign_endpoint(monkeypatch):
    opened_requests = []

    class FakeResponse(io.BytesIO):
        def __enter__(self):
            return self

        def __exit__(self, *_exc):
            return False

    def fake_urlopen(request, timeout):
        opened_requests.append((request, timeout))
        return FakeResponse(
            json.dumps(
                {
                    "ok": True,
                    "download": {
                        "download_url": "https://r2.example/presigned-download"
                    },
                }
            ).encode()
        )

    monkeypatch.setenv("CENTAUR_API_URL", "http://centaur-api-rs:8080/")
    monkeypatch.setattr(urllib.request, "urlopen", fake_urlopen)

    download_url = archive_import._request_archive_download_url("sai id/with/slash")

    assert download_url == "https://r2.example/presigned-download"
    request, timeout = opened_requests[0]
    assert timeout == 30
    assert request.get_method() == "POST"
    assert (
        request.full_url
        == "http://centaur-api-rs:8080/api/admin/slack/archive-imports/"
        "sai%20id%2Fwith%2Fslash/download-url"
    )


def test_download_archive_streams_api_presigned_url(tmp_path, monkeypatch):
    calls = []

    def fake_request_archive_download_url(import_id):
        calls.append(("presign", import_id))
        return "https://r2.example/presigned-download"

    def fake_download_url_to_path(download_url, destination):
        calls.append(("download", download_url, destination))
        destination.write_bytes(b"zip bytes")

    monkeypatch.setattr(
        archive_import,
        "_request_archive_download_url",
        fake_request_archive_download_url,
    )
    monkeypatch.setattr(
        archive_import,
        "_download_url_to_path",
        fake_download_url_to_path,
    )

    destination = tmp_path / "archive.zip"
    asyncio.run(
        archive_import._download_archive({"import_id": "sai_dummy"}, destination)
    )

    assert calls == [
        ("presign", "sai_dummy"),
        ("download", "https://r2.example/presigned-download", destination),
    ]
    assert destination.read_bytes() == b"zip bytes"
