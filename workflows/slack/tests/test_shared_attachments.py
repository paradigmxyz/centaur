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


def test_replace_message_attachments_upserts_and_deletes_stale_rows():
    class FakeConn:
        def __init__(self) -> None:
            self.calls = []

        async def execute(self, sql, *args):
            self.calls.append((sql, args))

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
    count = asyncio.run(shared._replace_message_attachments(conn, row))

    assert count == 1
    assert len(conn.calls) == 2
    upsert_sql, upsert_args = conn.calls[0]
    delete_sql, delete_args = conn.calls[1]
    assert "INSERT INTO slack_sync_message_attachments" in upsert_sql
    assert upsert_args[0:4] == ("C123", "1770000000.000300", "F123", "report.txt")
    assert upsert_args[13] == b"hello"
    assert "NOT (slack_file_id = ANY" in delete_sql
    assert delete_args == ("C123", "1770000000.000300", ["F123"])
