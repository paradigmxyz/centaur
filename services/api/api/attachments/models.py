"""Attachment normalization models shared by API and workflow code."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Literal

AttachmentKind = Literal[
    "image",
    "document",
    "text",
    "archive",
    "video",
    "audio",
    "file",
    "url",
]


@dataclass(frozen=True)
class AttachmentCandidate:
    """Normalized source attachment before any caller-specific storage policy."""

    source: str
    name: str
    mime_type: str
    kind: AttachmentKind = "file"
    data: bytes | None = None
    text_content: str | None = None
    external_id: str | None = None
    source_url: str | None = None
    source_path: str | None = None
    size_bytes: int | None = None
    metadata: dict[str, Any] = field(default_factory=dict)

    @property
    def effective_size_bytes(self) -> int | None:
        if self.size_bytes is not None:
            return self.size_bytes
        if self.data is not None:
            return len(self.data)
        if self.text_content is not None:
            return len(self.text_content.encode("utf-8"))
        return None


@dataclass(frozen=True)
class StoredAttachment:
    """Attachment persisted in the API-owned thread attachment table."""

    id: str
    thread_key: str
    message_id: str | None
    name: str
    mime_type: str
    size_bytes: int
    source: str | None = None
    source_url: str | None = None
    metadata: dict[str, Any] = field(default_factory=dict)

    @property
    def download_url(self) -> str:
        return f"/agent/attachments/{self.id}/download"
