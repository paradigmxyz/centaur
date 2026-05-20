"""Strip known plumbing-leak shapes from assistant text bound for Slack."""

from __future__ import annotations

import json
import re
from collections.abc import Callable
from typing import Any

_THREAD_TRAILER_RE = re.compile(
    r"(?:^|\s)(?:Agent|Codex|Amp|Claude\s+Code|Pi)\s+thread\s+`?[0-9a-f-]{8,}`?(?:,\s*with\s+interactive\s+elements)?(?=\s*$|[.!?]\s*$)",
    re.IGNORECASE | re.MULTILINE,
)
_EXECUTION_TRAILER_RE = re.compile(
    r"\b(?:Execution|execution_id)\s*[:=]\s*`?exe_[0-9a-f]{16}`?",
    re.IGNORECASE,
)
_CURL_EXIT_RE = re.compile(r"curl:?\s*\((\d+)\):?\s*[^\n]{0,200}", re.IGNORECASE)


def _replace_matching_json_objects(
    text: str,
    predicate: Callable[[dict[str, Any]], bool],
    replacement: str,
) -> str:
    decoder = json.JSONDecoder()
    out: list[str] = []
    index = 0
    while index < len(text):
        if text[index] != "{":
            out.append(text[index])
            index += 1
            continue
        try:
            value, end = decoder.raw_decode(text[index:])
        except ValueError:
            out.append(text[index])
            index += 1
            continue
        if isinstance(value, dict) and predicate(value):
            out.append(replacement)
            index += end
            continue
        out.append(text[index])
        index += 1
    return "".join(out)


def _is_k8s_status(value: dict[str, Any]) -> bool:
    return value.get("kind") == "Status" and any(
        key in value for key in ("status", "reason", "code", "message")
    )


def _is_tool_error_envelope(value: dict[str, Any]) -> bool:
    return "error_type" in value and any(
        key in value for key in ("detail", "error", "status_code")
    )


def sanitize_for_slack(text: str | None, *, preserve_edges: bool = False) -> str:
    """Strip known plumbing leaks from `text`. Idempotent; empty input -> ""."""
    if not text:
        return ""
    sanitized = _replace_matching_json_objects(
        text, _is_k8s_status, "[k8s status omitted]"
    )
    sanitized = _replace_matching_json_objects(
        sanitized, _is_tool_error_envelope, "[tool error omitted]"
    )
    sanitized = _THREAD_TRAILER_RE.sub("", sanitized)
    sanitized = _EXECUTION_TRAILER_RE.sub("[execution id omitted]", sanitized)
    sanitized = _CURL_EXIT_RE.sub(r"transport_error(\1)", sanitized)
    sanitized = re.sub(r"[ \t]+\n", "\n", sanitized)
    sanitized = re.sub(r"\n{3,}", "\n\n", sanitized)
    return sanitized if preserve_edges else sanitized.strip()
