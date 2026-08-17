"""Security-focused tests for Linear evidence upload validation."""

from __future__ import annotations

import os
import stat
import sys
from pathlib import Path

import pytest

# Pytest is invoked from the repository root; load this independently packaged
# tool from its own directory, matching the existing Linear tests.
sys.path.insert(0, str(Path(__file__).parent))

from uploads import (
    PNG_MAX_BYTES,
    WEBM_MAX_BYTES,
    UploadError,
    UploadPartialFailure,
    UploadValidationError,
    validate_upload_file,
    validate_upload_target,
)


PNG = b"\x89PNG\r\n\x1a\n" + b"test image"
# EBML header containing a correctly encoded DocType element with value "webm".
WEBM = b"\x1a\x45\xdf\xa3\x87\x42\x82\x84webm" + b"test video"


def _write(path: Path, content: bytes) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    return path


def _target(**overrides: object) -> dict[str, object]:
    value: dict[str, object] = {
        "uploadUrl": "https://uploads.linear.app/upload/object?signature=secret",
        "assetUrl": "https://uploads.linear.app/assets/evidence.png",
        "headers": [
            {"key": "x-amz-checksum-sha256", "value": "checksum"},
            {"key": "X-Amz-Meta-Test", "value": "value"},
        ],
    }
    value.update(overrides)
    return value


@pytest.mark.parametrize(
    ("filename", "content", "mime_type"),
    [
        ("evidence.png", PNG, "image/png"),
        ("evidence.webm", WEBM, "video/webm"),
    ],
)
def test_validate_upload_file_accepts_supported_evidence(
    tmp_path: Path, filename: str, content: bytes, mime_type: str
) -> None:
    path = _write(tmp_path / "uploads" / filename, content)

    upload = validate_upload_file(path, uploads_root=tmp_path / "uploads")

    assert upload.path == path.resolve()
    assert upload.filename == filename
    assert upload.mime_type == mime_type
    assert upload.size_bytes == len(content)


def test_validate_upload_file_resolves_relative_paths_from_uploads_root(
    tmp_path: Path,
) -> None:
    path = _write(tmp_path / "uploads" / "nested" / "evidence.png", PNG)

    upload = validate_upload_file(
        Path("nested/evidence.png"), uploads_root=tmp_path / "uploads"
    )

    assert upload.path == path.resolve()


def test_validate_upload_file_uses_home_uploads_as_default(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    path = _write(tmp_path / "uploads" / "evidence.png", PNG)
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: tmp_path))

    upload = validate_upload_file("evidence.png")

    assert upload.path == path.resolve()


@pytest.mark.parametrize(
    "path_factory",
    [
        lambda root, outside: outside,
        lambda root, outside: root / ".." / outside.name,
    ],
    ids=["absolute", "parent-traversal"],
)
def test_validate_upload_file_rejects_paths_outside_root(
    tmp_path: Path, path_factory
) -> None:
    root = tmp_path / "uploads"
    root.mkdir()
    outside = _write(tmp_path / "outside.png", PNG)

    with pytest.raises(UploadValidationError, match="beneath"):
        validate_upload_file(path_factory(root, outside), uploads_root=root)


def test_validate_upload_file_rejects_final_symlink(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = tmp_path / "uploads"
    target = _write(root / "real.png", PNG)
    link = root / "link.png"
    try:
        link.symlink_to(target)
    except OSError:
        # Unprivileged Windows runners cannot create symlinks. Simulate the
        # same lstat result so the fail-closed branch remains exercised.
        link.write_bytes(PNG)
        real_lstat = Path.lstat

        def fake_lstat(candidate: Path):
            result = real_lstat(candidate)
            if candidate == link:
                return os.stat_result((stat.S_IFLNK | 0o777, *result[1:]))
            return result

        monkeypatch.setattr(Path, "lstat", fake_lstat)

    with pytest.raises(UploadValidationError, match="symlink"):
        validate_upload_file(link, uploads_root=root)


def test_validate_upload_file_rejects_symlink_ancestor(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = tmp_path / "uploads"
    real_directory = root / "real"
    _write(real_directory / "evidence.png", PNG)
    linked_directory = root / "linked"
    try:
        linked_directory.symlink_to(real_directory, target_is_directory=True)
    except OSError:
        _write(linked_directory / "evidence.png", PNG)
        real_lstat = Path.lstat

        def fake_lstat(candidate: Path):
            result = real_lstat(candidate)
            if candidate == linked_directory:
                return os.stat_result((stat.S_IFLNK | 0o777, *result[1:]))
            return result

        monkeypatch.setattr(Path, "lstat", fake_lstat)

    with pytest.raises(UploadValidationError, match="symlink"):
        validate_upload_file(linked_directory / "evidence.png", uploads_root=root)


def test_validate_upload_file_rejects_directory(tmp_path: Path) -> None:
    root = tmp_path / "uploads"
    directory = root / "evidence.png"
    directory.mkdir(parents=True)

    with pytest.raises(UploadValidationError, match="regular file"):
        validate_upload_file(directory, uploads_root=root)


def test_validate_upload_file_rejects_non_regular_file(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = tmp_path / "uploads"
    path = _write(root / "evidence.png", PNG)
    real_lstat = Path.lstat

    def fake_lstat(candidate: Path):
        result = real_lstat(candidate)
        if candidate == path:
            return os.stat_result((0o010000, *result[1:]))  # FIFO file type.
        return result

    monkeypatch.setattr(Path, "lstat", fake_lstat)

    with pytest.raises(UploadValidationError, match="regular file"):
        validate_upload_file(path, uploads_root=root)


def test_validate_upload_file_rejects_empty_file(tmp_path: Path) -> None:
    root = tmp_path / "uploads"
    path = _write(root / "empty.png", b"")

    with pytest.raises(UploadValidationError, match="empty"):
        validate_upload_file(path, uploads_root=root)


@pytest.mark.parametrize(
    ("filename", "limit"),
    [("too-large.png", PNG_MAX_BYTES), ("too-large.webm", WEBM_MAX_BYTES)],
)
def test_validate_upload_file_rejects_files_over_size_cap(
    tmp_path: Path, filename: str, limit: int
) -> None:
    root = tmp_path / "uploads"
    root.mkdir()
    path = root / filename
    with path.open("wb") as handle:
        handle.truncate(limit + 1)

    with pytest.raises(UploadValidationError, match="exceeds"):
        validate_upload_file(path, uploads_root=root)


@pytest.mark.parametrize(
    ("filename", "content"),
    [
        ("mismatch.png", WEBM),
        ("mismatch.webm", PNG),
        ("unsupported.jpg", PNG),
    ],
)
def test_validate_upload_file_rejects_extension_or_signature_mismatch(
    tmp_path: Path, filename: str, content: bytes
) -> None:
    root = tmp_path / "uploads"
    path = _write(root / filename, content)

    with pytest.raises(UploadValidationError):
        validate_upload_file(path, uploads_root=root)


@pytest.mark.parametrize(
    "content",
    [
        b"\x1a\x45\xdf\xa3webm",  # Text is not an encoded DocType element.
        b"\x1a\x45\xdf\xa3\x87\x42\x82\x84matr",  # Wrong DocType.
        b"\x1a\x45\xdf\xa3\xff\x42\x82\x84webm",  # Unknown EBML size.
        b"\x1a\x45\xdf\xa3\x90\x42\x82\x84webm",  # Truncated EBML header.
    ],
)
def test_validate_upload_file_rejects_fake_webm(
    tmp_path: Path, content: bytes
) -> None:
    root = tmp_path / "uploads"
    path = _write(root / "fake.webm", content)

    with pytest.raises(UploadValidationError, match="WebM"):
        validate_upload_file(path, uploads_root=root)


def test_validate_upload_target_accepts_and_normalizes_safe_provider_headers() -> None:
    target = validate_upload_target(_target())

    assert target.upload_url.startswith("https://uploads.linear.app/")
    assert target.asset_url == "https://uploads.linear.app/assets/evidence.png"
    assert target.headers == {
        "x-amz-checksum-sha256": "checksum",
        "x-amz-meta-test": "value",
    }
    assert target.follow_redirects is False


@pytest.mark.parametrize(
    "field",
    ["uploadUrl", "assetUrl"],
)
@pytest.mark.parametrize(
    "url",
    [
        "http://uploads.linear.app/object",
        "//uploads.linear.app/object",
        "https://uploads.linear.app.evil.example/object",
        "https://sub.uploads.linear.app/object",
        "https://api.linear.app/object",
        "https://uploads.linear.app:444/object",
        "https://user:password@uploads.linear.app/object",
        "https://uploads.linear.app/object#fragment",
        "https://uploads.linear.app//evil.example/object",
        " https://uploads.linear.app/object",
        "https://uploads.linear.app\\@evil.example/object",
    ],
)
def test_validate_upload_target_rejects_unsafe_urls(field: str, url: str) -> None:
    with pytest.raises(UploadValidationError, match=field):
        validate_upload_target(_target(**{field: url}))


@pytest.mark.parametrize(
    "header",
    [
        "Authorization",
        "authorization",
        "Proxy-Authorization",
        "Cookie",
        "Set-Cookie",
        "Host",
    ],
)
def test_validate_upload_target_rejects_forbidden_returned_headers(
    header: str,
) -> None:
    with pytest.raises(UploadValidationError, match="forbidden"):
        validate_upload_target(
            _target(headers=[{"key": header, "value": "do-not-forward"}])
        )


@pytest.mark.parametrize(
    "headers",
    [
        [{"key": "X-Test\r\nAuthorization", "value": "x"}],
        [{"key": "X-Test", "value": "safe\r\nAuthorization: secret"}],
        [{"key": "X-Test", "value": "safe\x00unsafe"}],
        [{"key": "X-Test", "value": "one"}, {"key": "x-test", "value": "two"}],
        [{"key": "X-Test"}],
        {"X-Test": "not-the-documented-array-shape"},
    ],
)
def test_validate_upload_target_rejects_malformed_returned_headers(
    headers: object,
) -> None:
    with pytest.raises(UploadValidationError, match="headers"):
        validate_upload_target(_target(headers=headers))


@pytest.mark.parametrize(
    "payload",
    [
        {},
        {"uploadUrl": None, "assetUrl": None, "headers": []},
        {"uploadUrl": "https://uploads.linear.app/object", "headers": []},
    ],
)
def test_validate_upload_target_rejects_incomplete_payloads(
    payload: dict[str, object],
) -> None:
    with pytest.raises(UploadValidationError):
        validate_upload_target(payload)


def test_upload_partial_failure_preserves_only_safe_asset_metadata() -> None:
    error = UploadPartialFailure(
        asset_url="https://uploads.linear.app/assets/evidence.png",
        filename="evidence.png",
        mime_type="image/png",
        size_bytes=123,
    )

    assert error.stage == "comment"
    assert error.asset_url == "https://uploads.linear.app/assets/evidence.png"
    assert error.filename == "evidence.png"
    assert error.mime_type == "image/png"
    assert error.size_bytes == 123
    assert isinstance(error, UploadError)
    assert "signature" not in str(error).lower()
