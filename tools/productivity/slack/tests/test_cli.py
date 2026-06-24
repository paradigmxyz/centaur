from pathlib import Path

from slack.cli import _channel_arg_is_id, _upload_target_and_files


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
