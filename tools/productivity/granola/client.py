"""Granola client with two backends behind one interface.

- MCP backend (preferred): https://mcp.granola.ai/mcp, authenticated with a
  user-scoped OAuth token minted by the Centaur console consent flow. The
  sandbox proxy injects the Bearer token for the mcp.granola.ai host, so the
  tool never handles the credential. Sees only the connected user's meetings.
- REST backend (fallback): the official Enterprise public API
  (https://docs.granola.ai) at public-api.granola.ai, authenticated with a
  workspace API key (GRANOLA_API_KEY). Workspace-wide access.

`_client()` tries MCP first and falls back to REST. Override with
GRANOLA_BACKEND=mcp|rest.
"""

import json
import re
from datetime import datetime
from typing import Any

import httpx
from centaur_sdk import secret

API_BASE = "https://public-api.granola.ai"
MCP_URL = "https://mcp.granola.ai/mcp"


class GranolaClient:
    """Client for Granola Enterprise API (workspace-wide notes access)."""

    def __init__(self, api_key: str | None = None):
        self._api_key = api_key or secret("GRANOLA_API_KEY", "")
        if not self._api_key:
            raise RuntimeError(
                "GRANOLA_API_KEY not set.\n"
                "Generate one at Settings → Workspaces → API tab (Enterprise plan required)."
            )
        self._client = httpx.Client(
            base_url=API_BASE,
            headers={
                "Authorization": f"Bearer {self._api_key}",
                "Content-Type": "application/json",
            },
            timeout=30.0,
        )

    def close(self) -> None:
        self._client.close()

    def _get(self, path: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        """Make authenticated GET request."""
        response = self._client.get(path, params=params)
        response.raise_for_status()
        return response.json()

    def list_notes(
        self,
        page_size: int = 30,
        cursor: str | None = None,
        created_before: str | None = None,
        created_after: str | None = None,
        updated_after: str | None = None,
    ) -> dict[str, Any]:
        """List meeting notes across the workspace.

        Returns {notes: [...], hasMore: bool, cursor: str|None}.
        Use cursor for pagination. Dates in ISO 8601 format.
        """
        params: dict[str, Any] = {"page_size": min(page_size, 30)}
        if cursor:
            params["cursor"] = cursor
        if created_before:
            params["created_before"] = created_before
        if created_after:
            params["created_after"] = created_after
        if updated_after:
            params["updated_after"] = updated_after
        return self._get("/v1/notes", params=params)

    def get_note(self, note_id: str, include_transcript: bool = False) -> dict[str, Any]:
        """Fetch a single note by ID (not_* format, e.g. not_1d3tmYTlCICgjy).

        Returns full note with title, owner, attendees, summary_markdown,
        calendar_event, folder_membership, and optionally transcript.
        """
        params: dict[str, Any] = {}
        if include_transcript:
            params["include"] = "transcript"
        return self._get(f"/v1/notes/{note_id}", params=params)

    def list_all_notes(
        self,
        limit: int = 50,
        created_after: str | None = None,
        updated_after: str | None = None,
    ) -> list[dict[str, Any]]:
        """Paginate through notes up to limit. Convenience wrapper over list_notes."""
        all_notes: list[dict[str, Any]] = []
        cursor: str | None = None
        while len(all_notes) < limit:
            page_size = min(30, limit - len(all_notes))
            result = self.list_notes(
                page_size=page_size,
                cursor=cursor,
                created_after=created_after,
                updated_after=updated_after,
            )
            notes = result.get("notes", [])
            all_notes.extend(notes)
            if not result.get("hasMore") or not result.get("cursor"):
                break
            cursor = result["cursor"]
        return all_notes[:limit]

    def get_transcript(self, note_id: str) -> list[dict[str, Any]]:
        """Fetch transcript for a note. Returns list of utterances."""
        note = self.get_note(note_id, include_transcript=True)
        return note.get("transcript") or []

    def search_notes(self, query: str, limit: int = 50) -> list[dict[str, Any]]:
        """Search notes by title keyword. Case-insensitive substring match.

        Paginates through workspace notes and returns those whose title
        contains the query string.
        """
        query_lower = query.lower()
        all_notes = self.list_all_notes(limit=200)
        return [n for n in all_notes if query_lower in (n.get("title") or "").lower()][:limit]


# Matches one <meeting ...>...</meeting> block in MCP meetings_data output.
_MEETING_RE = re.compile(
    r'<meeting id="(?P<id>[^"]+)" title="(?P<title>[^"]*)" date="(?P<date>[^"]*)">'
    r"(?P<body>.*?)</meeting>",
    re.DOTALL,
)
_PARTICIPANTS_RE = re.compile(r"<known_participants>(.*?)</known_participants>", re.DOTALL)
_SUMMARY_RE = re.compile(r"<summary>(.*?)</summary>", re.DOTALL)
# "Zygimantas (note creator) from Tempo <z@tempo.xyz>" -> name + email
_PARTICIPANT_RE = re.compile(r"(?P<name>[^,<]+?)\s*<(?P<email>[^>]+)>")


def _parse_meeting_date(raw: str) -> str:
    """Convert MCP dates like 'Jul 8, 2026 5:30 PM GMT+2' to ISO, best effort."""
    m = re.match(r"(\w+ \d+, \d+ \d+:\d+ [AP]M) GMT(?P<off>[+-]\d+)?", raw)
    if not m:
        return raw
    try:
        dt = datetime.strptime(m.group(1), "%b %d, %Y %I:%M %p")
        off = m.group("off")
        return dt.isoformat() + (f"{int(off):+03d}:00" if off else "")
    except ValueError:
        return raw


def _parse_meetings(text: str) -> list[dict[str, Any]]:
    """Parse MCP meetings_data text into REST-shaped note dicts."""
    notes = []
    for m in _MEETING_RE.finditer(text):
        body = m.group("body")
        attendees = []
        pm = _PARTICIPANTS_RE.search(body)
        if pm:
            for p in _PARTICIPANT_RE.finditer(pm.group(1)):
                attendees.append({"name": p.group("name").strip(), "email": p.group("email")})
        owner = next(
            (a for a in attendees if "(note creator)" in a["name"]),
            attendees[0] if attendees else {},
        )
        if owner:
            owner = {**owner, "name": owner["name"].replace("(note creator)", "").split(" from ")[0].strip()}
        sm = _SUMMARY_RE.search(body)
        notes.append(
            {
                "id": m.group("id"),
                "title": m.group("title"),
                "created_at": _parse_meeting_date(m.group("date")),
                "owner": owner,
                "attendees": attendees,
                "summary_markdown": sm.group(1).strip() if sm else None,
            }
        )
    return notes


class GranolaMcpClient:
    """Client for the Granola MCP server (user-scoped meeting access).

    Speaks Streamable HTTP JSON-RPC. The server is stateless (no session id).
    In the Centaur sandbox the proxy injects the OAuth Bearer token for
    mcp.granola.ai; outside it, set GRANOLA_MCP_TOKEN.
    """

    def __init__(self, token: str | None = None):
        headers = {
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
        }
        token = token or secret("GRANOLA_MCP_TOKEN", "")
        if token:
            headers["Authorization"] = f"Bearer {token}"
        self._client = httpx.Client(headers=headers, timeout=30.0)
        self._rpc_id = 0

    def close(self) -> None:
        self._client.close()

    def _rpc(self, method: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        self._rpc_id += 1
        response = self._client.post(
            MCP_URL,
            json={"jsonrpc": "2.0", "id": self._rpc_id, "method": method, "params": params or {}},
        )
        response.raise_for_status()
        body = response.text
        if response.headers.get("content-type", "").startswith("text/event-stream"):
            payloads = [line[6:] for line in body.splitlines() if line.startswith("data: ")]
            if not payloads:
                raise RuntimeError(f"empty MCP event stream for {method}")
            msg = json.loads(payloads[-1])
        else:
            msg = json.loads(body)
        if "error" in msg:
            raise RuntimeError(f"MCP error from {method}: {msg['error']}")
        return msg["result"]

    def _call_tool(self, name: str, arguments: dict[str, Any] | None = None) -> str:
        result = self._rpc("tools/call", {"name": name, "arguments": arguments or {}})
        if result.get("isError"):
            raise RuntimeError(f"granola MCP tool {name} failed: {result}")
        return "\n".join(c.get("text", "") for c in result.get("content", []) if c.get("type") == "text")

    def get_account_info(self) -> dict[str, Any]:
        """Email and active workspace of the connected Granola account."""
        return json.loads(self._call_tool("get_account_info"))

    def list_folders(self) -> str:
        """List meeting folders (raw text: id, title, description, note count)."""
        return self._call_tool("list_meeting_folders")

    def query(self, query: str, document_ids: list[str] | None = None) -> str:
        """Ask Granola a natural-language question about your meetings."""
        args: dict[str, Any] = {"query": query}
        if document_ids:
            args["document_ids"] = document_ids
        return self._call_tool("query_granola_meetings", args)

    def list_notes(
        self,
        page_size: int = 30,
        cursor: str | None = None,
        created_before: str | None = None,
        created_after: str | None = None,
        updated_after: str | None = None,
    ) -> dict[str, Any]:
        """List the connected user's meetings, REST-shaped.

        Returns {notes: [...], hasMore: False, cursor: None} (MCP has no
        pagination). Date filters map onto the MCP custom time range.
        """
        args: dict[str, Any]
        if created_after or created_before:
            args = {
                "time_range": "custom",
                "custom_start": (created_after or "2000-01-01")[:10],
                "custom_end": (created_before or datetime.now().strftime("%Y-%m-%d"))[:10],
            }
        else:
            args = {"time_range": "last_30_days"}
        text = self._call_tool("list_meetings", args)
        return {"notes": _parse_meetings(text)[:page_size], "hasMore": False, "cursor": None}

    def list_all_notes(
        self,
        limit: int = 50,
        created_after: str | None = None,
        updated_after: str | None = None,
    ) -> list[dict[str, Any]]:
        """List meetings up to limit. Without a date filter, covers the last year."""
        created_after = created_after or updated_after
        if not created_after:
            now = datetime.now()
            created_after = now.replace(year=now.year - 1).strftime("%Y-%m-%d")
        result = self.list_notes(page_size=limit, created_after=created_after)
        return result["notes"][:limit]

    def get_note(self, note_id: str, include_transcript: bool = False) -> dict[str, Any]:
        """Fetch a single meeting by UUID, REST-shaped (title, owner, attendees,
        summary_markdown, optionally transcript)."""
        text = self._call_tool("get_meetings", {"meeting_ids": [note_id]})
        notes = _parse_meetings(text)
        if not notes:
            raise RuntimeError(f"meeting {note_id} not found")
        note = notes[0]
        if include_transcript:
            note["transcript"] = self.get_transcript(note_id)
        return note

    def get_transcript(self, note_id: str) -> list[dict[str, Any]]:
        """Fetch the transcript for a meeting. Returns a list of utterances
        (single block if the MCP text is not line-structured)."""
        text = self._call_tool("get_meeting_transcript", {"meeting_id": note_id})
        if not text.strip():
            return []
        return [{"speaker": {"source": "transcript"}, "text": text.strip()}]

    def search_notes(self, query: str, limit: int = 50) -> list[dict[str, Any]]:
        """Search meetings by title keyword. Case-insensitive substring match."""
        query_lower = query.lower()
        all_notes = self.list_all_notes(limit=200)
        return [n for n in all_notes if query_lower in (n.get("title") or "").lower()][:limit]


def _client() -> GranolaMcpClient | GranolaClient:
    """Pick a backend: MCP first, REST fallback. GRANOLA_BACKEND=mcp|rest overrides."""
    backend = secret("GRANOLA_BACKEND", "").lower()
    if backend == "rest":
        return GranolaClient()
    if backend == "mcp":
        return GranolaMcpClient()

    mcp = GranolaMcpClient()
    try:
        mcp.get_account_info()
        return mcp
    except Exception as mcp_error:
        mcp.close()
        try:
            return GranolaClient()
        except Exception as rest_error:
            raise RuntimeError(
                "No working Granola backend.\n"
                f"MCP ({MCP_URL}): {mcp_error}\n"
                f"REST ({API_BASE}): {rest_error}\n"
                "Connect Granola via the Centaur console OAuth flow (MCP), "
                "or set GRANOLA_API_KEY (Enterprise REST API)."
            ) from mcp_error
