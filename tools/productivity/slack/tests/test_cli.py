import base64
import sys
import types
from pathlib import Path

from slack.cli import _channel_arg_is_id, app
from typer.testing import CliRunner


def test_channel_arg_is_id_accepts_channel_id_forms() -> None:
    assert _channel_arg_is_id("C0AJ07U8Z1N")
    assert _channel_arg_is_id("#C0AJ07U8Z1N")
    assert _channel_arg_is_id("<#C0AJ07U8Z1N|eng-centaur>")


def test_channel_arg_is_id_rejects_names() -> None:
    assert not _channel_arg_is_id("eng-centaur")
    assert not _channel_arg_is_id("#eng-centaur")


def test_upload_requires_explicit_channel_and_thread(monkeypatch, tmp_path: Path) -> None:
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
            "upload",
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


def test_upload_rejects_channel_name(monkeypatch, tmp_path: Path) -> None:
    upload = tmp_path / "chart.png"
    upload.write_bytes(b"png")
    fake_client = types.SimpleNamespace(upload_file=lambda **_: {})
    monkeypatch.setitem(sys.modules, "slack.client", fake_client)

    result = CliRunner().invoke(
        app,
        ["upload", "#eng-ai", str(upload), "--thread", "1780000000.000000"],
    )

    assert result.exit_code == 1
    assert "must be a Slack conversation ID" in result.output


def test_upload_proxy_calls_proxy_client(monkeypatch, tmp_path: Path) -> None:
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
            "upload-proxy",
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


def test_download_proxy_writes_file(monkeypatch, tmp_path: Path) -> None:
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
        ["download-proxy", "F1234567890", "C1234567890", "--output", str(tmp_path)],
    )

    assert result.exit_code == 0
    assert calls == [{"file_id": "F1234567890", "channel_id": "C1234567890"}]
    assert (tmp_path / "report.pdf").read_bytes() == b"%PDF"
