"""Tests for the Linear tool client's mutation result handling.

Run from this directory: uv run --no-project --with pytest pytest test_client.py
"""

from __future__ import annotations

import importlib.util
import sys
import types
from pathlib import Path
from typing import Any

import pytest

sys.path.insert(0, str(Path(__file__).parent))

import uploads as uploads_module
from uploads import UploadError, UploadValidationError

# client.py inherits from the packaged readonly client. The mutation logic under
# test never touches readonly behavior, so stub the base class before loading the
# module as a standalone file.
if "readonly" not in sys.modules:
    readonly_mod = types.ModuleType("readonly")

    class LinearReadonlyClient:
        def __init__(self, *args: Any, **kwargs: Any) -> None:
            pass

        def _query(self, query: str, variables: dict | None = None) -> dict:
            raise NotImplementedError

    readonly_mod.LinearReadonlyClient = LinearReadonlyClient
    sys.modules["readonly"] = readonly_mod

spec = importlib.util.spec_from_file_location(
    "linear_client", Path(__file__).with_name("client.py")
)
assert spec and spec.loader
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
LinearClient = module.LinearClient


class RecordingLinearClient(LinearClient):
    """Returns canned mutation payloads keyed by substring, records calls."""

    def __init__(self, responses: dict[str, Any]) -> None:
        self.responses = responses
        self.calls: list[dict[str, Any]] = []

    def _query(self, query: str, variables: dict | None = None) -> dict:
        self.calls.append({"query": query, "variables": variables})
        for key, payload in self.responses.items():
            if key in query:
                return {key: payload}
        raise AssertionError(f"unexpected query: {query}")


def test_create_issue_merges_success_into_issue_fields():
    client = RecordingLinearClient(
        {
            "issueCreate": {
                "success": True,
                "issue": {"id": "issue-1", "identifier": "ENG-1", "title": "Test"},
            }
        }
    )

    created = client.create_issue("Test", team_id="team-1", priority=2)

    assert created["identifier"] == "ENG-1"
    assert created["success"] is True
    assert client.calls[0]["variables"]["input"] == {
        "title": "Test",
        "teamId": "team-1",
        "priority": 2,
    }


def test_create_issue_sets_project_and_milestone():
    client = RecordingLinearClient(
        {
            "issueCreate": {
                "success": True,
                "issue": {"id": "issue-1", "identifier": "ENG-1"},
            }
        }
    )

    client.create_issue(
        "Test",
        team_id="team-1",
        project_id="project-1",
        project_milestone_id="milestone-1",
    )

    assert client.calls[0]["variables"]["input"] == {
        "title": "Test",
        "teamId": "team-1",
        "projectId": "project-1",
        "projectMilestoneId": "milestone-1",
    }


def test_update_issue_merges_success_into_issue_fields():
    client = RecordingLinearClient(
        {
            "issueUpdate": {
                "success": True,
                "issue": {"id": "issue-1", "identifier": "ENG-1", "title": "Renamed"},
            }
        }
    )

    updated = client.update_issue("ENG-1", title="Renamed")

    assert updated["title"] == "Renamed"
    assert updated["success"] is True


def test_update_issue_sets_project_milestone():
    client = RecordingLinearClient(
        {
            "issueUpdate": {
                "success": True,
                "issue": {"id": "issue-1", "identifier": "ENG-1"},
            }
        }
    )

    client.update_issue("ENG-1", project_milestone_id="milestone-1")

    assert client.calls[0]["variables"]["input"] == {"projectMilestoneId": "milestone-1"}


def test_update_issue_clears_project_milestone():
    client = RecordingLinearClient(
        {
            "issueUpdate": {
                "success": True,
                "issue": {"id": "issue-1", "identifier": "ENG-1"},
            }
        }
    )

    client.update_issue("ENG-1", clear_project_milestone=True)

    assert client.calls[0]["variables"]["input"] == {"projectMilestoneId": None}


def test_update_issue_rejects_set_and_clear_project_milestone():
    client = RecordingLinearClient({})

    try:
        client.update_issue(
            "ENG-1",
            project_milestone_id="milestone-1",
            clear_project_milestone=True,
        )
    except ValueError as exc:
        assert "mutually exclusive" in str(exc)
    else:
        raise AssertionError("expected ValueError")


def test_add_comment_merges_success_into_comment_fields():
    client = RecordingLinearClient(
        {"commentCreate": {"success": True, "comment": {"id": "comment-1", "body": "hi"}}}
    )

    comment = client.add_comment("ENG-1", "hi")

    assert comment["id"] == "comment-1"
    assert comment["success"] is True


def test_mutations_surface_failure():
    client = RecordingLinearClient(
        {
            "issueCreate": {"success": False, "issue": None},
            "issueUpdate": {"success": False, "issue": None},
            "commentCreate": {"success": False, "comment": None},
        }
    )

    # Callers (e.g. workflow helpers) key on result["success"] is False.
    assert client.create_issue("Test", team_id="team-1") == {"success": False}
    assert client.update_issue("ENG-1", title="New title") == {"success": False}
    assert client.add_comment("ENG-1", "hello") == {"success": False}


def _upload_target() -> dict[str, object]:
    return {
        "uploadUrl": "https://uploads.linear.app/upload/object?signature=secret",
        "assetUrl": "https://uploads.linear.app/assets/evidence.png",
        "headers": [{"key": "x-amz-checksum-sha256", "value": "checksum"}],
    }


UPLOAD_COMMENT_ID = "123e4567-e89b-12d3-a456-426614174000"


def _write_png(tmp_path: Path) -> tuple[Path, bytes]:
    content = b"\x89PNG\r\n\x1a\nprotocol evidence"
    path = tmp_path / "uploads" / "evidence.png"
    path.parent.mkdir()
    path.write_bytes(content)
    return path, content


def test_upload_evidence_uses_exact_file_upload_mutation_and_variables(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    path, content = _write_png(tmp_path)
    client = RecordingLinearClient(
        {"fileUpload": {"success": True, "uploadFile": _upload_target()}}
    )
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: tmp_path))
    monkeypatch.setattr(module, "put_upload", lambda target, upload: target.asset_url)

    result = client.upload_evidence("NEU-497", path)

    allocation = client.calls[0]
    compact_query = " ".join(allocation["query"].split())
    assert (
        "mutation FileUpload($filename: String!, $contentType: String!, $size: Int!)"
        in compact_query
    )
    assert (
        "fileUpload(filename: $filename, contentType: $contentType, size: $size)"
        in compact_query
    )
    assert "success uploadFile { uploadUrl assetUrl headers { key value } }" in compact_query
    assert allocation["variables"] == {
        "filename": "evidence.png",
        "contentType": "image/png",
        "size": len(content),
    }
    assert result == {
        "ok": True,
        "tool": "linear",
        "issue_id": "NEU-497",
        "asset_url": "https://uploads.linear.app/assets/evidence.png",
        "filename": "evidence.png",
        "mime_type": "image/png",
        "size_bytes": len(content),
        "comment_id": None,
    }


def test_upload_evidence_uploads_before_creating_comment(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    path, content = _write_png(tmp_path)
    events: list[str] = []

    class OrderedClient(RecordingLinearClient):
        def _query(self, query: str, variables: dict | None = None) -> dict:
            events.append("comment" if "commentCreate" in query else "allocate")
            return super()._query(query, variables)

    client = OrderedClient(
        {
            "fileUpload": {"success": True, "uploadFile": _upload_target()},
            "commentCreate": {
                "success": True,
                "comment": {"id": UPLOAD_COMMENT_ID, "body": "Evidence"},
            },
        }
    )
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: tmp_path))

    def record_upload(target, upload) -> str:
        events.append("upload")
        assert upload.content == content
        return target.asset_url

    monkeypatch.setattr(module, "put_upload", record_upload)

    result = client.upload_evidence("NEU-497", path, comment="Evidence")

    assert events == ["allocate", "upload", "comment"]
    assert result["comment_id"] == UPLOAD_COMMENT_ID
    assert client.calls[1]["variables"] == {
        "input": {
            "issueId": "NEU-497",
            "body": (
                "Evidence\n\n"
                "[evidence.png](https://uploads.linear.app/assets/evidence.png)"
            ),
        }
    }


def test_upload_evidence_escapes_filename_in_markdown_asset_link(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    content = b"\x89PNG\r\n\x1a\nprotocol evidence"
    path = tmp_path / "uploads" / "evi[proof].png"
    path.parent.mkdir()
    path.write_bytes(content)
    client = RecordingLinearClient(
        {
            "fileUpload": {"success": True, "uploadFile": _upload_target()},
            "commentCreate": {
                "success": True,
                "comment": {"id": UPLOAD_COMMENT_ID},
            },
        }
    )
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: tmp_path))
    monkeypatch.setattr(module, "put_upload", lambda target, upload: target.asset_url)

    client.upload_evidence("NEU-497", path, comment="Supplied text")

    comment_calls = [call for call in client.calls if "commentCreate" in call["query"]]
    assert len(comment_calls) == 1
    assert comment_calls[0]["variables"] == {
        "input": {
            "issueId": "NEU-497",
            "body": (
                "Supplied text\n\n"
                "[evi\\[proof\\].png]"
                "(https://uploads.linear.app/assets/evidence.png)"
            ),
        }
    }


def test_markdown_link_label_escapes_brackets_and_backslashes() -> None:
    assert module._markdown_link_label("evi\\[proof].png") == "evi\\\\\\[proof\\].png"


def test_upload_evidence_rejects_unsafe_filename_before_allocation_or_comment(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    class UnsafeFilenamePath:
        name = "unsafe\nname.png"

        def open(self, *args: object, **kwargs: object):
            raise AssertionError("unsafe filename must be rejected before file access")

    monkeypatch.setattr(
        uploads_module,
        "_confined_file",
        lambda path, root: (UnsafeFilenamePath(), ()),
    )
    upload_calls = 0

    def record_upload(target: object, upload: object) -> str:
        nonlocal upload_calls
        upload_calls += 1
        return "must-not-upload"

    monkeypatch.setattr(module, "put_upload", record_upload)
    client = RecordingLinearClient({})

    with pytest.raises(UploadValidationError, match="filename"):
        client.upload_evidence("NEU-497", "ignored", comment="must not be created")

    assert client.calls == []
    assert upload_calls == 0


@pytest.mark.parametrize("field", ["uploadUrl", "assetUrl"])
def test_upload_evidence_rejects_root_target_before_put_or_comment(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    field: str,
) -> None:
    path, _ = _write_png(tmp_path)
    target = {**_upload_target(), field: "https://uploads.linear.app/"}
    client = RecordingLinearClient(
        {
            "fileUpload": {"success": True, "uploadFile": target},
            "commentCreate": {
                "success": True,
                "comment": {"id": "must-not-exist"},
            },
        }
    )
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: tmp_path))
    upload_calls = 0

    def record_upload(target: object, upload: object) -> str:
        nonlocal upload_calls
        upload_calls += 1
        return "must-not-upload"

    monkeypatch.setattr(module, "put_upload", record_upload)

    with pytest.raises(UploadValidationError, match=field):
        client.upload_evidence("NEU-497", path, comment="must not be created")

    assert len(client.calls) == 1
    assert "fileUpload" in client.calls[0]["query"]
    assert upload_calls == 0


def test_upload_evidence_does_not_comment_after_upload_failure(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    path, _ = _write_png(tmp_path)
    client = RecordingLinearClient(
        {
            "fileUpload": {"success": True, "uploadFile": _upload_target()},
            "commentCreate": {
                "success": True,
                "comment": {"id": "must-not-exist"},
            },
        }
    )
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: tmp_path))

    def fail_upload(target, upload) -> str:
        raise UploadError("Linear evidence upload failed")

    monkeypatch.setattr(module, "put_upload", fail_upload)

    with pytest.raises(UploadError, match="upload failed"):
        client.upload_evidence("NEU-497", path, comment="must not be created")

    assert len(client.calls) == 1
    assert "fileUpload" in client.calls[0]["query"]


@pytest.mark.parametrize("comment_failure", ["result", "exception"])
def test_upload_evidence_returns_safe_partial_failure_without_retry(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    comment_failure: str,
) -> None:
    path, content = _write_png(tmp_path)
    upload_calls = 0
    comment_attempts: list[tuple[str, str]] = []

    class CommentFailureClient(RecordingLinearClient):
        def add_comment(self, issue_id: str, body: str) -> dict[str, Any]:
            comment_attempts.append((issue_id, body))
            if comment_failure == "exception":
                raise RuntimeError("secret comment transport detail")
            return super().add_comment(issue_id, body)

    client = CommentFailureClient(
        {
            "fileUpload": {"success": True, "uploadFile": _upload_target()},
            "commentCreate": {"success": False, "comment": None},
        }
    )
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: tmp_path))

    def record_upload(target, upload) -> str:
        nonlocal upload_calls
        upload_calls += 1
        return target.asset_url

    monkeypatch.setattr(module, "put_upload", record_upload)

    result = client.upload_evidence("NEU-497", path, comment="Evidence")

    assert upload_calls == 1
    assert comment_attempts == [
        (
            "NEU-497",
            "Evidence\n\n"
            "[evidence.png](https://uploads.linear.app/assets/evidence.png)",
        )
    ]
    assert result == {
        "ok": False,
        "tool": "linear",
        "issue_id": "NEU-497",
        "asset_url": "https://uploads.linear.app/assets/evidence.png",
        "filename": "evidence.png",
        "mime_type": "image/png",
        "size_bytes": len(content),
        "comment_id": None,
        "stage": "comment",
        "error": "Linear evidence uploaded, but comment creation failed",
    }
    rendered = repr(result)
    assert "signature" not in rendered.lower()
    assert "checksum" not in rendered.lower()
    assert "secret comment transport detail" not in rendered
