"""Tests for the attachment extraction and download flow.

Covers:
1. POST /agent/messages with inline base64 image/document parts
   → stores binary in attachments table, replaces parts with attachment_ref
2. GET /agent/attachments?thread_key=... lists stored attachments
3. GET /agent/attachments/{id}/download returns the raw binary
4. harness_protocol.messages_to_content_blocks converts attachment_ref
   into a curl download instruction for the sandbox agent
"""

from __future__ import annotations

import base64
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import pytest

from api.runtime_control import extract_inline_attachments, event_to_chat_parts
from api.sandbox.harness_protocol import messages_to_content_blocks


# ── Unit: harness_protocol attachment_ref handling ──────────────────────────


class TestAttachmentRefConversion:
    def test_attachment_ref_produces_curl_instruction(self):
        messages = [
            {
                "role": "user",
                "parts": [
                    {"type": "text", "text": "analyze this"},
                    {
                        "type": "attachment_ref",
                        "id": "att-abc123",
                        "name": "report.pdf",
                        "mime_type": "application/pdf",
                    },
                ],
            }
        ]
        blocks = messages_to_content_blocks(messages)
        assert len(blocks) == 2
        assert blocks[0] == {"type": "text", "text": "analyze this"}
        assert "att-abc123" in blocks[1]["text"]
        assert "curl" in blocks[1]["text"]
        assert "report.pdf" in blocks[1]["text"]
        assert "/agent/attachments/att-abc123/download" in blocks[1]["text"]

    def test_attachment_ref_with_user_attribution(self):
        messages = [
            {
                "role": "user",
                "user_id": "U123",
                "parts": [
                    {"type": "text", "text": "check this file"},
                    {
                        "type": "attachment_ref",
                        "id": "att-xyz",
                        "name": "data.csv",
                        "mime_type": "text/csv",
                    },
                ],
            }
        ]
        blocks = messages_to_content_blocks(messages)
        assert blocks[0]["text"].startswith("<@U123>:")
        assert "att-xyz" in blocks[1]["text"]


# ── Unit: attachment extraction pipeline ────────────────────────────────────
#
# The slackbot buffers messages with attachments via POST /agent/messages,
# which calls extract_inline_attachments() (api.runtime_control) to store
# base64 blobs in the attachments table and replace them with lightweight
# attachment_ref parts.  On flush, messages_to_content_blocks converts
# attachment_ref → text (curl download instructions) so the sandbox agent
# can download the files.


class TestAttachmentExtractionPipeline:
    """Validates the extraction pipeline: base64 → attachment_ref → curl text.

    Without this pipeline, raw image/document content blocks would reach the
    sandbox, which only accepts text blocks and would reject them.
    """

    def test_document_block_would_fail_without_extraction(self):
        """Raw document blocks pass through messages_to_content_blocks unchanged.

        This proves that without the extraction step, the sandbox would
        receive a non-text block and reject it.
        """
        messages = [
            {
                "role": "user",
                "parts": [
                    {"type": "text", "text": "summarize this"},
                    {
                        "type": "document",
                        "source": {
                            "type": "base64",
                            "media_type": "application/pdf",
                            "data": "JVBER...",
                        },
                    },
                ],
            }
        ]
        blocks = messages_to_content_blocks(messages)
        assert len(blocks) == 2
        assert blocks[0] == {"type": "text", "text": "summarize this"}
        # The document block passes through as-is — NOT a text block
        assert blocks[1]["type"] == "document"
        assert "source" in blocks[1]

    @pytest.mark.asyncio
    async def test_video_file_block_is_extracted_to_attachment_ref(self):
        video_bytes = b"\x00\x00\x00\x18ftypmp42" + (b"video" * 16)
        inserted: list[tuple] = []

        class FakePool:
            async def execute(self, query, *args):
                inserted.append(args)

        transformed, attachment_ids = await extract_inline_attachments(
            FakePool(),
            thread_key="slack:T:C:1.000000",
            chat_message_id="msg-video",
            parts=[
                {"type": "text", "text": "what happens here?"},
                {
                    "type": "file",
                    "name": "CleanShot demo.mp4",
                    "mime_type": "video/mp4",
                    "size": len(video_bytes),
                    "slack_file_id": "FVID123",
                    "source": {
                        "type": "base64",
                        "media_type": "video/mp4",
                        "data": base64.b64encode(video_bytes).decode(),
                    },
                },
            ],
        )

        assert len(attachment_ids) == 1
        assert transformed == [
            {"type": "text", "text": "what happens here?"},
            {
                "type": "attachment_ref",
                "attachment_id": attachment_ids[0],
                "media_type": "video/mp4",
                "name": "CleanShot demo.mp4",
            },
        ]
        assert inserted[0][1] == "slack:T:C:1.000000"
        assert inserted[0][2] == "msg-video"
        assert inserted[0][3] == "CleanShot demo.mp4"
        assert inserted[0][4] == "video/mp4"
        assert inserted[0][5] == video_bytes

        chat_parts = event_to_chat_parts(transformed)
        assert chat_parts[1] == {
            "type": "attachment_ref",
            "id": attachment_ids[0],
            "name": "CleanShot demo.mp4",
            "mime_type": "video/mp4",
        }

    def test_extracted_attachment_ref_becomes_download_instruction(self):
        """After extraction, attachment_ref parts become text with curl."""
        messages = [
            {
                "role": "user",
                "parts": [
                    {
                        "type": "attachment_ref",
                        "id": "att-deadbeef1234",
                        "name": "slides.pdf",
                        "mime_type": "application/pdf",
                    },
                    {
                        "type": "attachment_ref",
                        "id": "att-cafebabe5678",
                        "name": "screenshot.png",
                        "mime_type": "image/png",
                    },
                ],
            }
        ]
        blocks = messages_to_content_blocks(messages)
        assert len(blocks) == 2
        for block in blocks:
            assert block["type"] == "text", (
                "All blocks should be text after extraction + conversion"
            )
            assert "curl" in block["text"]
        assert "att-deadbeef1234" in blocks[0]["text"]
        assert "slides.pdf" in blocks[0]["text"]
        assert "att-cafebabe5678" in blocks[1]["text"]
        assert "screenshot.png" in blocks[1]["text"]

    def test_mixed_text_and_document_extraction_flow(self):
        """End-to-end: text + document → after extraction, all blocks are text."""
        # Simulate the state AFTER extract_inline_attachments has run:
        # the original document part has been replaced with attachment_ref.
        messages = [
            {
                "role": "user",
                "parts": [
                    {"type": "text", "text": "please review this contract"},
                    {
                        "type": "attachment_ref",
                        "id": "att-contract99",
                        "name": "contract.pdf",
                        "mime_type": "application/pdf",
                    },
                ],
            }
        ]
        blocks = messages_to_content_blocks(messages)
        assert len(blocks) == 2
        # Text block preserved
        assert blocks[0] == {"type": "text", "text": "please review this contract"}
        # Attachment ref became a text download instruction
        assert blocks[1]["type"] == "text"
        assert "contract.pdf" in blocks[1]["text"]
        assert "/agent/attachments/att-contract99/download" in blocks[1]["text"]
        assert "curl" in blocks[1]["text"]

    @pytest.mark.asyncio
    async def test_large_file_block_extracts_to_attachment_ref(self):
        """Multi-MB generic file blocks are extracted before sandbox delivery."""
        large_zip = b"PK\x03\x04" + (b"x" * (2 * 1024 * 1024))
        parts = [
            {"type": "text", "text": "inspect this larger archive"},
            {
                "type": "file",
                "source_path": "file:///tmp/large-archive.zip",
                "source": {
                    "type": "base64",
                    "media_type": "application/zip",
                    "data": base64.b64encode(large_zip).decode(),
                },
            },
        ]
        pool = RecordingAttachmentPool()

        transformed, attachment_ids = await extract_inline_attachments(
            pool,
            thread_key="test:att-file-large-unit",
            chat_message_id="msg-large-file",
            parts=parts,
        )

        assert len(attachment_ids) == 1
        assert transformed == [
            {"type": "text", "text": "inspect this larger archive"},
            {
                "type": "attachment_ref",
                "attachment_id": attachment_ids[0],
                "media_type": "application/zip",
                "name": "large-archive.zip",
                "source_path": "file:///tmp/large-archive.zip",
            },
        ]
        assert len(pool.inserts) == 1
        insert = pool.inserts[0]
        assert insert["name"] == "large-archive.zip"
        assert insert["mime_type"] == "application/zip"
        assert insert["data"] == large_zip


class RecordingAttachmentPool:
    def __init__(self):
        self.inserts: list[dict[str, object]] = []

    async def execute(
        self,
        _sql: str,
        attachment_id: str,
        thread_key: str,
        message_id: str,
        name: str,
        mime_type: str,
        data: bytes,
    ) -> None:
        self.inserts.append(
            {
                "id": attachment_id,
                "thread_key": thread_key,
                "message_id": message_id,
                "name": name,
                "mime_type": mime_type,
                "data": data,
            }
        )


# ── Integration: full roundtrip through the API ────────────────────────────

SAMPLE_PNG = b"\x89PNG\r\n\x1a\n" + b"\x00" * 100  # fake PNG bytes
SAMPLE_PDF = b"%PDF-1.4 fake content for testing"
SAMPLE_ZIP = b"PK\x03\x04 fake zip content for testing"


async def _seed_assignment(db_pool, thread_key: str, generation: int = 1) -> None:
    await db_pool.execute(
        "INSERT INTO agent_runtime_assignments ("
        "thread_key, assignment_generation, runtime_id, harness, engine, "
        "persona_id, prompt_ref, effective_agents_md_sha256, state"
        ") VALUES ($1, $2, $3, 'amp', 'amp', NULL, 'harness:amp', 'sha', 'active')",
        thread_key,
        generation,
        f"rt-{thread_key}-{generation}",
    )


@pytest.mark.asyncio
async def test_attachment_roundtrip(client, db_pool, api_key):
    """POST message with base64 image → stored in attachments → downloadable."""
    thread_key = "test:att-roundtrip"
    await _seed_assignment(db_pool, thread_key)
    b64_png = base64.b64encode(SAMPLE_PNG).decode()

    # 1. Buffer a message with an inline base64 image
    resp = await client.post(
        "/agent/messages",
        json={
            "thread_key": thread_key,
            "assignment_generation": 1,
            "messages": [
                {
                    "role": "user",
                    "parts": [
                        {"type": "text", "text": "what is in this image?"},
                        {
                            "type": "image",
                            "source_path": "file:///tmp/image.png",
                            "source": {
                                "type": "base64",
                                "media_type": "image/png",
                                "data": b64_png,
                            },
                        },
                    ],
                }
            ],
        },
        headers={"Authorization": f"Bearer {api_key}"},
    )
    assert resp.status_code == 200
    body = resp.json()
    assert body["inserted"] == 1

    # 2. List attachments for the thread
    resp = await client.get(
        f"/agent/attachments?thread_key={thread_key}",
        headers={"Authorization": f"Bearer {api_key}"},
    )
    assert resp.status_code == 200
    attachments = resp.json()
    assert len(attachments) == 1
    att = attachments[0]
    assert att["mime_type"] == "image/png"
    assert att["name"] == "image.png"
    att_id = att["id"]

    # 3. Download the attachment
    resp = await client.get(
        f"/agent/attachments/{att_id}/download",
        headers={"Authorization": f"Bearer {api_key}"},
    )
    assert resp.status_code == 200
    assert resp.content == SAMPLE_PNG
    assert resp.headers["content-type"] == "image/png"

    # 4. Verify chat_messages stores attachment_ref, not the base64 blob
    resp = await client.get(
        f"/agent/messages?thread_key={thread_key}",
        headers={"Authorization": f"Bearer {api_key}"},
    )
    assert resp.status_code == 200
    messages = resp.json()["messages"]
    assert len(messages) == 1
    parts = messages[0]["parts"]
    assert parts[0]["type"] == "text"
    assert parts[1]["type"] == "attachment_ref"
    assert parts[1]["id"] == att_id
    # Confirm no base64 data leaked into chat_messages
    assert "data" not in parts[1]
    assert "source" not in parts[1]


@pytest.mark.asyncio
async def test_document_attachment(client, db_pool, api_key):
    """POST message with base64 document part → stored and downloadable."""
    thread_key = "test:att-doc"
    await _seed_assignment(db_pool, thread_key)
    b64_pdf = base64.b64encode(SAMPLE_PDF).decode()

    resp = await client.post(
        "/agent/messages",
        json={
            "thread_key": thread_key,
            "assignment_generation": 1,
            "messages": [
                {
                    "role": "user",
                    "parts": [
                        {"type": "text", "text": "summarize this PDF"},
                        {
                            "type": "document",
                            "source_path": "file:///tmp/document.pdf",
                            "source": {
                                "type": "base64",
                                "media_type": "application/pdf",
                                "data": b64_pdf,
                            },
                        },
                    ],
                }
            ],
        },
        headers={"Authorization": f"Bearer {api_key}"},
    )
    assert resp.status_code == 200

    resp = await client.get(
        f"/agent/attachments?thread_key={thread_key}",
        headers={"Authorization": f"Bearer {api_key}"},
    )
    attachments = resp.json()
    assert len(attachments) == 1
    assert attachments[0]["mime_type"] == "application/pdf"

    resp = await client.get(
        f"/agent/attachments/{attachments[0]['id']}/download",
        headers={"Authorization": f"Bearer {api_key}"},
    )
    assert resp.content == SAMPLE_PDF


@pytest.mark.asyncio
async def test_generic_file_attachment(client, db_pool, api_key):
    """POST message with base64 file part → stored and downloadable."""
    thread_key = "test:att-file"
    await _seed_assignment(db_pool, thread_key)
    b64_zip = base64.b64encode(SAMPLE_ZIP).decode()

    resp = await client.post(
        "/agent/messages",
        json={
            "thread_key": thread_key,
            "assignment_generation": 1,
            "messages": [
                {
                    "role": "user",
                    "parts": [
                        {"type": "text", "text": "inspect this archive"},
                        {
                            "type": "file",
                            "source_path": "file:///tmp/archive.zip",
                            "source": {
                                "type": "base64",
                                "media_type": "application/zip",
                                "data": b64_zip,
                            },
                        },
                    ],
                }
            ],
        },
        headers={"Authorization": f"Bearer {api_key}"},
    )
    assert resp.status_code == 200

    resp = await client.get(
        f"/agent/attachments?thread_key={thread_key}",
        headers={"Authorization": f"Bearer {api_key}"},
    )
    attachments = resp.json()
    assert len(attachments) == 1
    assert attachments[0]["name"] == "archive.zip"
    assert attachments[0]["mime_type"] == "application/zip"

    resp = await client.get(
        f"/agent/attachments/{attachments[0]['id']}/download",
        headers={"Authorization": f"Bearer {api_key}"},
    )
    assert resp.content == SAMPLE_ZIP

    resp = await client.get(
        f"/agent/messages?thread_key={thread_key}",
        headers={"Authorization": f"Bearer {api_key}"},
    )
    parts = resp.json()["messages"][0]["parts"]
    assert parts[0]["type"] == "text"
    assert parts[1]["type"] == "attachment_ref"
    assert parts[1]["name"] == "archive.zip"
    assert parts[1]["mime_type"] == "application/zip"


@pytest.mark.asyncio
async def test_large_generic_file_attachment(client, db_pool, api_key):
    """POST message with multi-MB file part → stored and downloadable."""
    thread_key = "test:att-file-large"
    await _seed_assignment(db_pool, thread_key)
    large_zip = b"PK\x03\x04" + (b"x" * (2 * 1024 * 1024))
    b64_zip = base64.b64encode(large_zip).decode()

    resp = await client.post(
        "/agent/messages",
        json={
            "thread_key": thread_key,
            "assignment_generation": 1,
            "messages": [
                {
                    "role": "user",
                    "parts": [
                        {"type": "text", "text": "inspect this larger archive"},
                        {
                            "type": "file",
                            "source_path": "file:///tmp/large-archive.zip",
                            "source": {
                                "type": "base64",
                                "media_type": "application/zip",
                                "data": b64_zip,
                            },
                        },
                    ],
                }
            ],
        },
        headers={"Authorization": f"Bearer {api_key}"},
    )
    assert resp.status_code == 200

    resp = await client.get(
        f"/agent/attachments?thread_key={thread_key}",
        headers={"Authorization": f"Bearer {api_key}"},
    )
    attachments = resp.json()
    assert len(attachments) == 1
    assert attachments[0]["name"] == "large-archive.zip"
    assert attachments[0]["mime_type"] == "application/zip"

    resp = await client.get(
        f"/agent/attachments/{attachments[0]['id']}/download",
        headers={"Authorization": f"Bearer {api_key}"},
    )
    assert resp.content == large_zip


@pytest.mark.asyncio
async def test_text_only_message_no_attachments(client, db_pool, api_key):
    """Text-only messages pass through without creating attachments."""
    thread_key = "test:att-textonly"
    await _seed_assignment(db_pool, thread_key)

    resp = await client.post(
        "/agent/messages",
        json={
            "thread_key": thread_key,
            "assignment_generation": 1,
            "messages": [
                {
                    "role": "user",
                    "parts": [{"type": "text", "text": "just plain text"}],
                }
            ],
        },
        headers={"Authorization": f"Bearer {api_key}"},
    )
    assert resp.status_code == 200

    resp = await client.get(
        f"/agent/attachments?thread_key={thread_key}",
        headers={"Authorization": f"Bearer {api_key}"},
    )
    assert resp.json() == []


@pytest.mark.asyncio
async def test_mixed_attachments_and_text(client, db_pool, api_key):
    """Message with text + image + document → two attachments, text preserved."""
    thread_key = "test:att-mixed"
    await _seed_assignment(db_pool, thread_key)
    b64_png = base64.b64encode(SAMPLE_PNG).decode()
    b64_pdf = base64.b64encode(SAMPLE_PDF).decode()

    resp = await client.post(
        "/agent/messages",
        json={
            "thread_key": thread_key,
            "assignment_generation": 1,
            "messages": [
                {
                    "role": "user",
                    "parts": [
                        {"type": "text", "text": "review both files"},
                        {
                            "type": "image",
                            "source_path": "file:///tmp/image.png",
                            "source": {
                                "type": "base64",
                                "media_type": "image/png",
                                "data": b64_png,
                            },
                        },
                        {
                            "type": "document",
                            "source_path": "file:///tmp/document.pdf",
                            "source": {
                                "type": "base64",
                                "media_type": "application/pdf",
                                "data": b64_pdf,
                            },
                        },
                    ],
                }
            ],
        },
        headers={"Authorization": f"Bearer {api_key}"},
    )
    assert resp.status_code == 200

    resp = await client.get(
        f"/agent/attachments?thread_key={thread_key}",
        headers={"Authorization": f"Bearer {api_key}"},
    )
    attachments = resp.json()
    assert len(attachments) == 2
    mime_types = {a["mime_type"] for a in attachments}
    assert mime_types == {"image/png", "application/pdf"}

    # Verify parts in chat_messages
    resp = await client.get(
        f"/agent/messages?thread_key={thread_key}",
        headers={"Authorization": f"Bearer {api_key}"},
    )
    parts = resp.json()["messages"][0]["parts"]
    assert parts[0]["type"] == "text"
    assert parts[1]["type"] == "attachment_ref"
    assert parts[2]["type"] == "attachment_ref"


@pytest.mark.asyncio
async def test_nonexistent_attachment_404(client, api_key):
    """Downloading a non-existent attachment returns 404."""
    resp = await client.get(
        "/agent/attachments/att-doesnotexist/download",
        headers={"Authorization": f"Bearer {api_key}"},
    )
    assert resp.status_code == 404


@pytest.mark.asyncio
async def test_download_attachment_allows_cross_thread_reads_by_default(monkeypatch):
    """A sandbox token may download another thread's attachment by default.

    Cross-thread reads are relaxed so anyone holding a thread link can view its
    attachments. Exercises the handler directly: the HTTP path can't be used
    because a test client never carries sandbox claims.
    """
    import types

    from fastapi import HTTPException

    from api.routers.attachments import download_attachment

    monkeypatch.delenv("SANDBOX_CROSS_THREAD_READS", raising=False)

    row = {
        "data": SAMPLE_PNG,
        "mime_type": "image/png",
        "name": "secret.png",
        "thread_key": "test:owner-thread",
    }

    class _Pool:
        async def fetchrow(self, _sql, _attachment_id):
            return row

    def _request(sandbox_claims):
        return types.SimpleNamespace(
            app=types.SimpleNamespace(state=types.SimpleNamespace(db_pool=_Pool())),
            state=types.SimpleNamespace(sandbox_claims=sandbox_claims),
        )

    # Sandbox token scoped to a different thread → still serves the bytes
    resp = await download_attachment(
        _request({"thread_key": "test:other-thread"}), "att-x"
    )
    assert resp.status_code == 200
    assert resp.body == SAMPLE_PNG

    # Sandbox token scoped to the owning thread → serves the bytes
    resp = await download_attachment(
        _request({"thread_key": "test:owner-thread"}), "att-x"
    )
    assert resp.status_code == 200
    assert resp.body == SAMPLE_PNG

    # Non-sandbox caller (no claims) → unaffected
    resp = await download_attachment(_request(None), "att-x")
    assert resp.status_code == 200

    # Explicit thread_key must match the attachment's thread, regardless of
    # token type (used by privileged callers acting for an agent).
    with pytest.raises(HTTPException) as excinfo:
        await download_attachment(
            _request(None), "att-x", thread_key="test:other-thread"
        )
    assert excinfo.value.status_code == 403

    resp = await download_attachment(
        _request(None), "att-x", thread_key="test:owner-thread"
    )
    assert resp.status_code == 200


@pytest.mark.asyncio
async def test_download_attachment_enforces_thread_scope_when_reads_locked(monkeypatch):
    """With SANDBOX_CROSS_THREAD_READS disabled, a sandbox token is confined."""
    import types

    from fastapi import HTTPException

    from api.routers.attachments import download_attachment

    monkeypatch.setenv("SANDBOX_CROSS_THREAD_READS", "0")

    row = {
        "data": SAMPLE_PNG,
        "mime_type": "image/png",
        "name": "secret.png",
        "thread_key": "test:owner-thread",
    }

    class _Pool:
        async def fetchrow(self, _sql, _attachment_id):
            return row

    def _request(sandbox_claims):
        return types.SimpleNamespace(
            app=types.SimpleNamespace(state=types.SimpleNamespace(db_pool=_Pool())),
            state=types.SimpleNamespace(sandbox_claims=sandbox_claims),
        )

    # Sandbox token scoped to a different thread → 403
    with pytest.raises(HTTPException) as excinfo:
        await download_attachment(
            _request({"thread_key": "test:other-thread"}), "att-x"
        )
    assert excinfo.value.status_code == 403

    # Sandbox token scoped to the owning thread → serves the bytes
    resp = await download_attachment(
        _request({"thread_key": "test:owner-thread"}), "att-x"
    )
    assert resp.status_code == 200
    assert resp.body == SAMPLE_PNG


@pytest.mark.asyncio
async def test_download_attachment_handles_non_ascii_filename():
    """A filename with non-ASCII or quote characters must not 500 the response.

    Starlette encodes headers as latin-1, so the handler needs to emit an RFC
    5987 Content-Disposition rather than dropping the raw name into the header.
    """
    import types

    from api.routers.attachments import download_attachment

    class _Pool:
        def __init__(self, name: str):
            self._name = name

        async def fetchrow(self, _sql, _attachment_id):
            return {
                "data": SAMPLE_PNG,
                "mime_type": "image/png",
                "name": self._name,
                "thread_key": "test:owner-thread",
            }

    for tricky_name in ["截图.png", 'has"quote.png', "résumé.pdf"]:
        request = types.SimpleNamespace(
            app=types.SimpleNamespace(state=types.SimpleNamespace(db_pool=_Pool(tricky_name))),
            state=types.SimpleNamespace(sandbox_claims=None),
        )
        resp = await download_attachment(request, "att-x")
        assert resp.status_code == 200
        # Confirm the response can actually be serialized to the wire.
        for _, value in resp.raw_headers:
            assert isinstance(value, bytes)


# ── Integration: upload endpoint roundtrip ─────────────────────────────────


@pytest.mark.asyncio
async def test_upload_attachment_roundtrip(client, api_key):
    """POST /agent/attachments/upload → download → list roundtrip."""
    thread_key = "test:att-upload-rt"
    b64_png = base64.b64encode(SAMPLE_PNG).decode()

    # 1. Upload
    resp = await client.post(
        "/agent/attachments/upload",
        json={
            "thread_key": thread_key,
            "name": "screenshot.png",
            "mime_type": "image/png",
            "data": b64_png,
        },
        headers={"Authorization": f"Bearer {api_key}"},
    )
    assert resp.status_code == 200
    body = resp.json()
    assert "id" in body
    assert body["name"] == "screenshot.png"
    assert body["mime_type"] == "image/png"
    assert "download_url" in body

    # 2. Download via the returned URL
    resp = await client.get(
        body["download_url"],
        headers={"Authorization": f"Bearer {api_key}"},
    )
    assert resp.status_code == 200
    assert resp.content == SAMPLE_PNG

    # 3. List attachments for the thread
    resp = await client.get(
        f"/agent/attachments?thread_key={thread_key}",
        headers={"Authorization": f"Bearer {api_key}"},
    )
    assert resp.status_code == 200
    attachments = resp.json()
    assert any(a["id"] == body["id"] for a in attachments)


@pytest.mark.asyncio
async def test_upload_attachment_missing_fields(client, api_key):
    """POST /agent/attachments/upload with missing data field → 422."""
    resp = await client.post(
        "/agent/attachments/upload",
        json={
            "thread_key": "test:att-missing",
            "name": "test.png",
            "mime_type": "image/png",
            # no "data" field
        },
        headers={"Authorization": f"Bearer {api_key}"},
    )
    assert resp.status_code == 422


@pytest.mark.asyncio
async def test_upload_attachment_bad_base64(client, api_key):
    """POST /agent/attachments/upload with invalid base64 → 422."""
    resp = await client.post(
        "/agent/attachments/upload",
        json={
            "thread_key": "test:att-bad-b64",
            "name": "test.pdf",
            "mime_type": "application/pdf",
            "data": "not-valid-base64!!!",
        },
        headers={"Authorization": f"Bearer {api_key}"},
    )
    assert resp.status_code == 422
