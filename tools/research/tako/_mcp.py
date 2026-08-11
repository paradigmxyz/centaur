"""Keyless backend: Tako's hosted MCP (https://mcp.tako.com/mcp).

When no TAKO_API_KEY is configured, `search`, `answer`, and `available_data`
fall back to Tako's anonymous free MCP tier. `contents` (row-level export
billing) stays key-only. This mirrors the websearch tool, which falls back to
Parallel's free hosted MCP without a PARALLEL_API_KEY.

The server side is TakoData/tako-mcp#171. Its contract, which this client is
written against:

- The anonymous surface is exactly the three tools this backend calls
  (`tako_search`, `tako_answer`, `tako_available_data`); everything else is
  hidden. The tier is enabled per environment, fail-closed: where it is off,
  anonymous calls still 401 (`_raise_for_auth` -> "set TAKO_API_KEY").
- `tools/call` is metered at 10/min per client IP (handshake methods are
  never metered). Over-limit -> HTTP 429 with a JSON-RPC error body and
  `Retry-After` (`_raise_for_rate_limit` -> `McpRateLimited`). NB: metering
  is per egress IP, so Centaur sandboxes NATed through one IP share the
  bucket. The free tier is a fallback, not fleet capacity.
- JSON-RPC batch arrays are rejected (400); this client never sends one.
- Only a request with NO Authorization header reaches the free tier; this
  backend sends none. (A malformed/empty header would 401.)

The wire protocol is MCP Streamable HTTP (JSON-RPC over POST). The Tako
Worker is stateless: it issues no `Mcp-Session-Id` and keeps no cross-request
state, so each command performs one self-contained initialize -> initialized
-> tools/call exchange.

httpx honors HTTPS_PROXY and SSL_CERT_FILE from the environment, so unlike
the SDK's urllib3 transport this path needs no explicit iron-proxy wiring.
"""

from __future__ import annotations

import contextlib
import json
import uuid
from typing import Any

import httpx

MCP_URL = "https://mcp.tako.com/mcp"
MCP_PROTOCOL_VERSION = "2025-03-26"
MCP_CLIENT_NAME = "centaur-tako-tool"
MCP_CLIENT_VERSION = "0.1.0"

# Effort levels the hosted search tool accepts. Its input schema is
# fast/instant only (the synchronous tool has no deep mode), so the SDK's
# "deep" is flagged and omitted rather than sent to a certain schema
# rejection server-side.
MCP_SEARCH_EFFORTS = ("fast", "instant")


class McpAuthRequired(RuntimeError):
    """The MCP rejected the anonymous call (free tier not enabled there)."""


class McpRateLimited(RuntimeError):
    """The free tier's per-IP rate limit (10 tools calls/min) was hit."""


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
        if effort is not None and effort not in MCP_SEARCH_EFFORTS:
            partial_failures.append(
                {
                    "feature": "effort",
                    "error": (
                        "the free MCP search tool serves "
                        f"{' and '.join(MCP_SEARCH_EFFORTS)} only; ignoring "
                        f"effort={effort}. Configure TAKO_API_KEY for deep search."
                    ),
                }
            )
            effort = None
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
        structured, markdown = self._call_tool("tako_search", arguments)
        return {
            "query": query,
            # The readable `## Tako Data` document. The hosted tool renders it
            # in the MCP text channel; card rows ride in structuredContent
            # (post tako-mcp#187) with a pointer in the text.
            "answer_markdown": markdown,
            "cards": structured.get("cards", []),
            "web_results": structured.get("web_results", []),
            "request_id": structured.get("request_id"),
            **_top_card_pointer(structured),
            "meta": {"backend": "tako:mcp", "partial_failures": partial_failures},
        }

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
        structured, markdown = self._call_tool("tako_answer", arguments)
        return {
            "query": query,
            # Post tako-mcp#187 the synthesized answer rides in
            # structuredContent; before it, only the text channel carries it.
            "answer": structured.get("answer") or markdown,
            "cards": structured.get("cards", []),
            "web_results": structured.get("web_results", []),
            "request_id": structured.get("request_id"),
            "meta": {"backend": "tako:mcp", "partial_failures": partial_failures},
        }

    def available_data(
        self,
        q: str,
        *,
        types: str | None = None,
        label: str | None = None,
    ) -> dict:
        # The hosted tool returns {found, query, next_call} in structuredContent
        # and the coverage report as the markdown text block. Surface the text
        # as `summary` and keep the SDK-path key set so both backends match.
        # `matches`/`other_matches` aren't individually structured on the MCP,
        # so they come through only when the server provides them.
        arguments = _drop_none({"q": q, "types": types, "label": label})
        structured, markdown = self._call_tool("tako_available_data", arguments)
        return {
            "found": structured.get("found", False),
            "query": structured.get("query", q),
            "summary": markdown,
            "matches": structured.get("matches", []),
            "other_matches": structured.get("other_matches", []),
            "meta": {"backend": "tako:mcp", "partial_failures": []},
        }

    # -- MCP plumbing ---------------------------------------------------------

    def _call_tool(self, tool_name: str, arguments: dict[str, Any]) -> tuple[dict[str, Any], str]:
        """Call an MCP tool and return (structuredContent, text_markdown).

        MCP responses carry two channels, and this tool needs both: the text
        block is the readable `## Tako Data` document, and structuredContent
        holds the machine payload (cards[].content, web_results, answer, plus
        request_id/usage). Callers read the readable field from the text and
        the structured fields from the dict; either can be empty depending on
        the server version (tako-mcp#187 moved bulk data into structuredContent).
        """
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
            _raise_for_rate_limit(response)
            response.raise_for_status()
            body = _decode_envelope(response)
        if "error" in body:
            raise RuntimeError(f"Tako MCP error: {str(body['error'])[:500]}")
        result = body.get("result") or {}
        if result.get("isError"):
            raise RuntimeError(f"Tako MCP tool error: {str(result)[:500]}")
        structured = result.get("structuredContent")
        structured = structured if isinstance(structured, dict) else {}
        texts = [
            str(block.get("text") or "")
            for block in (result.get("content") or [])
            if isinstance(block, dict) and block.get("type") == "text" and block.get("text")
        ]
        text = "\n\n".join(texts)
        if not structured and not text:
            raise RuntimeError(f"Tako MCP returned no content: {str(result)[:500]}")
        return structured, text

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
        # Handshake methods are never metered server-side; kept for symmetry
        # so a contract change can't degrade into a bare HTTPStatusError.
        _raise_for_rate_limit(response)
        response.raise_for_status()
        notify = {"jsonrpc": "2.0", "method": "notifications/initialized"}
        ack = client.post(self._mcp_url, headers=self._headers(), json=notify)
        if ack.status_code >= 400:
            raise RuntimeError(
                f"Tako MCP initialize ack failed ({ack.status_code}): {ack.text[:200]}"
            )

    def _headers(self) -> dict[str, str]:
        # No Authorization header: its complete ABSENCE is what routes the
        # request to the anonymous tier (an empty/malformed header would
        # 401). No Mcp-Session-Id either: the Worker is stateless and never
        # issues one, and free-tier metering is per client IP, not session.
        return {
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
        }


def _drop_none(mapping: dict[str, Any]) -> dict[str, Any]:
    return {k: v for k, v in mapping.items() if v is not None}


#: Top-level pointer to the response's lead card, which the hosted tool returns
#: alongside `cards`. The SDK path passes the whole /v3/search body through, so
#: it keeps these; this backend rebuilds its response dict and would otherwise
#: drop them, leaving the free tier unable to render a chart that the paid tier
#: can. Kept so both backends expose the same pointer, and so a response whose
#: `cards` are empty (the pre-187 shape) still names its chart.
TOP_CARD_POINTER_KEYS = ("pub_id", "image_url", "embed_url", "webpage_url")


def _top_card_pointer(structured: dict[str, Any]) -> dict[str, Any]:
    return {
        key: structured[key] for key in TOP_CARD_POINTER_KEYS if structured.get(key) is not None
    }


def _raise_for_auth(response: httpx.Response) -> None:
    if response.status_code in (401, 403):
        raise McpAuthRequired(
            "Tako's MCP rejected the anonymous request; the free tier is "
            "not enabled on this endpoint. Configure TAKO_API_KEY for full "
            "access."
        )


def _raise_for_rate_limit(response: httpx.Response) -> None:
    """Convert the free tier's 429 into a clear, actionable error.

    The body is a JSON-RPC error envelope whose `error.message` carries the
    server's own limit statement and upsell; surface it verbatim when
    parseable so the number never drifts from the server's actual limit.
    """
    if response.status_code != 429:
        return
    server_message = ""
    with contextlib.suppress(json.JSONDecodeError, AttributeError):
        server_message = str(response.json().get("error", {}).get("message") or "")
    retry_after = response.headers.get("Retry-After", "60")
    raise McpRateLimited(
        (server_message or "Tako free tier rate limit reached.")
        + f" Retry in ~{retry_after}s, or configure TAKO_API_KEY to skip "
        "the shared per-IP limit."
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
