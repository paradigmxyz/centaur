import base64
import json
import sys
import types
from pathlib import Path

import pytest
from slack.cli import _channel_arg_is_id, app
from typer.testing import CliRunner


@pytest.mark.parametrize("added", [True, False])
def test_react_calls_client_and_reports_result(monkeypatch, added) -> None:
    calls = []

    def fake_add_reaction(*args):
        calls.append(args)
        return {"added": added}

    monkeypatch.setitem(
        sys.modules, "slack.client", types.SimpleNamespace(add_reaction=fake_add_reaction)
    )
    result = CliRunner().invoke(app, ["react", "C1234567890", "123.000001", ":pencil2:"])

    assert result.exit_code == 0
    assert calls == [("C1234567890", "123.000001", ":pencil2:")]
    assert ("Reaction added" if added else "Reaction already present") in result.output


@pytest.mark.parametrize(
    "error", [RuntimeError("Slack API error: missing_scope"), ValueError("invalid emoji")]
)
def test_react_reports_failure(monkeypatch, error) -> None:
    def fake_add_reaction(*args):
        raise error

    monkeypatch.setitem(
        sys.modules, "slack.client", types.SimpleNamespace(add_reaction=fake_add_reaction)
    )
    result = CliRunner().invoke(app, ["react", "C1234567890", "123.456", "pencil2"])

    assert result.exit_code == 1
    assert str(error) in result.output
    assert "Reaction added" not in result.output


def test_react_is_discoverable() -> None:
    assert "react" in CliRunner().invoke(app, ["--help"]).output
    result = CliRunner().invoke(app, ["react", "--help"])
    assert result.exit_code == 0
    assert "reactions:write" in result.output


def test_channel_arg_is_id_accepts_channel_id_forms() -> None:
    assert _channel_arg_is_id("C0AJ07U8Z1N")
    assert _channel_arg_is_id("#C0AJ07U8Z1N")
    assert _channel_arg_is_id("<#C0AJ07U8Z1N|eng-centaur>")


def test_channel_arg_is_id_rejects_names() -> None:
    assert not _channel_arg_is_id("eng-centaur")
    assert not _channel_arg_is_id("#eng-centaur")


def test_channel_calls_proxy_client(monkeypatch) -> None:
    calls = []

    def fake_get_channel_history_proxy(*args, **kwargs):
        calls.append((args, kwargs))
        return {
            "ok": True,
            "messages": [{"user": "U123", "text": "root"}],
            "has_more": False,
            "response_metadata": {},
        }

    fake_client = types.SimpleNamespace(get_channel_history_proxy=fake_get_channel_history_proxy)
    monkeypatch.setitem(sys.modules, "slack.client", fake_client)

    result = CliRunner().invoke(
        app,
        [
            "channel",
            "C1234567890",
            "--limit",
            "10",
            "--cursor",
            "next",
            "--inclusive",
        ],
    )

    assert result.exit_code == 0
    assert json.loads(result.output)["messages"][0]["text"] == "root"
    assert calls == [
        (
            ("C1234567890",),
            {
                "cursor": "next",
                "include_all_metadata": None,
                "inclusive": True,
                "latest": None,
                "limit": 10,
                "oldest": None,
            },
        )
    ]


def test_channel_direct_calls_direct_client(monkeypatch) -> None:
    calls = []

    def fake_get_channel_history_page(*args, **kwargs):
        calls.append((args, kwargs))
        return {
            "channel": "C1234567890",
            "messages": [{"user": "alice", "text": "root"}],
            "has_more": False,
            "window": {"oldest": None, "latest": None, "inclusive": False},
        }

    fake_client = types.SimpleNamespace(get_channel_history_page=fake_get_channel_history_page)
    monkeypatch.setitem(sys.modules, "slack.client", fake_client)

    result = CliRunner().invoke(
        app,
        [
            "channel-direct",
            "C1234567890",
            "--limit",
            "10",
        ],
    )

    assert result.exit_code == 0
    assert json.loads(result.output)["messages"][0]["text"] == "root"
    assert calls == [
        (
            ("C1234567890",),
            {
                "limit": 10,
                "cursor": None,
                "oldest": None,
                "latest": None,
                "inclusive": False,
            },
        )
    ]


def test_channels_calls_proxy_client(monkeypatch) -> None:
    calls = []

    def fake_list_channels_proxy(*args, **kwargs):
        calls.append((args, kwargs))
        return [
            {
                "id": "C1234567890",
                "name": "general",
                "purpose": "Company",
                "topic": "",
                "member_count": 10,
                "is_private": False,
                "can_read_history": True,
                "can_upload": False,
                "can_download": True,
            }
        ]

    fake_client = types.SimpleNamespace(list_channels_proxy=fake_list_channels_proxy)
    monkeypatch.setitem(sys.modules, "slack.client", fake_client)

    result = CliRunner().invoke(
        app,
        ["channels", "--limit", "10", "--query", "general"],
    )

    assert result.exit_code == 0
    assert calls == [((), {"limit": 10, "query": "general"})]
    assert json.loads(result.output)[0]["name"] == "general"


def test_channels_direct_calls_direct_client(monkeypatch) -> None:
    calls = []

    def fake_list_channels(*args, **kwargs):
        calls.append(("list_channels", args, kwargs))
        return [
            {
                "id": "C1234567890",
                "name": "general",
                "purpose": "Company",
                "topic": "",
                "member_count": 10,
                "is_private": False,
            }
        ]

    def fake_list_bot_channels(*args, **kwargs):
        calls.append(("list_bot_channels", args, kwargs))
        return []

    fake_client = types.SimpleNamespace(
        list_channels=fake_list_channels,
        list_bot_channels=fake_list_bot_channels,
    )
    monkeypatch.setitem(sys.modules, "slack.client", fake_client)

    result = CliRunner().invoke(app, ["channels-direct", "--limit", "10"])

    assert result.exit_code == 0
    assert calls == [("list_channels", (), {"limit": 10, "query": None})]
    assert json.loads(result.output)[0]["name"] == "general"


def test_channels_direct_passes_query_before_limit(monkeypatch) -> None:
    calls = []

    def fake_list_channels(*args, **kwargs):
        calls.append((args, kwargs))
        return []

    fake_client = types.SimpleNamespace(
        list_channels=fake_list_channels,
        list_bot_channels=lambda **_: [],
    )
    monkeypatch.setitem(sys.modules, "slack.client", fake_client)

    result = CliRunner().invoke(
        app,
        ["channels-direct", "--query", "centaur", "--limit", "5"],
    )

    assert result.exit_code == 0
    assert calls == [((), {"limit": 5, "query": "centaur"})]


def test_channel_members_calls_proxy_client(monkeypatch) -> None:
    calls = []

    def fake_get_channel_members_proxy(*args, **kwargs):
        calls.append((args, kwargs))
        return [{"id": "U123456789", "name": "alice"}]

    fake_client = types.SimpleNamespace(get_channel_members_proxy=fake_get_channel_members_proxy)
    monkeypatch.setitem(sys.modules, "slack.client", fake_client)

    result = CliRunner().invoke(
        app,
        ["channel-members", "C1234567890", "--limit", "25"],
    )

    assert result.exit_code == 0
    assert calls == [(("C1234567890",), {"limit": 25})]
    assert json.loads(result.output) == [{"id": "U123456789", "name": "alice"}]


def test_channel_members_direct_calls_direct_client(monkeypatch) -> None:
    calls = []

    def fake_get_channel_members(*args, **kwargs):
        calls.append((args, kwargs))
        return [{"id": "U123456789", "name": "alice"}]

    fake_client = types.SimpleNamespace(get_channel_members=fake_get_channel_members)
    monkeypatch.setitem(sys.modules, "slack.client", fake_client)

    result = CliRunner().invoke(app, ["channel-members-direct", "eng-ai"])

    assert result.exit_code == 0
    assert calls == [(("eng-ai",), {})]
    assert json.loads(result.output) == [{"id": "U123456789", "name": "alice"}]


def test_search_files_calls_proxy_client(monkeypatch) -> None:
    calls = []

    def fake_search_files(*args, **kwargs):
        calls.append((args, kwargs))
        return [
            {
                "id": "F1234567890",
                "name": "report.pdf",
                "title": "Report",
                "filetype": "pdf",
                "size": 1234,
                "user": "alice",
                "channels": ["C1234567890"],
                "permalink": "https://slack.example/files/F1234567890",
                "url_private": "https://files.example/F1234567890",
                "created": 1700000000,
            }
        ]

    fake_client = types.SimpleNamespace(search_files=fake_search_files)
    monkeypatch.setitem(sys.modules, "slack.client", fake_client)

    result = CliRunner().invoke(
        app,
        ["search-files", "C1234567890", "report", "--limit", "10"],
    )

    assert result.exit_code == 0
    assert calls == [(("C1234567890", "report"), {"max_results": 10})]
    payload = json.loads(result.output)
    assert payload["count"] == 1
    assert payload["results"][0]["name"] == "report.pdf"


def test_search_files_direct_calls_direct_client(monkeypatch) -> None:
    calls = []

    def fake_search_files_direct(*args, **kwargs):
        calls.append((args, kwargs))
        return [
            {
                "id": "F1234567890",
                "name": "report.pdf",
                "title": "Report",
                "filetype": "pdf",
                "size": 1234,
                "user": "alice",
                "channels": ["C1234567890"],
                "permalink": "https://slack.example/files/F1234567890",
                "url_private": "https://files.example/F1234567890",
                "created": 1700000000,
            }
        ]

    fake_client = types.SimpleNamespace(search_files_direct=fake_search_files_direct)
    monkeypatch.setitem(sys.modules, "slack.client", fake_client)

    result = CliRunner().invoke(
        app,
        ["search-files-direct", "report", "--limit", "10"],
    )

    assert result.exit_code == 0
    assert calls == [(("report",), {"max_results": 10})]
    payload = json.loads(result.output)
    assert payload["count"] == 1
    assert payload["results"][0]["name"] == "report.pdf"


def test_upload_direct_requires_explicit_channel_and_thread(monkeypatch, tmp_path: Path) -> None:
    upload = tmp_path / "chart.png"
    upload.write_bytes(b"png")

    fake_client = types.SimpleNamespace(upload_file=lambda **_: {})
    monkeypatch.setitem(sys.modules, "slack.client", fake_client)

    result = CliRunner().invoke(
        app,
        ["upload-direct", "C1234567890", str(upload)],
    )

    assert result.exit_code != 0


def test_upload_direct_calls_direct_client(monkeypatch, tmp_path: Path) -> None:
    upload = tmp_path / "chart.png"
    upload.write_bytes(b"png")
    calls = []

    def fake_upload_file(**kwargs):
        calls.append(kwargs)
        return {"permalink": "https://slack.example/files/chart.png"}

    fake_client = types.SimpleNamespace(upload_file=fake_upload_file)
    monkeypatch.setitem(sys.modules, "slack.client", fake_client)

    result = CliRunner().invoke(
        app,
        [
            "upload-direct",
            "C1234567890",
            str(upload),
            "--thread",
            "1780000000.000000",
            "--comment",
            "chart",
        ],
    )

    assert result.exit_code == 0
    assert calls == [
        {
            "channel": "C1234567890",
            "content_base64": "cG5n",
            "filename": "chart.png",
            "title": "chart.png",
            "comment": "chart",
            "thread_ts": "1780000000.000000",
        }
    ]


def test_upload_rejects_file_only_form(tmp_path: Path) -> None:
    upload = tmp_path / "chart.png"
    upload.write_bytes(b"png")

    result = CliRunner().invoke(app, ["upload", str(upload)])

    assert result.exit_code != 0


def test_upload_direct_rejects_channel_name(monkeypatch, tmp_path: Path) -> None:
    upload = tmp_path / "chart.png"
    upload.write_bytes(b"png")
    fake_client = types.SimpleNamespace(upload_file=lambda **_: {})
    monkeypatch.setitem(sys.modules, "slack.client", fake_client)

    result = CliRunner().invoke(
        app,
        ["upload-direct", "#eng-ai", str(upload), "--thread", "1780000000.000000"],
    )

    assert result.exit_code == 1
    assert "upload-direct channel must be a Slack conversation ID" in result.output


def test_upload_calls_proxy_client(monkeypatch, tmp_path: Path) -> None:
    upload = tmp_path / "chart.png"
    upload.write_bytes(b"png")
    calls = []

    def fake_upload_file_proxy(**kwargs):
        calls.append(kwargs)
        return {"file_id": "F1234567890"}

    fake_client = types.SimpleNamespace(upload_file_proxy=fake_upload_file_proxy)
    monkeypatch.setitem(sys.modules, "slack.client", fake_client)

    result = CliRunner().invoke(
        app,
        [
            "upload",
            "C1234567890",
            str(upload),
            "--thread",
            "1780000000.000000",
            "--comment",
            "chart",
            "--content-type",
            "image/png",
            "--alt-text",
            "chart alt",
        ],
    )

    assert result.exit_code == 0
    assert calls == [
        {
            "channel_id": "C1234567890",
            "content_base64": "cG5n",
            "filename": "chart.png",
            "title": "chart.png",
            "initial_comment": "chart",
            "thread_ts": "1780000000.000000",
            "content_type": "image/png",
            "alt_txt": "chart alt",
            "snippet_type": None,
        }
    ]


def test_download_direct_resolves_workspace_file_permalink(monkeypatch, tmp_path: Path) -> None:
    calls = []

    def fake_get_file_info_direct(file_id):
        calls.append(file_id)
        return {
            "name": "report.pdf",
            "url_private_download": "https://files.slack.com/files-pri/T1-F1/report.pdf",
        }

    def fake_fetch_slack_file(url):
        calls.append(url)
        return "report.pdf", "application/pdf", b"report"

    fake_client = types.SimpleNamespace(
        get_file_info_direct=fake_get_file_info_direct,
        get_message_files=lambda *_args, **_kwargs: [],
        _fetch_slack_file=fake_fetch_slack_file,
    )
    monkeypatch.setitem(sys.modules, "slack.client", fake_client)

    result = CliRunner().invoke(
        app,
        [
            "download-direct",
            "https://paradigm-ops.slack.com/files/U123/F123/report.pdf",
            "--output",
            str(tmp_path),
        ],
    )

    assert result.exit_code == 0
    assert calls == [
        "F123",
        "https://files.slack.com/files-pri/T1-F1/report.pdf",
    ]
    assert (tmp_path / "report.pdf").read_bytes() == b"report"


def test_download_writes_file_with_proxy(monkeypatch, tmp_path: Path) -> None:
    calls = []

    def fake_download_file_proxy(**kwargs):
        calls.append(kwargs)
        return {
            "filename": "report.pdf",
            "content_base64": base64.b64encode(b"%PDF").decode(),
            "size_bytes": 4,
        }

    fake_client = types.SimpleNamespace(download_file_proxy=fake_download_file_proxy)
    monkeypatch.setitem(sys.modules, "slack.client", fake_client)

    result = CliRunner().invoke(
        app,
        ["download", "F1234567890", "C1234567890", "--output", str(tmp_path)],
    )

    assert result.exit_code == 0
    assert calls == [{"file_id": "F1234567890", "channel_id": "C1234567890"}]
    assert (tmp_path / "report.pdf").read_bytes() == b"%PDF"
    assert json.loads(result.output)["output_path"] == str((tmp_path / "report.pdf").absolute())


def test_file_info_calls_proxy_client(monkeypatch) -> None:
    calls = []

    def fake_file_info_proxy(**kwargs):
        calls.append(kwargs)
        return {
            "ok": True,
            "file_id": "F1234567890",
            "channel_id": "C1234567890",
            "file": {
                "id": "F1234567890",
                "name": "report.pdf",
                "filetype": "pdf",
                "size": 1234,
            },
        }

    fake_client = types.SimpleNamespace(file_info_proxy=fake_file_info_proxy)
    monkeypatch.setitem(sys.modules, "slack.client", fake_client)

    result = CliRunner().invoke(
        app,
        ["file-info", "F1234567890", "C1234567890"],
    )

    assert result.exit_code == 0
    assert calls == [{"file_id": "F1234567890", "channel_id": "C1234567890"}]
    payload = json.loads(result.output)
    assert payload["file"]["name"] == "report.pdf"
    assert payload["file"]["size"] == 1234


def test_thread_calls_api_server_client(monkeypatch) -> None:
    calls = []

    def fake_get_thread_replies_proxy(*args, **kwargs):
        calls.append((args, kwargs))
        return {
            "ok": True,
            "messages": [{"user": "U123", "text": "root"}],
            "has_more": False,
        }

    def fake_get_thread_replies_page(*args, **kwargs):
        raise AssertionError("direct thread fallback should not be used")

    fake_client = types.SimpleNamespace(
        get_thread_replies_page=fake_get_thread_replies_page,
        get_thread_replies_proxy=fake_get_thread_replies_proxy,
    )
    monkeypatch.setitem(sys.modules, "slack.client", fake_client)

    result = CliRunner().invoke(
        app,
        [
            "thread",
            "C1234567890:1780000000.000000",
            "--limit",
            "10",
            "--cursor",
            "next",
        ],
    )

    assert result.exit_code == 0
    assert calls == [
        (
            ("C1234567890", "1780000000.000000"),
            {
                "limit": 10,
                "cursor": "next",
                "oldest": None,
                "latest": None,
                "inclusive": True,
            },
        )
    ]


def test_help_explains_channel_and_dm_access_paths() -> None:
    result = CliRunner().invoke(app, ["--help"])

    assert result.exit_code == 0
    assert "two access paths" in result.output
    assert "Proxied commands omit" in result.output
    assert "Slack channel chat surfaces" in result.output
    assert "Direct commands end" in result.output
    assert "actual user token" in result.output
    assert "including in Slack DM chat surfaces and MCP" in result.output
    assert (
        "inside a Slack DM, upload files with regular `upload`, not `upload-direct`"
        in " ".join(result.output.split())
    )
    assert "surface and credential context" in result.output
    assert "search-direct" in result.output


def test_search_uses_indexed_slack_client(monkeypatch) -> None:
    calls = []

    class FakeIndexedSlackClient:
        def search_messages(self, **kwargs):
            calls.append(kwargs)
            return {
                "status": "ok",
                "results": [
                    {
                        "channel": "eng-infra",
                        "user": "alice",
                        "text": "database migration completed",
                        "permalink": "https://example.slack.com/archives/C123/p123",
                    }
                ],
            }

    monkeypatch.setitem(
        sys.modules,
        "slack.client",
        types.SimpleNamespace(IndexedSlackClient=FakeIndexedSlackClient),
    )

    result = CliRunner().invoke(
        app,
        [
            "search",
            "database migration",
            "--channels",
            "eng-infra,C1234567890",
            "--from",
            "alice",
            "--limit",
            "5",
        ],
    )

    assert result.exit_code == 0
    assert json.loads(result.output)["results"][0]["text"] == "database migration completed"
    assert calls == [
        {
            "query": "database migration",
            "limit": 5,
            "channels": ["eng-infra", "C1234567890"],
            "from_user": "alice",
        }
    ]


def test_search_direct_calls_native_search_client(monkeypatch) -> None:
    calls = []

    def fake_search_messages_direct(*args, **kwargs):
        calls.append((args, kwargs))
        return [
            {
                "channel": "eng-infra",
                "user": "alice",
                "text": "deploy completed",
                "permalink": "https://example.slack.com/archives/C123/p123",
            }
        ]

    fake_client = types.SimpleNamespace(search_messages_direct=fake_search_messages_direct)
    monkeypatch.setitem(sys.modules, "slack.client", fake_client)

    result = CliRunner().invoke(
        app,
        [
            "search-direct",
            "deploy",
            "--channels",
            "eng-infra,C1234567890",
            "--from",
            "alice",
            "--limit",
            "5",
        ],
    )

    assert result.exit_code == 0
    payload = json.loads(result.output)
    assert payload["count"] == 1
    assert payload["results"][0]["text"] == "deploy completed"
    assert calls == [
        (
            ("deploy",),
            {
                "max_results": 5,
                "channels": ["eng-infra", "C1234567890"],
                "from_user": "alice",
            },
        )
    ]


def test_thread_does_not_fall_back_when_api_server_fails(monkeypatch) -> None:
    proxy_calls = []

    def fake_get_thread_replies_proxy(*args, **kwargs):
        proxy_calls.append((args, kwargs))
        raise RuntimeError("proxy unavailable")

    fake_client = types.SimpleNamespace(get_thread_replies_proxy=fake_get_thread_replies_proxy)
    monkeypatch.setitem(sys.modules, "slack.client", fake_client)

    result = CliRunner().invoke(
        app,
        ["thread", "C1234567890:1780000000.000000", "--limit", "10"],
    )

    assert result.exit_code == 1
    assert len(proxy_calls) == 1
    assert "proxy unavailable" in result.output


def test_thread_direct_calls_direct_client(monkeypatch) -> None:
    calls = []

    def fake_get_thread_replies_page(*args, **kwargs):
        calls.append((args, kwargs))
        return {
            "messages": [{"user": "alice", "text": "root"}],
            "has_more": False,
            "window": {"oldest": None, "latest": None, "inclusive": True},
        }

    fake_client = types.SimpleNamespace(get_thread_replies_page=fake_get_thread_replies_page)
    monkeypatch.setitem(sys.modules, "slack.client", fake_client)

    result = CliRunner().invoke(
        app,
        [
            "thread-direct",
            "C1234567890:1780000000.000000",
            "--limit",
            "10",
        ],
    )

    assert result.exit_code == 0
    assert calls == [
        (
            ("C1234567890", "1780000000.000000"),
            {
                "limit": 10,
                "cursor": None,
                "oldest": None,
                "latest": None,
                "inclusive": True,
            },
        )
    ]
