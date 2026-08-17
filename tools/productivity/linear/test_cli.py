"""Tests for the Linear CLI's label listing.

Run from this directory:
    uv run --no-project --with pytest --with typer --with rich pytest test_cli.py
"""

from __future__ import annotations

import importlib.util
import inspect
import json
from pathlib import Path
import sys
import tomllib
from typing import Any

import pytest

# Record the rows added to Rich's table without depending on its rendering.
RECORDED_ROWS: list[tuple[str, ...]] = []


class RecordingTable:
    def __init__(self, *args: Any, **kwargs: Any) -> None:
        pass

    def add_column(self, *args: Any, **kwargs: Any) -> None:
        pass

    def add_row(self, *cells: str) -> None:
        RECORDED_ROWS.append(cells)

spec = importlib.util.spec_from_file_location(
    "linear_cli", Path(__file__).with_name("cli.py")
)
assert spec and spec.loader
cli = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cli)

sys.path.insert(0, str(Path(__file__).parent))
import uploads as uploads_module


class FakeClient:
    def __init__(self, labels: list[dict[str, Any]]) -> None:
        self._labels = labels

    def labels(self, team_key: str | None = None) -> list[dict[str, Any]]:
        return self._labels


def _run_labels(monkeypatch, labels: list[dict[str, Any]]):
    from typer.testing import CliRunner

    RECORDED_ROWS.clear()
    monkeypatch.setattr(cli, "Table", RecordingTable)
    monkeypatch.setattr(cli, "get_client", lambda: FakeClient(labels))
    return CliRunner().invoke(cli.app, ["labels"])


def test_labels_renders_org_wide_label_without_crashing(monkeypatch):
    # An org-wide label arrives with team explicitly None (PE-7945 repro).
    result = _run_labels(
        monkeypatch,
        [
            {"name": "team-bug", "team": {"key": "PE"}},
            {"name": "org-wide", "team": None},
        ],
    )

    assert result.exit_code == 0, result.output
    assert ("org", "org-wide") in RECORDED_ROWS
    assert ("PE", "team-bug") in RECORDED_ROWS


def test_labels_handles_missing_team_key(monkeypatch):
    # Defensive: a label with no team key at all must also not crash.
    result = _run_labels(monkeypatch, [{"name": "loose"}])

    assert result.exit_code == 0, result.output
    assert ("org", "loose") in RECORDED_ROWS


SIGNED_URL = "https://uploads.linear.app/upload?signature=do-not-print"
PROVIDER_HEADER = "x-provider-secret=do-not-print"
TOKEN = "linear-token-do-not-print"
FILE_BYTES = "raw-file-bytes-do-not-print"
PARTIAL_ERROR = "Linear evidence uploaded, but comment creation failed"
SAFE_UPLOAD_RESULT = {
    "ok": True,
    "tool": TOKEN,
    "issue_id": TOKEN,
    "asset_url": "https://uploads.linear.app/assets/evidence.png",
    "filename": "evidence.png",
    "mime_type": "image/png",
    "size_bytes": 123,
    "comment_id": "123e4567-e89b-12d3-a456-426614174000",
}
GENERIC_UPLOAD_FAILURE = {
    "ok": False,
    "tool": "linear",
    "stage": "upload",
    "error": "Linear evidence upload failed",
}

class FakeUploadClient:
    def __init__(
        self,
        result: dict[str, Any] | None = None,
        error: Exception | None = None,
        close_error: Exception | None = None,
    ):
        self.result = result
        self.error = error
        self.close_error = close_error
        self.calls: list[tuple[str, str, str | None]] = []
        self.close_calls = 0

    def upload_evidence(
        self, issue_id: str, path: str, comment: str | None = None
    ) -> dict[str, Any]:
        self.calls.append((issue_id, path, comment))
        if self.error:
            raise self.error
        assert self.result is not None
        return self.result

    def close(self) -> None:
        self.close_calls += 1
        if self.close_error:
            raise self.close_error


class UnsafeUploadFailure(RuntimeError):
    stage = "upload"


class UnsafeValidationFailure(RuntimeError):
    stage = "validation"


def _run_upload(monkeypatch, client: FakeUploadClient, *args: str):
    from typer.testing import CliRunner

    monkeypatch.setattr(cli, "get_client", lambda: client)
    return CliRunner().invoke(cli.app, ["upload", *args])


def _run_upload_with_construction_error(monkeypatch, error: Exception, *args: str):
    from typer.testing import CliRunner

    def fail_construction():
        raise error

    monkeypatch.setattr(cli, "get_client", fail_construction)
    return CliRunner().invoke(cli.app, ["upload", *args])


def _all_output(result) -> str:
    return result.output + getattr(result, "stderr", "")


def _assert_no_unsafe_output(result) -> None:
    output = _all_output(result)
    for unsafe in (SIGNED_URL, PROVIDER_HEADER, TOKEN, FILE_BYTES):
        assert unsafe not in output


def test_upload_json_success_has_exact_safe_shape(monkeypatch):
    client = FakeUploadClient(result=SAFE_UPLOAD_RESULT)

    result = _run_upload(
        monkeypatch,
        client,
        "NEU-497",
        "/home/agent/uploads/evidence.png",
        "--comment",
        "Evidence",
        "--json",
    )

    assert result.exit_code == 0, result.output
    assert json.loads(result.stdout) == {
        **SAFE_UPLOAD_RESULT,
        "tool": "linear",
        "issue_id": "NEU-497",
    }
    assert client.calls == [
        ("NEU-497", "/home/agent/uploads/evidence.png", "Evidence")
    ]
    assert client.close_calls == 1
    _assert_no_unsafe_output(result)


def test_upload_json_success_discards_undocumented_sensitive_fields(monkeypatch):
    unsafe_result = {
        **SAFE_UPLOAD_RESULT,
        "upload_url": SIGNED_URL,
        "headers": PROVIDER_HEADER,
        "token": TOKEN,
        "bytes": FILE_BYTES,
    }

    result = _run_upload(
        monkeypatch,
        FakeUploadClient(result=unsafe_result),
        "NEU-497",
        "/home/agent/uploads/evidence.png",
        "--json",
    )

    assert result.exit_code == 0, result.output
    assert json.loads(result.stdout) == {
        **SAFE_UPLOAD_RESULT,
        "tool": "linear",
        "issue_id": "NEU-497",
    }
    _assert_no_unsafe_output(result)


def test_upload_json_rejects_a_signed_asset_url_from_the_client(monkeypatch):
    unsafe_result = {**SAFE_UPLOAD_RESULT, "asset_url": SIGNED_URL}

    result = _run_upload(
        monkeypatch,
        FakeUploadClient(result=unsafe_result),
        "NEU-497",
        "/home/agent/uploads/evidence.png",
        "--json",
    )

    assert result.exit_code == 1
    assert json.loads(result.stdout) == GENERIC_UPLOAD_FAILURE
    _assert_no_unsafe_output(result)


def test_upload_human_success_reports_only_stable_metadata(monkeypatch):
    client = FakeUploadClient(result=SAFE_UPLOAD_RESULT)

    result = _run_upload(
        monkeypatch, client, "NEU-497", "/home/agent/uploads/evidence.png"
    )

    assert result.exit_code == 0, result.output
    assert "Uploaded evidence.png to NEU-497" in result.stdout
    assert "https://uploads.linear.app/assets/evidence.png" in result.stdout
    assert "image/png" in result.stdout
    assert "123 bytes" in result.stdout
    assert "123e4567-e89b-12d3-a456-426614174000" in result.stdout
    assert client.close_calls == 1
    _assert_no_unsafe_output(result)


@pytest.mark.parametrize(
    ("error", "stage"),
    [
        (
            UnsafeValidationFailure(
                f"bad file {SIGNED_URL} {PROVIDER_HEADER} {TOKEN} {FILE_BYTES}"
            ),
            "validation",
        ),
        (
            UnsafeUploadFailure(
                f"provider failed {SIGNED_URL} {PROVIDER_HEADER} {TOKEN} {FILE_BYTES}"
            ),
            "upload",
        ),
    ],
)
def test_upload_json_failure_is_nonzero_and_redacts_exception_internals(
    monkeypatch, error: Exception, stage: str
):
    result = _run_upload(
        monkeypatch,
        FakeUploadClient(error=error),
        "NEU-497",
        "/home/agent/uploads/evidence.png",
        "--json",
    )

    assert result.exit_code == 1
    assert json.loads(result.stdout) == {
        "ok": False,
        "tool": "linear",
        "stage": stage,
        "error": f"Linear evidence {stage} failed",
    }
    _assert_no_unsafe_output(result)


def test_upload_human_failure_is_nonzero_and_redacts_exception_internals(monkeypatch):
    error = UnsafeUploadFailure(
        f"provider failed {SIGNED_URL} {PROVIDER_HEADER} {TOKEN} {FILE_BYTES}"
    )

    result = _run_upload(
        monkeypatch,
        FakeUploadClient(error=error),
        "NEU-497",
        "/home/agent/uploads/evidence.png",
    )

    assert result.exit_code == 1
    assert "Linear evidence upload failed" in result.stdout
    assert result.exception is not error
    _assert_no_unsafe_output(result)


def test_upload_comment_partial_failure_is_nonzero_and_keeps_stable_asset_url(
    monkeypatch,
):
    partial = {
        **SAFE_UPLOAD_RESULT,
        "ok": False,
        "comment_id": None,
        "stage": "comment",
        "error": PARTIAL_ERROR,
    }
    unsafe_partial = {
        **partial,
        "upload_url": SIGNED_URL,
        "headers": PROVIDER_HEADER,
        "token": TOKEN,
        "bytes": FILE_BYTES,
    }

    result = _run_upload(
        monkeypatch,
        FakeUploadClient(result=unsafe_partial),
        "NEU-497",
        "/home/agent/uploads/evidence.png",
        "--comment",
        "Evidence",
        "--json",
    )

    assert result.exit_code == 1
    assert json.loads(result.stdout) == {
        **partial,
        "tool": "linear",
        "issue_id": "NEU-497",
    }
    assert "https://uploads.linear.app/assets/evidence.png" in result.stdout
    _assert_no_unsafe_output(result)


def test_upload_human_comment_partial_failure_keeps_only_stable_asset(monkeypatch):
    partial = {
        **SAFE_UPLOAD_RESULT,
        "ok": False,
        "comment_id": None,
        "stage": "comment",
        "error": PARTIAL_ERROR,
    }
    client = FakeUploadClient(result=partial)

    result = _run_upload(
        monkeypatch,
        client,
        "NEU-497",
        "/home/agent/uploads/evidence.png",
        "--comment",
        "Evidence",
    )

    assert result.exit_code == 1
    assert PARTIAL_ERROR in result.stdout
    assert "https://uploads.linear.app/assets/evidence.png" in result.stdout
    assert TOKEN not in result.stdout
    assert client.close_calls == 1


@pytest.mark.parametrize("json_output", [False, True], ids=["human", "json"])
def test_upload_construction_failure_is_sanitized(monkeypatch, json_output: bool):
    args = ["NEU-497", "/home/agent/uploads/evidence.png"]
    if json_output:
        args.append("--json")

    result = _run_upload_with_construction_error(
        monkeypatch, RuntimeError(f"construction failed: {TOKEN} {SIGNED_URL}"), *args
    )

    assert result.exit_code == 1
    if json_output:
        assert json.loads(result.stdout) == GENERIC_UPLOAD_FAILURE
    else:
        assert "Linear evidence upload failed" in result.stdout
    _assert_no_unsafe_output(result)


def test_upload_failure_closes_client(monkeypatch):
    client = FakeUploadClient(error=UnsafeUploadFailure(TOKEN))

    result = _run_upload(
        monkeypatch,
        client,
        "NEU-497",
        "/home/agent/uploads/evidence.png",
        "--json",
    )

    assert result.exit_code == 1
    assert client.close_calls == 1
    _assert_no_unsafe_output(result)


@pytest.mark.parametrize("json_output", [False, True], ids=["human", "json"])
def test_upload_close_failure_is_sanitized(monkeypatch, json_output: bool):
    client = FakeUploadClient(
        result=SAFE_UPLOAD_RESULT,
        close_error=RuntimeError(f"close failed: {TOKEN} {SIGNED_URL}"),
    )
    args = ["NEU-497", "/home/agent/uploads/evidence.png"]
    if json_output:
        args.append("--json")

    result = _run_upload(monkeypatch, client, *args)

    assert result.exit_code == 1
    assert client.close_calls == 1
    if json_output:
        assert json.loads(result.stdout) == GENERIC_UPLOAD_FAILURE
    else:
        assert "Linear evidence upload failed" in result.stdout
        assert "Uploaded evidence.png" not in result.stdout
    _assert_no_unsafe_output(result)


MALFORMED_DOCUMENTED_RESULTS = [
    pytest.param({"ok": TOKEN}, id="ok-not-bool"),
    pytest.param({"ok": 1}, id="ok-int-not-bool"),
    pytest.param({"asset_url": SIGNED_URL}, id="asset-url-signed"),
    pytest.param(
        {"asset_url": f"https://example.com/{TOKEN}.png"}, id="asset-url-wrong-host"
    ),
    pytest.param({"filename": f"../{TOKEN}.png"}, id="filename-not-basename"),
    pytest.param({"filename": f"{TOKEN}.jpg"}, id="filename-unsupported"),
    pytest.param({"filename": f"{TOKEN}\n.png"}, id="filename-not-printable"),
    pytest.param({"filename": f"{TOKEN}{'x' * 240}.png"}, id="filename-too-long"),
    pytest.param({"mime_type": f"image/png;{TOKEN}"}, id="mime-malformed"),
    pytest.param({"mime_type": "video/webm"}, id="mime-inconsistent"),
    pytest.param({"size_bytes": TOKEN}, id="size-not-int"),
    pytest.param({"size_bytes": True}, id="size-bool"),
    pytest.param({"size_bytes": 0}, id="size-empty"),
    pytest.param({"size_bytes": 10 * 1024 * 1024 + 1}, id="size-over-png-cap"),
    pytest.param(
        {
            "filename": "evidence.webm",
            "mime_type": "video/webm",
            "size_bytes": 50 * 1024 * 1024 + 1,
        },
        id="size-over-webm-cap",
    ),
    pytest.param({"comment_id": TOKEN}, id="comment-id-malformed"),
    pytest.param(
        {
            "ok": False,
            "comment_id": None,
            "stage": TOKEN,
            "error": PARTIAL_ERROR,
        },
        id="partial-stage-malformed",
    ),
    pytest.param(
        {
            "ok": False,
            "comment_id": None,
            "stage": "comment",
            "error": TOKEN,
        },
        id="partial-error-malformed",
    ),
    pytest.param(
        {
            "ok": False,
            "comment_id": TOKEN,
            "stage": "comment",
            "error": PARTIAL_ERROR,
        },
        id="partial-comment-id-not-none",
    ),
]


@pytest.mark.parametrize("overrides", MALFORMED_DOCUMENTED_RESULTS)
@pytest.mark.parametrize("json_output", [False, True], ids=["human", "json"])
def test_upload_rejects_malformed_documented_result_without_leaking(
    monkeypatch, overrides: dict[str, Any], json_output: bool
):
    client_result = {**SAFE_UPLOAD_RESULT, **overrides}
    args = ["NEU-497", "/home/agent/uploads/evidence.png"]
    if json_output:
        args.append("--json")

    result = _run_upload(monkeypatch, FakeUploadClient(result=client_result), *args)

    assert result.exit_code == 1
    if json_output:
        assert json.loads(result.stdout) == GENERIC_UPLOAD_FAILURE
    else:
        assert "Linear evidence upload failed" in result.stdout
    _assert_no_unsafe_output(result)


@pytest.mark.parametrize("json_output", [False, True], ids=["human", "json"])
def test_upload_rejects_missing_comment_id_field(monkeypatch, json_output: bool):
    client_result = dict(SAFE_UPLOAD_RESULT)
    client_result.pop("comment_id")
    args = ["NEU-497", "/home/agent/uploads/evidence.png"]
    if json_output:
        args.append("--json")

    result = _run_upload(monkeypatch, FakeUploadClient(result=client_result), *args)

    assert result.exit_code == 1
    if json_output:
        assert json.loads(result.stdout) == GENERIC_UPLOAD_FAILURE
    else:
        assert "Linear evidence upload failed" in result.stdout
    _assert_no_unsafe_output(result)


def test_upload_accepts_webm_at_cap_with_no_comment(monkeypatch):
    client_result = {
        **SAFE_UPLOAD_RESULT,
        "asset_url": "https://uploads.linear.app/assets/evidence.webm",
        "filename": "evidence.webm",
        "mime_type": "video/webm",
        "size_bytes": 50 * 1024 * 1024,
        "comment_id": None,
    }

    result = _run_upload(
        monkeypatch,
        FakeUploadClient(result=client_result),
        "NEU-497",
        "/home/agent/uploads/evidence.webm",
        "--json",
    )

    assert result.exit_code == 0
    assert json.loads(result.stdout) == {
        **client_result,
        "tool": "linear",
        "issue_id": "NEU-497",
    }
    _assert_no_unsafe_output(result)


def test_linear_secret_manifest_replaces_only_explicit_authorization_placeholder():
    manifest = tomllib.loads(Path(__file__).with_name("pyproject.toml").read_text())

    assert manifest["tool"]["centaur"]["secrets"] == [
        {
            "type": "http",
            "name": "LINEAR_API_KEY",
            "match_headers": ["Authorization"],
            "hosts": ["api.linear.app", "uploads.linear.app"],
        }
    ]


def test_bare_uploader_does_not_construct_an_authorization_placeholder():
    source = inspect.getsource(uploads_module.put_upload).casefold()

    assert "authorization" not in source
    assert "linear_api_key" not in source


def test_sandbox_prompt_describes_only_constrained_linear_evidence_uploads():
    prompt = (
        Path(__file__).parents[3] / "services" / "sandbox" / "SYSTEM_PROMPT.md"
    ).read_text()

    assert "linear upload ISSUE_ID FILE [--comment TEXT]" in prompt
    assert "PNG" in prompt and "10 MiB" in prompt
    assert "WebM" in prompt and "50 MiB" in prompt
    assert "/home/agent/uploads/" in prompt
    assert "Linear and GitHub have no file-upload surface" not in prompt
    assert "arbitrary attachments" in prompt
