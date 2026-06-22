import sys
import types
from pathlib import Path

from typer.testing import CliRunner

from slack.cli import _channel_arg_is_id, _upload_target_and_files, app


def test_channel_arg_is_id_accepts_channel_id_forms() -> None:
    assert _channel_arg_is_id("C0AJ07U8Z1N")
    assert _channel_arg_is_id("#C0AJ07U8Z1N")
    assert _channel_arg_is_id("<#C0AJ07U8Z1N|eng-centaur>")


def test_channel_arg_is_id_rejects_names() -> None:
    assert not _channel_arg_is_id("eng-centaur")
    assert not _channel_arg_is_id("#eng-centaur")


def test_upload_target_defaults_when_first_arg_is_file(tmp_path: Path) -> None:
    first = tmp_path / "chart.png"
    second = tmp_path / "table.csv"
    first.write_bytes(b"png")
    second.write_text("a,b\n1,2\n")

    channel, files = _upload_target_and_files(str(first), [str(second)])

    assert channel is None
    assert files == [str(first), str(second)]


def test_upload_target_treats_non_file_first_arg_as_channel(tmp_path: Path) -> None:
    upload = tmp_path / "chart.png"
    upload.write_bytes(b"png")

    channel, files = _upload_target_and_files("C123", [str(upload)])

    assert channel == "C123"
    assert files == [str(upload)]


def test_upload_target_single_missing_path_uses_default_context() -> None:
    channel, files = _upload_target_and_files("chart.png", [])

    assert channel is None
    assert files == ["chart.png"]


def test_upload_single_file_uses_default_context_without_files_arg(
    monkeypatch, tmp_path: Path
) -> None:
    upload = tmp_path / "chart.png"
    upload.write_bytes(b"png")
    calls = []

    def fake_upload_file(**kwargs):
        calls.append(kwargs)
        return {"permalink": "https://slack.example/files/chart.png"}

    fake_client = types.SimpleNamespace(upload_file=fake_upload_file)
    monkeypatch.setitem(sys.modules, "slack.client", fake_client)

    result = CliRunner().invoke(app, ["upload", str(upload), "--comment", "chart"])

    assert result.exit_code == 0
    assert calls == [
        {
            "channel": None,
            "content_base64": "cG5n",
            "filename": "chart.png",
            "title": "chart.png",
            "comment": "chart",
            "thread_ts": None,
        }
    ]
