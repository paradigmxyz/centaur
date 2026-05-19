"""Centaur SDK — lightweight toolkit for building Centaur-compatible tools.

Public API:
    secret(key)       — resolve a secret via the pluggable backend
    Table             — Rich table (re-export for CLI tools)
    render_text_table — plain-text table renderer
"""

from __future__ import annotations

from centaur_sdk.cli_tables import Table, render_text_table
from centaur_sdk.tool_sdk import (
    ToolContext,
    current_thread_key,
    get_tool_context,
    reset_tool_context,
    save_attachment,
    save_attachment_from_path,
    secret,
    set_tool_context,
)

__all__ = [
    "Table",
    "ToolContext",
    "current_thread_key",
    "get_tool_context",
    "render_text_table",
    "reset_tool_context",
    "save_attachment",
    "save_attachment_from_path",
    "secret",
    "set_tool_context",
]
