"""Pydantic models for the message buffer API."""

from __future__ import annotations

from typing import Literal

from pydantic import BaseModel


# ── Content blocks ──────────────────────────────────────────────────────────


class TextBlock(BaseModel):
    type: Literal["text"] = "text"
    text: str


class Base64Source(BaseModel):
    type: Literal["base64"] = "base64"
    media_type: str
    data: str


class ImageBlock(BaseModel):
    type: Literal["image"] = "image"
    source: Base64Source


class DocumentBlock(BaseModel):
    type: Literal["document"] = "document"
    source: Base64Source


ContentBlock = TextBlock | ImageBlock | DocumentBlock


# ── Message types ───────────────────────────────────────────────────────────


class MessageItem(BaseModel):
    role: str = "user"
    parts: list[ContentBlock]
    user_id: str | None = None
    metadata: dict | None = None


class SingleMessageRequest(BaseModel):
    thread_key: str
    role: str = "user"
    parts: list[ContentBlock]
    user_id: str | None = None
    metadata: dict | None = None


class BatchMessageRequest(BaseModel):
    thread_key: str
    messages: list[MessageItem]


# ── Execute request (updated — no message field) ───────────────────────────


class ExecuteRequest(BaseModel):
    thread_key: str
    harness: str | None = None
    engine: str | None = None
    platform: str | None = None
    user_id: str | None = None
    options: dict | None = None


# ── Responses ───────────────────────────────────────────────────────────────


class MessageResponse(BaseModel):
    ok: bool = True
    inserted: int = 0


class MessagesPage(BaseModel):
    messages: list[dict]
    cursor: str | None = None
    has_more: bool = False
