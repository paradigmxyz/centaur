"""Centaur SDK — lightweight toolkit for building Centaur-compatible tools.

Public API:
    secret(key)       — resolve a secret via the pluggable backend
"""

from __future__ import annotations

from centaur_sdk.tool_sdk import (
    ToolContext,
    current_chat_destination,
    current_discord_thread,
    current_github_thread,
    current_linear_thread,
    current_session_context,
    current_slack_thread,
    current_thread_key,
    get_tool_context,
    reset_tool_context,
    save_attachment,
    save_attachment_from_path,
    secret,
    set_tool_context,
)

__all__ = [
    "ToolContext",
    "current_chat_destination",
    "current_discord_thread",
    "current_github_thread",
    "current_linear_thread",
    "current_session_context",
    "current_slack_thread",
    "current_thread_key",
    "get_tool_context",
    "reset_tool_context",
    "save_attachment",
    "save_attachment_from_path",
    "secret",
    "set_tool_context",
]
