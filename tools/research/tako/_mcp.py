"""Keyless backend: Tako's hosted MCP (https://mcp.tako.com/mcp).

When no TAKO_API_KEY is configured, `search`, `answer`, and `available_data`
fall back to Tako's free, rate-limited MCP tier. `contents` (row-level export
billing) stays key-only. This mirrors the websearch tool, which falls back to
Parallel's free hosted MCP without a PARALLEL_API_KEY.

STATUS: scaffold ahead of the server-side free tier. Until Tako ships
anonymous access on the MCP, keyless calls return 401 and `_raise_for_auth`
converts that into a clear "set TAKO_API_KEY" message.

The wire protocol is MCP Streamable HTTP (JSON-RPC over POST). The Tako
Worker is stateless: it issues no `Mcp-Session-Id` and keeps no cross-request
state, so each command performs one self-contained initialize -> initialized
-> tools/call exchange. A stable client-minted session id is still sent so
the server can attribute free-tier rate limits per client rather than per
egress IP (a Centaur deployment NATs many sandboxes through few IPs).

httpx honors HTTPS_PROXY and SSL_CERT_FILE from the environment, so unlike
the SDK's urllib3 transport this path needs no explicit iron-proxy wiring.
"""

from __future__ import annotations

import json
import uuid
from typing import Any

import httpx

MCP_URL = "https://mcp.tako.com/mcp"
MCP_PROTOCOL_VERSION = "2025-03-26"
MCP_CLIENT_NAME = "centaur-tako-tool"
MCP_CLIENT_VERSION = "0.1.0"

# MCP-app widget plumbing on tako_search/tako_answer outputs that has no
# equivalent in the SDK response shape. Stripped so both backends return the
# same top-level keys (cards, web_results, usage, request_id).
_WIDGET_FIELDS = ("pub_id", "embed_url", "image_url", "dark_mode", "width", "height")


class McpAuthRequired(RuntimeError):
    """The MCP rejected the anonymous call (free tier absent or exhausted)."""


def _strip_widget_fields(payload: dict[str, Any]) -> dict[str, Any]:
    return {k: v for k, v in payload.items() if k not in _WIDGET_FIELDS}


def _sources_and_count(
    data_count: int | None,
    web_count: int | None,
) -> tuple[list[str] | None, int | None, list[dict[str, str]]]:
    """Map the CLI's per-source counts onto the MCP's sources[] + single count.

    The MCP takes one `count` applied to each selected source; the SDK takes a
    count per source. `data_count=0` / `web_count=0` select sources exactly as
    the SDK path does. When both counts are set and differ, the data count
    wins and the divergence is recorded as a partial failure rather than
    silently averaged.
    """
    partial_failures: list[dict[str, str]] = []
    sources: list[str] | None = None
    if data_count == 0:
        sources = ["web"]
    elif web_count == 0:
        sources = ["data"]

    positive = [c for c in (data_count, web_count) if c]
    count = positive[0] if positive else None
    if len(positive) == 2 and data_count != web_count:
        count = data_count
        partial_failures.append(
            {
                "feature": "per_source_counts",
                "error": (
                    "the free MCP tier takes a single count for all sources; "
                    f"using data_count={data_count} and ignoring web_count={web_count}"
                ),
            }
        )
    return sources, count, partial_failures


class TakoMcpBackend:
    """Anonymous client for Tako's hosted MCP free tier."""

    def __init__(
        self,
        mcp_url: str = MCP_URL,
        timeout_seconds: float = 120.0,
        transport: httpx.BaseTransport | None = None,
    ) -> None:
        self._mcp_url = mcp_url
        self._timeout = timeout_seconds
        # Test seam: httpx.MockTransport in unit tests, None (real) otherwise.
        self._transport = transport
        # Stable per-client-instance id for free-tier rate-limit attribution.
        self._session_id = f"centaur-tako-{uuid.uuid4().hex}"

    # -- public operations --------------------------------------------------

    def search(
        self,
        query: str,
        *,
        effort: str | None = None,
        data_count: int | None = None,
        web_count: int | None = None,
        node_ids: list[str] | None = None,
        strict: bool = False,
        country_code: str | None = None,
        locale: str | None = None,
    ) -> dict:
        sources, count, partial_failures = _sources_and_count(data_count, web_count)
        arguments = _drop_none(
            {
                "query": query,
                "effort": effort,
                "sources": sources,
                "count": count,
                "node_ids": node_ids or None,
                "strict": strict or None,
                "country_code": country_code,
                "locale": locale,
            }
        )
        payload = _strip_widget_fields(self._call_tool("tako_search", arguments))
        payload["meta"] = {"backend": "tako:mcp", "partial_failures": partial_failures}
        return payload

    def answer(
        self,
        query: str,
        *,
        effort: str | None = None,
        data_count: int | None = None,
        web_count: int | None = None,
        node_ids: list[str] | None = None,
        strict: bool = False,
        country_code: str | None = None,
        locale: str | None = None,
    ) -> dict:
        sources, count, partial_failures = _sources_and_count(data_count, web_count)
        # tako_answer takes neither effort nor count; flag both rather than
        # silently dropping a knob the caller set.
        for name, value in (("effort", effort), ("count", count)):
            if value is not None:
                partial_failures.append(
                    {
                        "feature": name,
                        "error": f"{name} is not supported by the free MCP answer tool; ignored",
                    }
                )
        arguments = _drop_none(
            {
                "query": query,
                "sources": sources,
                "node_ids": node_ids or None,
                "strict": strict or None,
                "country_code": country_code,
                "locale": locale,
            }
        )
        payload = _strip_widget_fields(self._call_tool("tako_answer", arguments))
        payload["meta"] = {"backend": "tako:mcp", "partial_failures": partial_failures}
        return payload

    def available_data(
        self,
        q: str,
        *,
        types: str | None = None,
        label: str | None = None,
    ) -> dict:
        # The hosted tool's output shape is the source this tool's local
        # pipeline was ported from: {found, query, summary, matches,
        # other_matches} passes through unchanged.
        arguments = _drop_none({"q": q, "types": types, "label": label})
        payload = self._call_tool("tako_available_data", arguments)
        payload["meta"] = {"backend": "tako:mcp", "partial_failures": []}
        return payload

    # -- MCP plumbing ---------------------------------------------------------

    def _call_tool(self, tool_name: str, arguments: dict[str, Any]) -> dict[str, Any]:
        with httpx.Client(timeout=self._timeout, transport=self._transport) as client:
            self._initialize(client)
            envelope = {
                "jsonrpc": "2.0",
                "id": str(uuid.uuid4()),
                "method": "tools/call",
                "params": {"name": tool_name, "arguments": arguments},
            }
            response = client.post(self._mcp_url, headers=self._headers(), json=envelope)
            _raise_for_auth(response)
            response.raise_for_status()
            body = _decode_envelope(response)
        if "error" in body:
            raise RuntimeError(f"Tako MCP error: {str(body['error'])[:500]}")
        result = body.get("result") or {}
        if result.get("isError"):
            raise RuntimeError(f"Tako MCP tool error: {str(result)[:500]}")
        structured = result.get("structuredContent")
        if isinstance(structured, dict):
            return structured
        for block in result.get("content", []) or []:
            if isinstance(block, dict) and block.get("type") == "text":
                text = str(block.get("text") or "")
                try:
                    return json.loads(text)
                except json.JSONDecodeError:
                    continue
        raise RuntimeError(f"Tako MCP returned no parseable content: {str(result)[:500]}")

    def _initialize(self, client: httpx.Client) -> None:
        """One protocol-compliant handshake per command.

        The Tako Worker is stateless (no Mcp-Session-Id bookkeeping), so this
        never carries state across commands; it exists to stay within the MCP
        lifecycle contract and to tolerate a future stateful deployment.
        """
        init = {
            "jsonrpc": "2.0",
            "id": str(uuid.uuid4()),
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": MCP_CLIENT_NAME, "version": MCP_CLIENT_VERSION},
            },
        }
        response = client.post(self._mcp_url, headers=self._headers(), json=init)
        _raise_for_auth(response)
        response.raise_for_status()
        notify = {"jsonrpc": "2.0", "method": "notifications/initialized"}
        ack = client.post(self._mcp_url, headers=self._headers(), json=notify)
        if ack.status_code >= 400:
            raise RuntimeError(
                f"Tako MCP initialize ack failed ({ack.status_code}): {ack.text[:200]}"
            )

    def _headers(self) -> dict[str, str]:
        # No Authorization header: this backend exists precisely for the
        # anonymous tier. Mcp-Session-Id carries the rate-limit attribution id.
        return {
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
            "Mcp-Session-Id": self._session_id,
        }


def _drop_none(mapping: dict[str, Any]) -> dict[str, Any]:
    return {k: v for k, v in mapping.items() if v is not None}


def _raise_for_auth(response: httpx.Response) -> None:
    if response.status_code in (401, 403):
        raise McpAuthRequired(
            "Tako's MCP rejected the anonymous request. The free tier may not "
            "be available yet, or its rate limit is exhausted. Configure "
            "TAKO_API_KEY for full access."
        )


def _decode_envelope(response: httpx.Response) -> dict[str, Any]:
    """Decode a Streamable HTTP response: plain JSON or a one-event SSE body."""
    content_type = response.headers.get("content-type", "")
    if "text/event-stream" in content_type:
        for line in response.text.splitlines():
            if line.startswith("data:"):
                return json.loads(line[len("data:") :].strip())
        raise RuntimeError("Tako MCP returned an SSE body with no data event")
    return response.json()
