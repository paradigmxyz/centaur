from __future__ import annotations

import asyncio
import os
from typing import Any

import httpx
import structlog

from api.slack_sanitize import sanitize_for_slack

log = structlog.get_logger()

# Other 4xx is permanent: the slackbot is telling us the call is malformed.
_RETRYABLE_STATUS = frozenset({408, 429, 500, 502, 503, 504})
_RETRY_ATTEMPTS = 3
_RETRY_BASE_DELAY_S = 0.25


def _base_url() -> str:
    return os.getenv("SLACKBOT_URL", "").strip().rstrip("/")


def _api_key() -> str:
    return os.getenv("SLACKBOT_API_KEY", "").strip()


def enabled() -> bool:
    return bool(_base_url() and _api_key())


async def post(
    path: str,
    body: dict[str, Any],
    *,
    timeout: httpx.Timeout | None = None,
) -> dict[str, Any] | None:
    base_url = _base_url()
    api_key = _api_key()
    if not base_url or not api_key:
        return None
    request_timeout = timeout or httpx.Timeout(8.0, connect=2.0)
    last_status: int | None = None
    last_response: str | None = None
    last_error: str | None = None
    for attempt in range(_RETRY_ATTEMPTS):
        try:
            async with httpx.AsyncClient(timeout=request_timeout) as client:
                response = await client.post(
                    f"{base_url}{path}",
                    headers={
                        "Authorization": f"Bearer {api_key}",
                        "Content-Type": "application/json",
                    },
                    json=body,
                )
                text = response.text
                if response.is_success:
                    if not text:
                        return {}
                    data = response.json()
                    return data if isinstance(data, dict) else {}
                last_status = response.status_code
                last_response = text[:500]
                if response.status_code not in _RETRYABLE_STATUS:
                    log.warning(
                        "slackbot_call_failed",
                        path=path,
                        status=response.status_code,
                        response=last_response,
                    )
                    return None
        except Exception as exc:
            last_error = str(exc)
        if attempt + 1 < _RETRY_ATTEMPTS:
            await asyncio.sleep(_RETRY_BASE_DELAY_S * (2**attempt))
    log.warning(
        "slackbot_call_failed",
        path=path,
        status=last_status,
        response=last_response,
        error=last_error,
        attempts=_RETRY_ATTEMPTS,
    )
    return None


def is_slack_delivery(delivery: dict[str, Any] | None) -> bool:
    return isinstance(delivery, dict) and str(delivery.get("platform") or "") == "slack"


def channel_id(delivery: dict[str, Any]) -> str:
    return str(delivery.get("channel") or delivery.get("channel_id") or "").strip()


def thread_ts(delivery: dict[str, Any]) -> str:
    return str(delivery.get("thread_ts") or "").strip()


def recipient_team_id(delivery: dict[str, Any], thread_key: str) -> str:
    value = str(
        delivery.get("recipient_team_id")
        or delivery.get("team_id")
        or delivery.get("team")
        or ""
    ).strip()
    if value:
        return value
    parts = thread_key.split(":")
    return parts[1] if len(parts) >= 2 and parts[0] == "slack" else ""


def recipient_user_id(delivery: dict[str, Any], metadata: dict[str, Any]) -> str:
    return str(
        delivery.get("recipient_user_id")
        or delivery.get("user_id")
        or metadata.get("user_id")
        or ""
    ).strip()


async def open_agent_session(
    *,
    delivery: dict[str, Any],
    metadata: dict[str, Any],
    thread_key: str,
    title: str = "Centaur execution",
    header: str | None = None,
) -> str | None:
    if not enabled() or not is_slack_delivery(delivery):
        return None
    channel = channel_id(delivery)
    parent_ts = thread_ts(delivery)
    if not channel or not parent_ts:
        return None
    body: dict[str, Any] = {
        "channel": channel,
        "parent_ts": parent_ts,
        "recipient_team_id": recipient_team_id(delivery, thread_key),
        "recipient_user_id": recipient_user_id(delivery, metadata),
        "title": title,
    }
    header_text = (header or "").strip()
    if header_text:
        body["header"] = header_text
    result = await post("/api/slack/agent-sessions", body)
    session_id = str((result or {}).get("session_id") or "").strip()
    return session_id or None


async def session_text(session_id: str | None, markdown: str) -> None:
    sanitized = sanitize_for_slack(markdown)
    if not session_id or not sanitized.strip():
        return
    await post(f"/api/slack/agent-sessions/{session_id}/text", {"markdown": sanitized})


async def session_step(
    session_id: str | None,
    *,
    step_id: str,
    title: str,
    status: str = "in_progress",
    details: str | None = None,
    output: str | None = None,
) -> None:
    if not session_id or not step_id or not title:
        return
    body: dict[str, Any] = {
        "id": step_id,
        "title": sanitize_for_slack(title),
        "status": status,
    }
    if details:
        body["details"] = sanitize_for_slack(details)
    if output:
        body["output"] = sanitize_for_slack(output)
    await post(f"/api/slack/agent-sessions/{session_id}/step", body)


async def session_done(session_id: str | None, thread_id: str | None = None) -> None:
    if not session_id:
        return
    body: dict[str, Any] = {}
    if thread_id:
        body["thread_id"] = thread_id
    await post(f"/api/slack/agent-sessions/{session_id}/done", body)


async def harness_event(
    session_id: str | None, event: dict[str, Any]
) -> dict[str, Any] | None:
    if not session_id:
        return None
    return await post(
        f"/api/slack/agent-sessions/{session_id}/harness-event",
        {"event": sanitize_slack_event(event)},
        timeout=httpx.Timeout(60.0, connect=2.0),
    )


async def set_status(delivery: dict[str, Any], status: str) -> None:
    if not enabled() or not is_slack_delivery(delivery):
        return
    channel = channel_id(delivery)
    ts = thread_ts(delivery)
    if not channel or not ts:
        return
    await post(
        "/api/slack/assistant/status",
        {"channel_id": channel, "thread_ts": ts, "status": status},
    )


_TEXT_KEYS = {
    "content",
    "delta",
    "details",
    "error",
    "message",
    "output",
    "result",
    "summary",
    "text",
    "title",
}


def sanitize_slack_event(value: Any) -> Any:
    if isinstance(value, str):
        return sanitize_for_slack(value, preserve_edges=True)
    if isinstance(value, list):
        return [sanitize_slack_event(item) for item in value]
    if isinstance(value, dict):
        sanitized: dict[str, Any] = {}
        for key, item in value.items():
            if isinstance(item, (dict, list)) or key in _TEXT_KEYS:
                sanitized[key] = sanitize_slack_event(item)
            else:
                sanitized[key] = item
        return sanitized
    return value
