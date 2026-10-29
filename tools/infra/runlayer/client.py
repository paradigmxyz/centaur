"""One MCP connection using the active requester's proxy-injected OAuth grant."""
from __future__ import annotations

import json
import os
from typing import Any
from urllib.parse import urlsplit

import httpx
from jsonschema import Draft202012Validator, SchemaError, ValidationError
from referencing import Registry
from referencing.exceptions import Unresolvable

MAX_BYTES = 8 * 1024 * 1024
PROTOCOL = "2025-03-26"


class RunlayerError(RuntimeError):
    def __init__(self, message: str, *, connect_url: str | None = None, details: Any = None):
        super().__init__(message)
        self.connect_url = connect_url
        self.details = details

    def as_dict(self) -> dict[str, Any]:
        result: dict[str, Any] = {"error": str(self)}
        if self.connect_url:
            result["connect_url"] = self.connect_url
        if self.details is not None:
            result["details"] = self.details
        return result


def _https_url(value: str) -> str:
    try:
        if isinstance(value, str):
            parsed = urlsplit(value)
            if (parsed.scheme == "https" and parsed.hostname and not parsed.username and not parsed.password
                    and not parsed.query and not parsed.fragment and parsed.port in (None, 443) and parsed.path):
                return value
    except ValueError:
        pass
    raise RunlayerError("Set RUNLAYER_MCP_URL to an exact HTTPS Plugin MCP URL.")


class RunlayerClient:
    """MCP sessions and tool definitions live only for this client's execution.

    The sandbox supplies no user identifier or real credential. iron-proxy adds
    the current requester's bearer, after the runtime's config barrier.
    """
    def __init__(self, url: str | None = None, *, http_client: httpx.Client | None = None):
        self.url = _https_url(url) if url else None
        self.connect_url: str | None = None
        self.http = http_client or httpx.Client(timeout=30, follow_redirects=False)
        self._owns_http = http_client is None
        self._id = 0
        self._session: str | None = None
        self._initialized = False
        self._protocol = PROTOCOL

    def _connection(self) -> None:
        if self.url:
            return
        resource_url = _https_url(os.getenv("RUNLAYER_MCP_URL", "").strip())
        console = os.getenv("CENTAUR_CONSOLE_URL", "http://centaur-console:3000").rstrip("/")
        try:
            response = self.http.get(f"{console}/api/v1/sandbox/oauth_apps")
            response.raise_for_status()
            apps = response.json().get("data", [])
        except (httpx.HTTPError, ValueError, AttributeError):
            raise RunlayerError("Could not load Runlayer configuration from Centaur Console.") from None
        if not isinstance(apps, list):
            raise RunlayerError("Centaur Console returned an invalid OAuth-app catalog.")
        matches = [app for app in apps if isinstance(app, dict) and app.get("provider") == "runlayer"]
        if len(matches) != 1:
            raise RunlayerError("Ask an administrator to configure one personal Runlayer integration in Console.")
        self.url = resource_url
        self.connect_url = matches[0].get("start_url")

    def initialize(self) -> None:
        if self._initialized:
            return
        self._connection()
        result = self._request("initialize", {
            "protocolVersion": PROTOCOL, "capabilities": {},
            "clientInfo": {"name": "centaur-runlayer", "version": "0.1.0"},
        })
        if not isinstance(result, dict) or not isinstance(result.get("protocolVersion"), str):
            raise RunlayerError("Runlayer returned an invalid initialize response.")
        if result["protocolVersion"] != PROTOCOL:
            raise RunlayerError("Runlayer selected an unsupported MCP protocol version.")
        self._protocol = result["protocolVersion"]
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"}, request_id=None)
        self._initialized = True

    def list_tools(self) -> list[dict[str, Any]]:
        self.initialize()
        tools: list[dict[str, Any]] = []
        cursor = None
        seen = set()
        while True:
            result = self._request("tools/list", {"cursor": cursor} if cursor else {})
            if not isinstance(result, dict) or not isinstance(result.get("tools"), list):
                raise RunlayerError("Runlayer returned an invalid tools/list response.")
            for tool in result["tools"]:
                if (not isinstance(tool, dict) or not isinstance(tool.get("name"), str)
                        or not tool["name"] or not isinstance(tool.get("inputSchema"), dict)):
                    raise RunlayerError("Runlayer returned an invalid tool definition.")
                tools.append(tool)
            cursor = result.get("nextCursor")
            if not cursor:
                return tools
            if not isinstance(cursor, str) or cursor in seen or len(seen) >= 100:
                raise RunlayerError("Runlayer returned an invalid pagination cursor.")
            seen.add(cursor)

    def search(self, query: str) -> Any:
        tools = self.list_tools()
        if any(tool["name"] == "search_tools" for tool in tools):
            return self._call_tool("search_tools", {"query": query}, tools)
        terms = query.casefold().split()
        return [tool for tool in tools if all(term in f"{tool['name']} {tool.get('description', '')}".casefold()
                                              for term in terms)]

    def call_tool(self, name: str, arguments: dict[str, Any]) -> Any:
        """Call one tool using the schema discovered for the active requester."""
        return self._call_tool(name, arguments, self.list_tools())

    def _call_tool(self, name: str, arguments: dict[str, Any], tools) -> Any:
        if not isinstance(arguments, dict):
            raise RunlayerError("Tool arguments must be a JSON object.")
        tool = next((tool for tool in tools if tool["name"] == name), None)
        if tool is None:
            raise RunlayerError("Unknown tool. Use the exact name returned by Runlayer discovery.")
        try:
            Draft202012Validator.check_schema(tool["inputSchema"])
            Draft202012Validator(tool["inputSchema"], registry=Registry()).validate(arguments)
        except (SchemaError, ValidationError, Unresolvable):
            raise RunlayerError("Arguments do not match Runlayer's advertised input schema.") from None
        return self._request("tools/call", {"name": name, "arguments": arguments})

    def _request(self, method: str, params: dict[str, Any]) -> Any:
        self._id += 1
        reply = self._send({"jsonrpc": "2.0", "id": self._id, "method": method, "params": params}, self._id)
        if not isinstance(reply, dict) or reply.get("jsonrpc") != "2.0" or reply.get("id") != self._id:
            raise RunlayerError("Runlayer returned an invalid MCP response ID.")
        if "error" in reply:
            raise RunlayerError("Runlayer rejected the MCP request.", connect_url=self.connect_url, details=reply["error"])
        if "result" not in reply:
            raise RunlayerError("Runlayer omitted the MCP result.")
        return reply["result"]

    def _send(self, body: dict[str, Any] | None, request_id: int | None, method="POST") -> Any:
        headers = {"Accept": "application/json, text/event-stream", "MCP-Protocol-Version": self._protocol}
        if self._session:
            headers["Mcp-Session-Id"] = self._session
        try:
            with self.http.stream(method, self.url, json=body, headers=headers) as response:
                supplied_url = response.headers.get("X-Runlayer-Connect-URL")
                if supplied_url:
                    try:
                        parsed = urlsplit(supplied_url)
                        if parsed.scheme == "https" and parsed.hostname and not parsed.username and not parsed.password:
                            self.connect_url = supplied_url
                    except (RunlayerError, ValueError):
                        pass
                if response.status_code in (401, 403):
                    message = ("Runlayer authentication is required. Connect or reconnect your account."
                               if response.status_code == 401 else "Runlayer denied access for this requester (HTTP 403).")
                    raise RunlayerError(message, connect_url=self.connect_url)
                if not response.is_success:
                    raise RunlayerError(f"Runlayer returned HTTP {response.status_code}.", connect_url=self.connect_url)
                self._session = response.headers.get("Mcp-Session-Id", self._session)
                if request_id is None:
                    return None
                media = response.headers.get("Content-Type", "").split(";", 1)[0].strip()
                if media == "application/json":
                    raw = bytearray()
                    for chunk in response.iter_bytes():
                        raw.extend(chunk)
                        if len(raw) > MAX_BYTES:
                            raise RunlayerError("Runlayer response exceeded the size limit.")
                    return json.loads(raw)
                if media == "text/event-stream":
                    data: list[str] = []
                    size = 0
                    for line in response.iter_lines():
                        size += len(line.encode())
                        if size > MAX_BYTES:
                            raise RunlayerError("Runlayer response exceeded the size limit.")
                        if line.startswith("data:"):
                            data.append(line[5:].lstrip())
                        elif not line and data:
                            value = json.loads("\n".join(data))
                            data = []
                            if isinstance(value, dict) and value.get("id") == request_id:
                                return value
                raise RunlayerError("Runlayer returned no supported MCP reply.")
        except httpx.HTTPError as exc:
            # Tools can have side effects: no automatic replays after failures.
            raise RunlayerError(f"Runlayer transport failed ({type(exc).__name__}).") from None
        except (ValueError, TypeError):
            raise RunlayerError("Runlayer returned malformed MCP data.") from None

    def close(self) -> None:
        if self._session:
            try:
                self._send(None, None, method="DELETE")
            except RunlayerError:
                pass
        if self._owns_http:
            self.http.close()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


def _client() -> RunlayerClient:
    return RunlayerClient()
