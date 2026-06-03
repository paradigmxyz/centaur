"""Attachment discovery, classification, and current thread extraction logic."""

from __future__ import annotations

import base64
import mimetypes
from typing import Any
from urllib.parse import urlparse

from api.attachments.models import AttachmentCandidate, AttachmentKind, StoredAttachment
from api.attachments.storage import (
    attachment_name_from_source_path,
    insert_thread_attachment,
    new_attachment_id,
    safe_attachment_name,
)


class AttachmentDecodeError(ValueError):
    """Raised when an inline attachment body cannot be decoded."""


class AttachmentProcessor:
    """Normalize source-specific attachment shapes without owning every storage policy."""

    _ARCHIVE_MIME_TYPES = {
        "application/zip",
        "application/x-tar",
        "application/gzip",
        "application/x-gzip",
        "application/x-7z-compressed",
    }
    _DOCUMENT_MIME_FRAGMENTS = (
        "document",
        "spreadsheet",
        "presentation",
        "pdf",
    )

    def classify_mime_type(self, mime_type: str | None, *, name: str | None = None) -> AttachmentKind:
        resolved = self.normalize_mime_type(mime_type, name=name)
        if resolved.startswith("image/"):
            return "image"
        if resolved.startswith("video/"):
            return "video"
        if resolved.startswith("audio/"):
            return "audio"
        if resolved.startswith("text/"):
            return "text"
        if resolved in self._ARCHIVE_MIME_TYPES:
            return "archive"
        if any(fragment in resolved for fragment in self._DOCUMENT_MIME_FRAGMENTS):
            return "document"
        if resolved in {"application/json", "application/xml"}:
            return "text"
        return "file"

    def normalize_mime_type(self, mime_type: str | None, *, name: str | None = None) -> str:
        raw = str(mime_type or "").split(";", 1)[0].strip().lower()
        if raw:
            return raw
        guessed = mimetypes.guess_type(name or "")[0]
        return guessed or "application/octet-stream"

    def detect_url_source(self, url: str) -> dict[str, str] | None:
        """Return a stable source classification for known attachment/document URLs."""

        parsed = urlparse(str(url or "").strip())
        host = (parsed.hostname or "").lower()
        if not parsed.scheme or not host:
            return None
        if host in {"docs.google.com", "drive.google.com"}:
            return {"source": "google_drive_url", "host": host}
        if host.endswith("docsend.com"):
            return {"source": "docsend_url", "host": host}
        if host == "files.slack.com":
            return {"source": "slack_file_url", "host": host}
        return {"source": "url", "host": host}

    def candidate_from_inline_part(self, part: dict[str, Any]) -> AttachmentCandidate | None:
        """Create a candidate from an Anthropic-style base64 content block."""

        source = part.get("source") if isinstance(part, dict) else None
        if not (
            isinstance(source, dict)
            and source.get("type") == "base64"
            and isinstance(source.get("data"), str)
        ):
            return None

        media_type = self.normalize_mime_type(
            str(source.get("media_type") or "application/octet-stream"),
            name=part.get("name") if isinstance(part.get("name"), str) else None,
        )
        attachment_id = new_attachment_id()
        source_path = part.get("source_path") if isinstance(part.get("source_path"), str) else None
        fallback = attachment_name_from_source_path(source_path, attachment_id)
        name = (
            str(part.get("name"))
            if isinstance(part.get("name"), str) and part.get("name")
            else fallback
        )
        try:
            raw = base64.b64decode(source["data"])
        except Exception as exc:
            raise AttachmentDecodeError(f"invalid base64 attachment: {exc}") from exc

        metadata: dict[str, Any] = {}
        for key in ("slack_file_id", "size", "mime_type"):
            if key in part:
                metadata[key] = part[key]

        return AttachmentCandidate(
            source="inline_base64",
            external_id=attachment_id,
            name=name,
            mime_type=media_type,
            kind=self.classify_mime_type(media_type, name=name),
            data=raw,
            source_path=source_path,
            size_bytes=len(raw),
            metadata=metadata,
        )

    def candidate_from_slack_file_metadata(self, file: dict[str, Any]) -> AttachmentCandidate | None:
        """Normalize Slack file metadata without fetching or storing file bytes."""

        if not isinstance(file, dict):
            return None
        source_url = file.get("url_private_download") or file.get("url_private")
        name = safe_attachment_name(
            str(file.get("name") or file.get("title") or file.get("id") or "slack-file"),
            fallback="slack-file",
        )
        mime_type = self.normalize_mime_type(
            str(file.get("mimetype") or file.get("mime_type") or ""),
            name=name,
        )
        size = file.get("size")
        return AttachmentCandidate(
            source="slack_file",
            external_id=str(file.get("id") or "") or None,
            name=name,
            mime_type=mime_type,
            kind=self.classify_mime_type(mime_type, name=name),
            source_url=str(source_url) if source_url else None,
            size_bytes=size if isinstance(size, int) else None,
            metadata={k: v for k, v in file.items() if k not in {"url_private_download", "url_private"}},
        )

    def candidate_from_google_doc(
        self,
        file: dict[str, Any],
        *,
        text_content: str | None = None,
    ) -> AttachmentCandidate | None:
        """Normalize Google Drive/Docs metadata without choosing an ETL storage table."""

        if not isinstance(file, dict):
            return None
        file_id = str(file.get("id") or "").strip()
        if not file_id:
            return None
        name = safe_attachment_name(str(file.get("name") or file_id), fallback=file_id)
        mime_type = self.normalize_mime_type(
            str(file.get("mimeType") or "application/vnd.google-apps.document"),
            name=name,
        )
        return AttachmentCandidate(
            source="google_doc",
            external_id=file_id,
            name=name,
            mime_type=mime_type,
            kind="document",
            text_content=text_content,
            source_url=str(file.get("webViewLink") or "") or None,
            metadata=dict(file),
        )

    def event_ref_for_stored(self, stored: StoredAttachment, *, source_path: str | None = None) -> dict[str, Any]:
        part: dict[str, Any] = {
            "type": "attachment_ref",
            "attachment_id": stored.id,
            "media_type": stored.mime_type,
            "name": stored.name,
        }
        if source_path:
            part["source_path"] = source_path
        return part

    def chat_ref_from_event_part(self, part: dict[str, Any]) -> dict[str, Any] | None:
        if part.get("type") != "attachment_ref":
            return None
        attachment_id = str(part.get("attachment_id") or part.get("id") or "")
        media_type = str(
            part.get("media_type") or part.get("mime_type") or "application/octet-stream"
        )
        source_path = part.get("source_path") if isinstance(part.get("source_path"), str) else None
        name = safe_attachment_name(
            part.get("name") if isinstance(part.get("name"), str) else None,
            fallback=attachment_name_from_source_path(source_path, attachment_id),
        )
        return {
            "type": "attachment_ref",
            "id": attachment_id,
            "name": name,
            "mime_type": media_type,
        }

    def event_parts_to_chat_parts(self, parts: list[dict[str, Any]]) -> list[dict[str, Any]]:
        chat_parts: list[dict[str, Any]] = []
        for part in parts:
            part_type = part.get("type")
            if part_type == "text":
                chat_parts.append({"type": "text", "text": str(part.get("text") or "")})
            elif part_type == "attachment_ref":
                chat_parts.append(self.chat_ref_from_event_part(part) or part)
            else:
                chat_parts.append(part)
        return chat_parts

    async def extract_inline_parts(
        self,
        conn: Any,
        *,
        thread_key: str,
        chat_message_id: str,
        parts: list[dict[str, Any]],
    ) -> tuple[list[dict[str, Any]], list[str]]:
        """Store inline base64 bytes and replace content blocks with attachment refs."""

        transformed: list[dict[str, Any]] = []
        attachment_ids: list[str] = []
        for part in parts:
            candidate = self.candidate_from_inline_part(part)
            if candidate is None:
                transformed.append(part)
                continue
            if candidate.data is None:
                transformed.append(part)
                continue
            stored = await insert_thread_attachment(
                conn,
                thread_key=thread_key,
                message_id=chat_message_id,
                name=candidate.name,
                mime_type=candidate.mime_type,
                data=candidate.data,
                attachment_id=candidate.external_id,
                source=candidate.source,
                source_url=candidate.source_url,
                metadata=candidate.metadata,
            )
            transformed.append(
                self.event_ref_for_stored(stored, source_path=candidate.source_path)
            )
            attachment_ids.append(stored.id)
        return transformed, attachment_ids
