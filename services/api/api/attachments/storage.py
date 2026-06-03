"""Storage helpers for API-owned durable thread attachments."""

from __future__ import annotations

import contextlib
import pathlib
import uuid
from typing import Any

from api.attachments.models import StoredAttachment


def new_attachment_id() -> str:
    return f"att-{uuid.uuid4().hex[:16]}"


def attachment_name_from_source_path(source_path: str | None, attachment_id: str) -> str:
    if not source_path:
        return f"{attachment_id}.bin"
    with contextlib.suppress(Exception):
        parsed = pathlib.PurePosixPath(source_path)
        if parsed.name:
            return parsed.name
    return f"{attachment_id}.bin"


def safe_attachment_name(name: str | None, *, fallback: str) -> str:
    raw = str(name or "").strip()
    if not raw:
        return fallback
    with contextlib.suppress(Exception):
        parsed = pathlib.PurePosixPath(raw)
        if parsed.name:
            return parsed.name
    return raw


async def insert_thread_attachment(
    conn: Any,
    *,
    thread_key: str,
    message_id: str | None,
    name: str,
    mime_type: str,
    data: bytes,
    attachment_id: str | None = None,
    source: str | None = None,
    source_url: str | None = None,
    metadata: dict[str, Any] | None = None,
) -> StoredAttachment:
    """Insert bytes into the existing ``attachments`` table."""

    att_id = attachment_id or new_attachment_id()
    await conn.execute(
        "INSERT INTO attachments (id, thread_key, message_id, name, mime_type, data) "
        "VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (id) DO NOTHING",
        att_id,
        thread_key,
        message_id,
        name,
        mime_type,
        data,
    )
    return StoredAttachment(
        id=att_id,
        thread_key=thread_key,
        message_id=message_id,
        name=name,
        mime_type=mime_type,
        size_bytes=len(data),
        source=source,
        source_url=source_url,
        metadata=dict(metadata or {}),
    )
