"""Client for Mercator's hosted MCP endpoint.

Mercator finds external API services, prices a plan of calls, and pays for them
from a Tempo wallet access key that Mercator holds. Spending is capped by the
limits the wallet owner approved when authorizing Mercator. The sandbox never
sees a Mercator token: the proxy injects the OAuth bearer for mercator.sh.

API reference: https://mercator.sh/docs
"""

from __future__ import annotations

import json
from decimal import Decimal
from typing import Any

import httpx

MCP_URL = "https://mercator.sh/mcp/auth"
DEFAULT_SEARCH_LIMIT = 8


class MercatorClient:
    """Client for Mercator's MCP tools."""

    def __init__(self, timeout: float = 60.0) -> None:
        self.timeout = timeout
        self._client: httpx.Client | None = None

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(timeout=self.timeout)
        return self._client

    def _call(self, tool: str, arguments: dict[str, Any]) -> dict[str, Any]:
        response = self.client.post(
            MCP_URL,
            headers={"Accept": "application/json, text/event-stream"},
            json={
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": tool, "arguments": arguments},
            },
        )
        response.raise_for_status()
        message = _message(response)
        if "error" in message:
            raise RuntimeError(f"Mercator {tool} failed: {message['error'].get('message')}")
        result = message.get("result") or {}
        if result.get("isError"):
            text = " ".join(item.get("text", "") for item in result.get("content", []))
            raise RuntimeError(f"Mercator {tool} failed: {text}")
        if not isinstance(result.get("structuredContent"), dict):
            raise RuntimeError("Mercator returned an unexpected response.")
        return result["structuredContent"]

    def connection_status(self) -> dict[str, Any]:
        """Return authorization, wallet, and spending-limit readiness."""
        return self._call("get_connection_status", {})

    def search(self, query: str, *, limit: int = DEFAULT_SEARCH_LIMIT) -> dict[str, Any]:
        """Rank cataloged service endpoints for an intended outcome."""
        return self._call("search_services", {"query": query, "limit": limit})

    def describe(
        self,
        service_id: str,
        *,
        method: str | None = None,
        path: str | None = None,
    ) -> dict[str, Any]:
        """Return a service's endpoints, or one endpoint's full request schema."""
        arguments = {"service_id": service_id, "method": method, "path": path}
        return self._call(
            "describe_service",
            {key: value for key, value in arguments.items() if value is not None},
        )

    def quote(self, plan: dict[str, Any]) -> dict[str, Any]:
        """Validate a plan and live-price every node without paying."""
        return self._call("quote_plan", {"plan": plan})

    def submit(
        self,
        plan: dict[str, Any],
        *,
        max_spend: float,
        idempotency_key: str,
    ) -> dict[str, Any]:
        """Quote a plan and pay for it only if the total is within max_spend."""
        total = self.quote(plan)["totalAmount"]
        if Decimal(total) > Decimal(str(max_spend)):
            raise ValueError(
                f"Quoted total {total} exceeds max spend {max_spend:g}; nothing was paid."
            )
        return self._call(
            "create_job",
            {
                "plan": plan,
                "approved_total": total,
                "idempotency_key": idempotency_key,
            },
        )

    def get_job(self, job_id: str) -> dict[str, Any]:
        """Return a job's status and, once finished, its results."""
        return self._call("get_job", {"job_id": job_id})

    def close(self) -> None:
        if self._client is not None:
            self._client.close()
            self._client = None

    def __enter__(self) -> MercatorClient:
        return self

    def __exit__(self, *args: Any) -> None:
        self.close()


def _message(response: httpx.Response) -> dict[str, Any]:
    """Return the JSON-RPC response from a JSON body or an MCP event stream."""
    if not response.headers.get("content-type", "").startswith("text/event-stream"):
        return response.json()
    for event in reversed(response.text.replace("\r\n", "\n").split("\n\n")):
        data = "\n".join(
            line[5:].removeprefix(" ") for line in event.split("\n") if line.startswith("data:")
        )
        if data:
            message = json.loads(data)
            if "result" in message or "error" in message:
                return message
    raise RuntimeError("Mercator returned an event stream without a response.")


def _client() -> MercatorClient:
    return MercatorClient()
