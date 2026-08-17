"""Fail-closed validation primitives for Linear evidence uploads."""

from __future__ import annotations

import os
import re
import stat
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from pathlib import Path
from types import MappingProxyType
from typing import Any
from urllib.parse import urlsplit

import httpx

PNG_MAX_BYTES = 10 * 1024 * 1024
WEBM_MAX_BYTES = 50 * 1024 * 1024

_PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
_EBML_HEADER_ID = b"\x1a\x45\xdf\xa3"
_EBML_DOCTYPE_ID = b"\x42\x82"
_MAX_EBML_HEADER_BYTES = 4096
_LINEAR_UPLOAD_HOST = "uploads.linear.app"
_WINDOWS_REPARSE_POINT = (
    getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400) if os.name == "nt" else 0
)
_FORBIDDEN_HEADERS = {
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "upgrade",
    "te",
    "trailer",
    "keep-alive",
    "proxy-connection",
    "expect",
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
        self.asset_url = _validate_linear_url(
            asset_url, "assetUrl", allow_query=False
        )
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
    content: bytes = field(repr=False)


@dataclass(frozen=True, slots=True)
class ValidatedTarget:
    """Validated Linear upload allocation with safe provider headers only."""

    upload_url: str = field(repr=False)
    asset_url: str
    headers: Mapping[str, str] = field(repr=False)
    follow_redirects: bool = field(default=False, init=False)


def _absolute_without_symlink_resolution(path: Path) -> Path:
    return Path(os.path.abspath(os.fspath(path)))


def _is_junction(path: Path, info: os.stat_result) -> bool:
    file_attributes = getattr(info, "st_file_attributes", None)
    if (
        _WINDOWS_REPARSE_POINT
        and isinstance(file_attributes, int)
        and file_attributes & _WINDOWS_REPARSE_POINT
    ):
        return True
    checker = getattr(path, "is_junction", None)
    if callable(checker):
        return bool(checker())
    return bool(_WINDOWS_REPARSE_POINT and not isinstance(file_attributes, int))


def _file_identity(info: os.stat_result) -> tuple[int, int, int]:
    return info.st_dev, info.st_ino, stat.S_IFMT(info.st_mode)


def _path_info(path: Path, *, unavailable_message: str) -> os.stat_result:
    try:
        info = path.lstat()
        junction = _is_junction(path, info)
    except OSError as exc:
        raise UploadValidationError(unavailable_message) from exc
    if stat.S_ISLNK(info.st_mode):
        raise UploadValidationError("upload path must not contain symlinks")
    if junction:
        raise UploadValidationError("upload path must not contain junctions")
    return info


def _confined_file(
    path: str | os.PathLike[str], uploads_root: Path
) -> tuple[Path, tuple[tuple[Path, tuple[int, int, int]], ...]]:
    root = _absolute_without_symlink_resolution(uploads_root.expanduser())
    requested = Path(path).expanduser()
    candidate = _absolute_without_symlink_resolution(
        requested if requested.is_absolute() else root / requested
    )

    try:
        relative = candidate.relative_to(root)
    except ValueError as exc:
        raise UploadValidationError("upload file must be beneath the uploads root") from exc

    root_info = _path_info(root, unavailable_message="uploads root is unavailable")
    if not stat.S_ISDIR(root_info.st_mode):
        raise UploadValidationError("uploads root must be a directory")

    snapshots = [(root, _file_identity(root_info))]
    current = root
    parts = relative.parts
    for index, part in enumerate(parts):
        current = current / part
        info = _path_info(current, unavailable_message="upload file is unavailable")
        snapshots.append((current, _file_identity(info)))
        if index < len(parts) - 1 and not stat.S_ISDIR(info.st_mode):
            raise UploadValidationError("upload path ancestor must be a directory")

    leaf_info = root_info if not parts else info
    if not stat.S_ISREG(leaf_info.st_mode):
        raise UploadValidationError("upload path must name a regular file")

    try:
        resolved_root = root.resolve(strict=True)
        resolved_parent = candidate.parent.resolve(strict=True)
        resolved_parent.relative_to(resolved_root)
    except (OSError, ValueError) as exc:
        raise UploadValidationError("upload file must resolve beneath the uploads root") from exc
    return resolved_parent / candidate.name, tuple(snapshots)


def _assert_path_unchanged(
    snapshots: tuple[tuple[Path, tuple[int, int, int]], ...]
) -> None:
    for path, expected_identity in snapshots:
        info = _path_info(path, unavailable_message="upload path changed during validation")
        if _file_identity(info) != expected_identity:
            raise UploadValidationError("upload path changed or was swapped during validation")


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
    validated_path, path_snapshots = _confined_file(path, root)

    suffix = validated_path.suffix.lower()
    if suffix == ".png":
        mime_type = "image/png"
        size_limit = PNG_MAX_BYTES
    elif suffix == ".webm":
        mime_type = "video/webm"
        size_limit = WEBM_MAX_BYTES
    else:
        raise UploadValidationError("upload file must have a .png or .webm extension")

    try:
        with validated_path.open("rb") as handle:
            before = os.fstat(handle.fileno())
            leaf_identity = path_snapshots[-1][1]
            if not stat.S_ISREG(before.st_mode) or _file_identity(before) != leaf_identity:
                raise UploadValidationError(
                    "upload file changed or was swapped before it could be opened"
                )
            _assert_path_unchanged(path_snapshots)
            if before.st_size > size_limit:
                raise UploadValidationError(
                    f"upload file exceeds the {size_limit}-byte limit for {suffix}"
                )
            content = handle.read(size_limit)
            after = os.fstat(handle.fileno())
            _assert_path_unchanged(path_snapshots)
    except OSError as exc:
        raise UploadValidationError("upload file could not be read") from exc

    if _file_identity(after) != _file_identity(before):
        raise UploadValidationError("upload file changed or was swapped while being read")
    if len(content) != before.st_size or len(content) != after.st_size:
        if after.st_size > size_limit:
            raise UploadValidationError(
                f"upload file exceeds the {size_limit}-byte limit for {suffix}"
            )
        raise UploadValidationError("upload file changed while being read")
    size_bytes = len(content)
    if size_bytes == 0:
        raise UploadValidationError("upload file must not be empty")
    if size_bytes > size_limit:
        raise UploadValidationError(
            f"upload file exceeds the {size_limit}-byte limit for {suffix}"
        )

    if suffix == ".png" and not content.startswith(_PNG_SIGNATURE):
        raise UploadValidationError("upload file does not have a valid PNG signature")
    if suffix == ".webm" and not _has_webm_doctype(content):
        raise UploadValidationError("upload file does not have a valid WebM DocType")

    return UploadFile(
        path=validated_path,
        filename=validated_path.name,
        mime_type=mime_type,
        size_bytes=size_bytes,
        content=content,
    )


def _validate_linear_url(value: Any, field_name: str, *, allow_query: bool) -> str:
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
        or (parsed.query and not allow_query)
    ):
        raise UploadValidationError(
            f"{field_name} must use the exact https://{_LINEAR_UPLOAD_HOST} host"
        )
    return value


def _validated_headers(value: Any) -> Mapping[str, str]:
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
            or any(not 0x20 <= ord(character) <= 0x7E for character in header_value)
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
    return MappingProxyType(normalized)


def validate_upload_target(upload_file: Mapping[str, Any]) -> ValidatedTarget:
    """Validate URLs and provider headers returned by Linear's fileUpload mutation."""

    if not isinstance(upload_file, Mapping):
        raise UploadValidationError("Linear upload target must be an object")
    upload_url = _validate_linear_url(
        upload_file.get("uploadUrl"), "uploadUrl", allow_query=True
    )
    asset_url = _validate_linear_url(
        upload_file.get("assetUrl"), "assetUrl", allow_query=False
    )
    if upload_url == asset_url:
        raise UploadValidationError("uploadUrl and assetUrl must be different")
    headers = _validated_headers(upload_file.get("headers"))
    return ValidatedTarget(
        upload_url=upload_url,
        asset_url=asset_url,
        headers=headers,
    )


def put_upload(target: ValidatedTarget, upload_file: UploadFile) -> str:
    """PUT a validated byte snapshot to a validated Linear upload target."""

    headers = {
        "Content-Type": upload_file.mime_type,
        "Cache-Control": "public, max-age=31536000",
        **target.headers,
    }
    failed = False
    try:
        with httpx.Client(follow_redirects=False) as upload_client:
            response = upload_client.put(
                target.upload_url,
                content=upload_file.content,
                headers=headers,
            )
            response.raise_for_status()
    except httpx.HTTPError:
        failed = True

    if failed:
        # Raise outside the handler so the provider exception (which may contain
        # the signed URL and headers) is not attached as a visible cause.
        raise UploadError("Linear evidence upload failed")
    return target.asset_url
