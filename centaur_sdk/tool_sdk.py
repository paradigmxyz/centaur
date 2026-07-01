"""Tool SDK — what tool authors import."""

from __future__ import annotations

import base64
import contextlib
import json
import logging
import mimetypes
import os
import urllib.request
import uuid
from contextvars import ContextVar
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any
from urllib.parse import quote

log = logging.getLogger(__name__)


@dataclass
class ToolContext:
    name: str
    secrets: dict[str, str] = field(default_factory=dict)
    thread_key: str | None = None
    container_id: str | None = None


_tool_ctx: ContextVar[ToolContext] = ContextVar("_tool_ctx")


def set_tool_context(ctx: ToolContext) -> Any:
    return _tool_ctx.set(ctx)


def reset_tool_context(token: Any) -> None:
    _tool_ctx.reset(token)


def get_tool_context() -> ToolContext:
    return _tool_ctx.get()


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------


def secret(key: str, default: str | None = None) -> str:
    """Get a secret. Resolution order: tool context → pluggable backend → default.

    - **ToolContext**: Set by ToolManager, populated from .env files (if any).
    - **Pluggable backend**: Configured via ``centaur_sdk.backends.registry``
      (env vars, HTTP sidecar, etc.).
    """
    # 1. Check tool context if available (server mode)
    try:
        ctx = _tool_ctx.get()
        val = ctx.secrets.get(key)
        if val is not None:
            return val
    except LookupError:
        pass

    # 2. Pluggable secret backend
    from centaur_sdk.backends.registry import get_backend

    val = get_backend().get_sync(key)
    if val is not None:
        return val

    if default is not None:
        return default

    ctx_name = ""
    with contextlib.suppress(LookupError):
        ctx_name = f" for tool '{_tool_ctx.get().name}'"
    raise KeyError(f"Missing secret '{key}'{ctx_name}")


def current_thread_key() -> str:
    """Return the active thread key for a tool call."""
    try:
        thread_key = _tool_ctx.get().thread_key
    except LookupError:
        thread_key = None
    if not thread_key:
        raise RuntimeError(
            "this operation must run inside a scoped thread: no thread_key "
            "in the tool context."
        )
    return thread_key


def current_session_context() -> dict[str, Any]:
    """Return API-owned context for the current thread.

    For Slack-originated sessions this includes ``slack.channel_id`` and
    ``slack.thread_ts``. The API remains the source of truth so warm pooled
    sandboxes do not need per-thread environment mutation.
    """
    thread_key = current_thread_key()
    base_url = secret("CENTAUR_API_URL", "http://api:8000").rstrip("/")
    headers: dict[str, str] = {}
    api_key = secret("CENTAUR_API_KEY", "").strip()
    if api_key:
        headers["Authorization"] = f"Bearer {api_key}"
    request = urllib.request.Request(
        f"{base_url}/api/session/{quote(thread_key, safe='')}",
        headers=headers,
        method="GET",
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.loads(response.read())


def current_slack_thread() -> dict[str, str]:
    """Return ``{"channel_id": ..., "thread_ts": ...}`` for the current Slack thread."""
    context = current_session_context()
    slack = context.get("slack")
    if not isinstance(slack, dict) or not slack.get("channel_id") or not slack.get("thread_ts"):
        raise RuntimeError(f"current thread is not a Slack thread: {context.get('thread_key')!r}")
    return {
        "channel_id": str(slack["channel_id"]),
        "thread_ts": str(slack["thread_ts"]),
    }


def _sandbox_uploads_dir() -> Path | None:
    configured = os.environ.get("CENTAUR_UPLOADS_DIR", "").strip()
    if configured:
        return Path(configured)
    if os.environ.get("CENTAUR_THREAD_KEY", "").strip():
        return Path.home() / "uploads"
    return None


def _unique_upload_path(uploads_dir: Path, name: str) -> Path:
    candidate = uploads_dir / name
    if not candidate.exists():
        return candidate
    suffix = candidate.suffix
    stem = candidate.stem or "attachment"
    return uploads_dir / f"{stem}-{uuid.uuid4().hex}{suffix}"


def _save_local_attachment(
    *,
    name: str,
    data: bytes,
    mime_type: str,
    source_url: str | None,
    uploads_dir: Path,
) -> dict[str, Any]:
    uploads_dir.mkdir(parents=True, exist_ok=True)
    path = _unique_upload_path(uploads_dir, name)
    path.write_bytes(data)
    return {
        "attachment_id": None,
        "filename": name,
        "mime_type": mime_type,
        "download_url": None,
        "path": str(path),
        "local_path": str(path),
        "source_url": source_url,
        "size_bytes": len(data),
    }


def save_attachment(
    *,
    name: str,
    data: bytes,
    mime_type: str | None = None,
    source_url: str | None = None,
) -> dict[str, Any]:
    """Persist bytes as a Centaur attachment scoped to the current tool thread."""
    safe_name = Path(name).name or "attachment"
    resolved_mime = mime_type or mimetypes.guess_type(safe_name)[0] or "application/octet-stream"
    uploads_dir = _sandbox_uploads_dir()
    if uploads_dir is not None:
        return _save_local_attachment(
            name=safe_name,
            data=data,
            mime_type=resolved_mime,
            source_url=source_url,
            uploads_dir=uploads_dir,
        )

    thread_key = current_thread_key()
    base_url = secret("CENTAUR_API_URL", "http://api:8000").rstrip("/")
    payload = json.dumps(
        {
            "thread_key": thread_key,
            "name": safe_name,
            "mime_type": resolved_mime,
            "data": base64.b64encode(data).decode("ascii"),
            "source_url": source_url,
        }
    ).encode()
    headers = {"Content-Type": "application/json"}
    request = urllib.request.Request(
        f"{base_url}/agent/attachments/upload",
        data=payload,
        headers=headers,
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        result = json.loads(response.read())
    attachment_id = result.get("id")
    if not attachment_id:
        raise RuntimeError(f"attachment upload returned no id: {result!r}")
    return {
        "attachment_id": attachment_id,
        "filename": result.get("name") or safe_name,
        "mime_type": result.get("mime_type") or resolved_mime,
        "download_url": result.get("download_url"),
        "size_bytes": len(data),
    }


def save_attachment_from_path(
    path: str | Path,
    *,
    name: str | None = None,
    mime_type: str | None = None,
    source_url: str | None = None,
) -> dict[str, Any]:
    """Persist a local file as a thread-scoped Centaur attachment."""
    p = Path(path)
    return save_attachment(
        name=name or p.name,
        data=p.read_bytes(),
        mime_type=mime_type,
        source_url=source_url,
    )
