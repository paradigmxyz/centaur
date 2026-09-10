"""PayBox Streamable HTTP MCP client."""

from __future__ import annotations

import json
import uuid
from typing import Any

import httpx

from centaur_sdk import secret

MCP_URL = "https://api.paybox.sh/mcp"
MCP_PROTOCOL_VERSION = "2025-06-18"

# These calls do not request money movement, signatures, or credential output.
READ_ONLY_TOOLS = frozenset(
    {
        "discover_services",
        "get_buy_link",
        "get_portfolio",
        "get_request",
        "list_credentials",
        "list_requests",
        "request_account_change",
        "verify_solana_balance",
    }
)


class PayboxClient:
    """Client for PayBox's user-scoped MCP server.

    In a Centaur sandbox, iron-proxy injects the connected user's OAuth bearer
    token. For local development, set PAYBOX_ACCESS_TOKEN or PAYBOX_API_KEY.
    """

    def __init__(
        self,
        token: str | None = None,
        http_client: httpx.Client | None = None,
        timeout: float = 60.0,
    ):
        headers = {
            "Accept": "application/json, text/event-stream",
            "Content-Type": "application/json",
            "MCP-Protocol-Version": MCP_PROTOCOL_VERSION,
        }
        token = token or secret("PAYBOX_ACCESS_TOKEN", "") or secret("PAYBOX_API_KEY", "")
        if token:
            headers["Authorization"] = f"Bearer {token}"
        self._client = http_client or httpx.Client(headers=headers, timeout=timeout)
        if http_client is not None:
            self._client.headers.update(headers)
        self._session_id: str | None = None
        self._initialized = False

    def close(self) -> None:
        self._client.close()

    def __enter__(self) -> PayboxClient:
        return self

    def __exit__(self, *args: Any) -> None:
        self.close()

    def _decode_response(self, response: httpx.Response) -> dict[str, Any]:
        response.raise_for_status()
        if not response.content:
            return {}

        body = response.text
        content_type = response.headers.get("content-type", "").lower()
        if "text/event-stream" not in content_type and not body.lstrip().startswith("data:"):
            return response.json()

        payloads: list[str] = []
        lines: list[str] = []
        for raw_line in body.splitlines():
            if raw_line.startswith("data:"):
                lines.append(raw_line[5:].lstrip())
            elif not raw_line and lines:
                payloads.append("\n".join(lines))
                lines = []
        if lines:
            payloads.append("\n".join(lines))
        if not payloads:
            raise RuntimeError("PayBox MCP returned an empty event stream")
        return json.loads(payloads[-1])

    def _send(self, method: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        headers = {"Mcp-Session-Id": self._session_id} if self._session_id else None
        response = self._client.post(
            MCP_URL,
            headers=headers,
            json={
                "jsonrpc": "2.0",
                "id": str(uuid.uuid4()),
                "method": method,
                "params": params or {},
            },
        )
        session_id = response.headers.get("Mcp-Session-Id")
        if session_id:
            self._session_id = session_id
        message = self._decode_response(response)
        if "error" in message:
            raise RuntimeError(f"PayBox MCP {method} failed: {message['error']}")
        return message.get("result", {})

    def _notify_initialized(self) -> None:
        headers = {"Mcp-Session-Id": self._session_id} if self._session_id else None
        response = self._client.post(
            MCP_URL,
            headers=headers,
            json={"jsonrpc": "2.0", "method": "notifications/initialized"},
        )
        response.raise_for_status()

    def _ensure_initialized(self) -> None:
        if self._initialized:
            return
        self._send(
            "initialize",
            {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "centaur-paybox", "version": "0.1.0"},
            },
        )
        self._notify_initialized()
        self._initialized = True

    def _call_tool(self, tool_name: str, arguments: dict[str, Any] | None = None) -> Any:
        self._ensure_initialized()
        result = self._send("tools/call", {"name": tool_name, "arguments": arguments or {}})
        if result.get("isError"):
            text = "; ".join(
                item.get("text", "")
                for item in result.get("content", [])
                if isinstance(item, dict) and item.get("type") == "text"
            )
            raise RuntimeError(f"PayBox MCP tool {tool_name} failed: {text or result}")

        if result.get("structuredContent") is not None:
            return result["structuredContent"]
        content = result.get("content", [])
        texts = [
            item.get("text", "")
            for item in content
            if isinstance(item, dict) and item.get("type") == "text"
        ]
        if len(texts) == 1:
            try:
                return json.loads(texts[0])
            except json.JSONDecodeError:
                return {"text": texts[0]}
        return {"content": content}

    def health(self) -> dict[str, Any]:
        """Check authenticated MCP connectivity and return the tool count."""
        tools = self.list_tools()
        return {"ok": True, "tool": "paybox", "tool_count": len(tools)}

    def list_tools(self) -> list[dict[str, Any]]:
        """List the PayBox MCP tools enabled for the connected user."""
        self._ensure_initialized()
        result = self._send("tools/list")
        return result.get("tools", [])

    def execute(
        self,
        tool_name: str,
        arguments: dict[str, Any] | None = None,
        confirm: bool = False,
    ) -> Any:
        """Call any PayBox MCP tool.

        Set confirm=true only after the user explicitly approves all material
        payment, signing, secret, recipient, and amount fields. Known read-only
        tools do not require confirmation.
        """
        if tool_name not in READ_ONLY_TOOLS and not confirm:
            raise RuntimeError(
                f"PayBox tool {tool_name!r} may create or advance a sensitive operation; "
                "retry with confirm=true only after explicit user approval"
            )
        return self._call_tool(tool_name, arguments)

    def list_credentials(self) -> Any:
        """List credentials granted to this PayBox connector."""
        return self._call_tool("list_credentials")

    def get_portfolio(self, address: str, network_ids: str | None = None) -> Any:
        """Read token balances for an EVM or Solana wallet address."""
        arguments = {"address": address}
        if network_ids:
            arguments["network_ids"] = network_ids
        return self._call_tool("get_portfolio", arguments)

    def get_request(self, request_id: str) -> Any:
        """Read the current state of a PayBox request; never re-submit writes."""
        return self._call_tool("get_request", {"request_id": request_id})

    def list_requests(self) -> Any:
        """List recent PayBox requests for the connected client."""
        return self._call_tool("list_requests")

    def discover_services(self, query: str | None = None) -> Any:
        """Discover curated x402 services without paying for them."""
        arguments = {"query": query} if query else {}
        return self._call_tool("discover_services", arguments)


def _client() -> PayboxClient:
    return PayboxClient()
