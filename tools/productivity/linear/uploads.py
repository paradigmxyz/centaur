"""Fail-closed validation primitives for Linear evidence uploads."""

from __future__ import annotations

import os
import re
import stat
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

PNG_MAX_BYTES = 10 * 1024 * 1024
WEBM_MAX_BYTES = 50 * 1024 * 1024

_PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
_EBML_HEADER_ID = b"\x1a\x45\xdf\xa3"
_EBML_DOCTYPE_ID = b"\x42\x82"
_MAX_EBML_HEADER_BYTES = 4096
_LINEAR_UPLOAD_HOST = "uploads.linear.app"
_FORBIDDEN_HEADERS = {
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "host",
}
_HEADER_NAME = re.compile(r"[!#$%&'*+.^_`|~0-9A-Za-z-]+\Z")


class UploadError(RuntimeError):
    """Base class for failures in the Linear evidence upload flow."""

    stage = "upload"


class UploadValidationError(UploadError):
    """A local file or Linear-provided upload target failed validation."""

    stage = "validation"


class UploadPartialFailure(UploadError):
    """The bytes were uploaded, but the requested Linear comment failed."""

    stage = "comment"

    def __init__(
        self,
        *,
        asset_url: str,
        filename: str,
        mime_type: str,
        size_bytes: int,
    ) -> None:
        super().__init__("Linear evidence uploaded, but comment creation failed")
        self.asset_url = asset_url
        self.filename = filename
        self.mime_type = mime_type
        self.size_bytes = size_bytes


@dataclass(frozen=True, slots=True)
class UploadFile:
    """Validated local evidence metadata safe to pass to upload allocation."""

    path: Path
    filename: str
    mime_type: str
    size_bytes: int


@dataclass(frozen=True, slots=True)
class ValidatedTarget:
    """Validated Linear upload allocation with safe provider headers only."""

    upload_url: str
    asset_url: str
    headers: dict[str, str]
    follow_redirects: bool = field(default=False, init=False)


def _absolute_without_symlink_resolution(path: Path) -> Path:
    return Path(os.path.abspath(os.fspath(path)))


def _confined_file(path: str | os.PathLike[str], uploads_root: Path) -> Path:
    root = _absolute_without_symlink_resolution(uploads_root.expanduser())
    requested = Path(path).expanduser()
    candidate = _absolute_without_symlink_resolution(
        requested if requested.is_absolute() else root / requested
    )

    try:
        relative = candidate.relative_to(root)
    except ValueError as exc:
        raise UploadValidationError("upload file must be beneath the uploads root") from exc

    try:
        root_info = root.lstat()
    except OSError as exc:
        raise UploadValidationError("uploads root is unavailable") from exc
    if stat.S_ISLNK(root_info.st_mode):
        raise UploadValidationError("uploads root must not be a symlink")
    if not stat.S_ISDIR(root_info.st_mode):
        raise UploadValidationError("uploads root must be a directory")

    current = root
    parts = relative.parts
    for index, part in enumerate(parts):
        current = current / part
        try:
            info = current.lstat()
        except OSError as exc:
            raise UploadValidationError("upload file is unavailable") from exc
        if stat.S_ISLNK(info.st_mode):
            raise UploadValidationError("upload path must not contain symlinks")
        if index < len(parts) - 1 and not stat.S_ISDIR(info.st_mode):
            raise UploadValidationError("upload path ancestor must be a directory")

    try:
        leaf_info = candidate.lstat()
    except OSError as exc:
        raise UploadValidationError("upload file is unavailable") from exc
    if not stat.S_ISREG(leaf_info.st_mode):
        raise UploadValidationError("upload path must name a regular file")

    try:
        resolved_root = root.resolve(strict=True)
        resolved_parent = candidate.parent.resolve(strict=True)
        resolved_parent.relative_to(resolved_root)
    except (OSError, ValueError) as exc:
        raise UploadValidationError("upload file must resolve beneath the uploads root") from exc
    return resolved_parent / candidate.name


def _read_vint(data: bytes, offset: int, *, max_width: int) -> tuple[int, int] | None:
    if offset >= len(data) or data[offset] == 0:
        return None
    first = data[offset]
    marker = 0x80
    width = 1
    while width <= max_width and not first & marker:
        marker >>= 1
        width += 1
    if width > max_width or offset + width > len(data):
        return None

    value = first & (marker - 1)
    for byte in data[offset + 1 : offset + width]:
        value = (value << 8) | byte
    if value == (1 << (7 * width)) - 1:
        return None
    return value, width


def _read_element_id(data: bytes, offset: int) -> tuple[bytes, int] | None:
    if offset >= len(data) or data[offset] == 0:
        return None
    marker = 0x80
    width = 1
    while width <= 4 and not data[offset] & marker:
        marker >>= 1
        width += 1
    if width > 4 or offset + width > len(data):
        return None
    return data[offset : offset + width], width


def _has_webm_doctype(header: bytes) -> bool:
    if not header.startswith(_EBML_HEADER_ID):
        return False
    size_result = _read_vint(header, len(_EBML_HEADER_ID), max_width=8)
    if size_result is None:
        return False
    header_size, size_width = size_result
    if header_size > _MAX_EBML_HEADER_BYTES:
        return False

    position = len(_EBML_HEADER_ID) + size_width
    end = position + header_size
    if end > len(header):
        return False

    while position < end:
        element_id_result = _read_element_id(header, position)
        if element_id_result is None:
            return False
        element_id, id_width = element_id_result
        position += id_width
        element_size_result = _read_vint(header, position, max_width=8)
        if element_size_result is None:
            return False
        element_size, element_size_width = element_size_result
        position += element_size_width
        value_end = position + element_size
        if value_end > end:
            return False
        if element_id == _EBML_DOCTYPE_ID:
            return header[position:value_end] == b"webm"
        position = value_end
    return False


def validate_upload_file(
    path: str | os.PathLike[str], uploads_root: str | os.PathLike[str] | None = None
) -> UploadFile:
    """Validate a PNG or WebM regular file confined beneath the uploads root."""

    root = Path(uploads_root) if uploads_root is not None else Path.home() / "uploads"
    validated_path = _confined_file(path, root)
    try:
        size_bytes = validated_path.stat().st_size
    except OSError as exc:
        raise UploadValidationError("upload file is unavailable") from exc
    if size_bytes == 0:
        raise UploadValidationError("upload file must not be empty")

    suffix = validated_path.suffix.lower()
    if suffix == ".png":
        mime_type = "image/png"
        size_limit = PNG_MAX_BYTES
        bytes_to_read = len(_PNG_SIGNATURE)
    elif suffix == ".webm":
        mime_type = "video/webm"
        size_limit = WEBM_MAX_BYTES
        bytes_to_read = len(_EBML_HEADER_ID) + 8 + _MAX_EBML_HEADER_BYTES
    else:
        raise UploadValidationError("upload file must have a .png or .webm extension")

    if size_bytes > size_limit:
        raise UploadValidationError(
            f"upload file exceeds the {size_limit}-byte limit for {suffix}"
        )
    try:
        with validated_path.open("rb") as handle:
            header = handle.read(bytes_to_read)
    except OSError as exc:
        raise UploadValidationError("upload file could not be read") from exc

    if suffix == ".png" and not header.startswith(_PNG_SIGNATURE):
        raise UploadValidationError("upload file does not have a valid PNG signature")
    if suffix == ".webm" and not _has_webm_doctype(header):
        raise UploadValidationError("upload file does not have a valid WebM DocType")

    return UploadFile(
        path=validated_path,
        filename=validated_path.name,
        mime_type=mime_type,
        size_bytes=size_bytes,
    )


def _validate_linear_url(value: Any, field_name: str) -> str:
    if not isinstance(value, str) or not value or value != value.strip():
        raise UploadValidationError(f"{field_name} must be an exact Linear upload URL")
    if "\\" in value or any(ord(character) < 0x20 for character in value):
        raise UploadValidationError(f"{field_name} contains unsafe URL characters")
    try:
        parsed = urlsplit(value)
        port = parsed.port
    except ValueError as exc:
        raise UploadValidationError(f"{field_name} is not a valid URL") from exc
    if (
        parsed.scheme != "https"
        or parsed.netloc != _LINEAR_UPLOAD_HOST
        or parsed.hostname != _LINEAR_UPLOAD_HOST
        or port is not None
        or parsed.username is not None
        or parsed.password is not None
        or not parsed.path.startswith("/")
        or parsed.path.startswith("//")
        or parsed.fragment
    ):
        raise UploadValidationError(
            f"{field_name} must use the exact https://{_LINEAR_UPLOAD_HOST} host"
        )
    return value


def _validated_headers(value: Any) -> dict[str, str]:
    if not isinstance(value, Sequence) or isinstance(value, (str, bytes, bytearray)):
        raise UploadValidationError("upload target headers must use Linear's array shape")

    normalized: dict[str, str] = {}
    for item in value:
        if not isinstance(item, Mapping):
            raise UploadValidationError("upload target headers contain a malformed entry")
        key = item.get("key")
        header_value = item.get("value")
        if (
            not isinstance(key, str)
            or not key
            or _HEADER_NAME.fullmatch(key) is None
            or not isinstance(header_value, str)
            or any(
                ord(character) < 0x20 or ord(character) == 0x7F
                for character in header_value
            )
        ):
            raise UploadValidationError("upload target headers contain an unsafe entry")
        normalized_key = key.lower()
        if normalized_key in _FORBIDDEN_HEADERS:
            raise UploadValidationError(
                f"upload target contains forbidden header {normalized_key}"
            )
        if normalized_key in normalized:
            raise UploadValidationError("upload target headers contain a duplicate name")
        normalized[normalized_key] = header_value
    return normalized


def validate_upload_target(upload_file: Mapping[str, Any]) -> ValidatedTarget:
    """Validate URLs and provider headers returned by Linear's fileUpload mutation."""

    if not isinstance(upload_file, Mapping):
        raise UploadValidationError("Linear upload target must be an object")
    upload_url = _validate_linear_url(upload_file.get("uploadUrl"), "uploadUrl")
    asset_url = _validate_linear_url(upload_file.get("assetUrl"), "assetUrl")
    headers = _validated_headers(upload_file.get("headers"))
    return ValidatedTarget(
        upload_url=upload_url,
        asset_url=asset_url,
        headers=headers,
    )
