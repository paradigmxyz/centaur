"""HTTP client for Centaur create / append / execute session routes.

Uses only the standard library so Spaces does not depend on Centaur Python
packages. Request shapes match the durable session API used by chat ingress
services; adjust this module alone if Centaur is replaced.
"""

from __future__ import annotations

import json
import urllib.error
import urllib.request
from typing import Any
from urllib.parse import quote


class SessionClientError(RuntimeError):
    """Raised when the Centaur session API returns a non-success response."""

    def __init__(self, action: str, status: int, body: str) -> None:
        self.action = action
        self.status = status
        self.body = body
        super().__init__(f"{action} failed: HTTP {status}")


class SessionClient:
    """Minimal session control-plane client."""

    def __init__(
        self,
        base_url: str,
        token: str,
        *,
        timeout_s: float = 30.0,
        opener: Any | None = None,
    ) -> None:
        self._base_url = base_url.rstrip("/")
        self._token = token
        self._timeout_s = timeout_s
        self._opener = opener or urllib.request.urlopen

    def create_session(
        self,
        session_id: str,
        *,
        harness_type: str,
        metadata: dict[str, Any] | None = None,
        on_harness_conflict: str | None = None,
    ) -> dict[str, Any]:
        body: dict[str, Any] = {
            "harness_type": harness_type,
            "metadata": metadata or {},
        }
        if on_harness_conflict is not None:
            body["on_harness_conflict"] = on_harness_conflict
        return self._request(
            "POST",
            f"/v1/sessions/{quote(session_id, safe='')}",
            body,
            action="create session",
        )

    def append_messages(
        self,
        session_id: str,
        messages: list[dict[str, Any]],
    ) -> dict[str, Any]:
        return self._request(
            "POST",
            f"/v1/sessions/{quote(session_id, safe='')}/messages",
            {"messages": messages},
            action="append messages",
        )

    def execute(
        self,
        session_id: str,
        *,
        input_lines: list[str] | None = None,
        client_execution_id: str | None = None,
    ) -> dict[str, Any]:
        body: dict[str, Any] = {}
        if input_lines is not None:
            body["input_lines"] = input_lines
        if client_execution_id is not None:
            body["client_execution_id"] = client_execution_id
        return self._request(
            "POST",
            f"/v1/sessions/{quote(session_id, safe='')}/execute",
            body,
            action="execute session",
        )

    def _request(
        self,
        method: str,
        path: str,
        body: dict[str, Any],
        *,
        action: str,
    ) -> dict[str, Any]:
        data = json.dumps(body).encode("utf-8")
        request = urllib.request.Request(
            f"{self._base_url}{path}",
            data=data,
            method=method,
            headers={
                "Authorization": f"Bearer {self._token}",
                "Content-Type": "application/json",
                "Accept": "application/json",
            },
        )
        try:
            with self._opener(request, timeout=self._timeout_s) as response:
                raw = response.read().decode("utf-8")
                status = getattr(response, "status", 200)
        except urllib.error.HTTPError as exc:
            raw = exc.read().decode("utf-8", errors="replace")
            raise SessionClientError(action, exc.code, raw) from exc

        if status < 200 or status >= 300:
            raise SessionClientError(action, status, raw)
        if not raw.strip():
            return {}
        parsed = json.loads(raw)
        return parsed if isinstance(parsed, dict) else {"value": parsed}
