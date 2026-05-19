from __future__ import annotations

import os
from typing import Any

import httpx
import structlog

log = structlog.get_logger()


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
            if not response.is_success:
                log.warning(
                    "slackbot_call_failed",
                    path=path,
                    status=response.status_code,
                    response=text[:500],
                )
                return None
            if not text:
                return {}
            data = response.json()
            return data if isinstance(data, dict) else {}
    except Exception as exc:
        log.warning("slackbot_call_error", path=path, error=str(exc))
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
    if not session_id or not markdown.strip():
        return
    await post(f"/api/slack/agent-sessions/{session_id}/text", {"markdown": markdown})


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
    body: dict[str, Any] = {"id": step_id, "title": title, "status": status}
    if details:
        body["details"] = details
    if output:
        body["output"] = output
    await post(f"/api/slack/agent-sessions/{session_id}/step", body)


async def session_done(session_id: str | None, thread_id: str | None = None) -> None:
    if not session_id:
        return
    body: dict[str, Any] = {}
    if thread_id:
        body["thread_id"] = thread_id
    await post(f"/api/slack/agent-sessions/{session_id}/done", body)


async def harness_event(session_id: str | None, event: dict[str, Any]) -> dict[str, Any] | None:
    if not session_id:
        return None
    return await post(
        f"/api/slack/agent-sessions/{session_id}/harness-event",
        {"event": event},
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
