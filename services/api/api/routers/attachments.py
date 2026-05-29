"""Attachments router — download attachments from sandbox agents."""

from __future__ import annotations

import base64
import uuid

import structlog
from fastapi import APIRouter, Depends, HTTPException, Request
from fastapi.responses import Response

from api.deps import enforce_sandbox_thread_scope, verify_api_key

log = structlog.get_logger()

router = APIRouter(
    prefix="/agent/attachments",
    tags=["attachments"],
    dependencies=[Depends(verify_api_key)],
)


def _ascii_filename(name: str) -> str:
    """Strip non-ASCII chars and `"` so the name is safe in a latin-1 header."""
    return name.encode("ascii", "ignore").decode("ascii").replace('"', "")


@router.get("")
async def list_attachments(request: Request, thread_key: str):
    """List attachment metadata for a thread."""
    enforce_sandbox_thread_scope(request, thread_key, write=False)
    pool = request.app.state.db_pool
    rows = await pool.fetch(
        "SELECT id, thread_key, message_id, name, mime_type, created_at "
        "FROM attachments WHERE thread_key = $1 ORDER BY created_at",
        thread_key,
    )
    return [
        {
            "id": row["id"],
            "name": row["name"],
            "mime_type": row["mime_type"],
            "message_id": row["message_id"],
            "created_at": row["created_at"].isoformat() if row["created_at"] else None,
        }
        for row in rows
    ]


@router.post("/upload")
async def upload_attachment(request: Request):
    """Upload a file directly to the attachments table.

    Body (JSON):
        thread_key: str (required)
        name: str (required) — filename
        mime_type: str (required)
        data: str (required) — base64-encoded file content
        message_id: str (optional) — associated chat_message id
        source_url: str (optional) — original URL the file was downloaded from
    """
    body = await request.json()

    thread_key = body.get("thread_key")
    name = body.get("name")
    mime_type = body.get("mime_type")
    data_b64 = body.get("data")

    if not thread_key or not name or not mime_type or data_b64 is None:
        raise HTTPException(
            status_code=422,
            detail="thread_key, name, mime_type, and data are required",
        )

    enforce_sandbox_thread_scope(request, thread_key, write=True)

    try:
        raw_bytes = base64.b64decode(data_b64)
    except Exception:
        raise HTTPException(status_code=422, detail="data is not valid base64")

    att_id = f"att-{uuid.uuid4().hex[:16]}"
    message_id = body.get("message_id")
    source_url = body.get("source_url")

    pool = request.app.state.db_pool
    await pool.execute(
        "INSERT INTO attachments (id, thread_key, message_id, name, mime_type, data) "
        "VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (id) DO NOTHING",
        att_id, thread_key, message_id, name, mime_type, raw_bytes,
    )
    log.info(
        "attachment_uploaded",
        id=att_id,
        thread_key=thread_key,
        name=name,
        mime_type=mime_type,
        size=len(raw_bytes),
        source_url=source_url,
    )
    return {
        "id": att_id,
        "name": name,
        "mime_type": mime_type,
        "download_url": f"/agent/attachments/{att_id}/download",
    }


@router.get("/{attachment_id}/download")
async def download_attachment(
    request: Request, attachment_id: str, thread_key: str | None = None
):
    """Download attachment raw bytes.

    When ``thread_key`` is supplied, the attachment must belong to it. This
    lets a privileged caller (e.g. the slack tool acting for an agent, which
    authenticates with a service key rather than a sandbox token) constrain
    the read to the agent's own thread.
    """
    pool = request.app.state.db_pool
    row = await pool.fetchrow(
        "SELECT data, mime_type, name, thread_key FROM attachments WHERE id = $1",
        attachment_id,
    )
    if not row:
        raise HTTPException(status_code=404, detail="Attachment not found")
    # Reject a sandbox token reading an attachment from another thread, unless
    # cross-thread reads are enabled (the default) — a thread link is the
    # capability to view its attachments.
    enforce_sandbox_thread_scope(request, row["thread_key"], write=False)
    # An explicit thread_key constrains the read to that thread, for callers
    # whose key is not a sandbox token (so the check above does not apply).
    if thread_key is not None and row["thread_key"] != thread_key:
        raise HTTPException(
            status_code=403, detail="Attachment does not belong to the requested thread"
        )
    return Response(
        content=row["data"],
        media_type=row["mime_type"],
        headers={"Content-Disposition": f'attachment; filename="{_ascii_filename(row["name"])}"'},
    )
