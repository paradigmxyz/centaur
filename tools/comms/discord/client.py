"""Discord self-token client."""

import re
import time
from datetime import datetime, timezone
from typing import Any

import httpx

from centaur_sdk import secret

BASE_URL = "https://discord.com/api/v10"
INVITE_RE = re.compile(r"(?:https?://)?(?:discord(?:\.gg|\.com/invite)/)?([A-Za-z0-9-]+)")


class DiscordClient:
    """High-level Discord client using a regular user token."""

    def __init__(self, token: str | None = None, timeout: float = 30.0):
        self._token = token
        self.timeout = timeout

    def _get_token(self) -> str:
        token = self._token or secret("DISCORD_BOT_TOKEN", "")
        if not token:
            raise RuntimeError("DISCORD_BOT_TOKEN not set.")
        return token

    def _request(self, method: str, endpoint: str, **kwargs) -> dict[str, Any] | list[Any]:
        headers = {
            "Authorization": self._get_token(),
            "Content-Type": "application/json",
            "User-Agent": (
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 "
                "(KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"
            ),
        }
        with httpx.Client(timeout=self.timeout) as client:
            response = client.request(method, f"{BASE_URL}{endpoint}", headers=headers, **kwargs)
            if response.status_code == 429:
                try:
                    retry_after = float(response.json().get("retry_after", 0))
                except Exception:
                    retry_after = 0
                if 0 < retry_after <= self.timeout:
                    time.sleep(retry_after)
                    response = client.request(method, f"{BASE_URL}{endpoint}", headers=headers, **kwargs)
        if response.status_code == 204:
            return {}
        if response.status_code >= 400:
            try:
                message = response.json().get("message", response.text)
            except Exception:
                message = response.text
            raise RuntimeError(f"Discord API error ({response.status_code}): {message}")
        return response.json()

    def get_me(self) -> dict[str, Any]:
        """Get the current Discord user."""
        data = dict(self._request("GET", "/users/@me"))
        return {
            "id": data.get("id"),
            "username": data.get("username"),
            "discriminator": data.get("discriminator"),
            "global_name": data.get("global_name"),
        }

    def join_server(self, invite: str) -> dict[str, Any]:
        """Join a server using an invite code or invite URL."""
        match = INVITE_RE.fullmatch(invite.strip())
        if not match:
            raise ValueError("Provide a Discord invite code or URL.")
        return dict(self._request("POST", f"/invites/{match.group(1)}", json={}))

    def list_servers(self, query: str = "", limit: int = 100) -> list[dict[str, Any]]:
        """List joined servers/guilds."""
        rows = []
        for guild in self._request("GET", "/users/@me/guilds"):
            if query and query.lower() not in guild.get("name", "").lower():
                continue
            rows.append(
                {
                    "id": str(guild.get("id", "")),
                    "name": guild.get("name", ""),
                    "member_count": guild.get("approximate_member_count"),
                }
            )
            if len(rows) >= limit:
                break
        return rows

    def list_channels(self, guild: str, query: str = "") -> list[dict[str, Any]]:
        """List text channels in a server by name or ID."""
        resolved = self._find_guild(guild)
        rows = []
        needle = query.lower().lstrip("#")
        for channel in self._guild_channels(resolved["id"]):
            if channel.get("type") != 0:
                continue
            if needle and needle not in channel.get("name", "").lower():
                continue
            rows.append(
                {
                    "id": str(channel.get("id", "")),
                    "name": channel.get("name", ""),
                    "guild_id": str(resolved["id"]),
                    "guild_name": resolved["name"],
                }
            )
        return rows

    def get_messages(self, channel: str, limit: int = 50) -> list[dict[str, Any]]:
        """Get recent messages from a channel by name or ID."""
        resolved = self._find_channel(channel)
        messages = self._request("GET", f"/channels/{resolved['id']}/messages", params={"limit": min(limit, 100)})
        return list(reversed([self._format_message(msg, resolved.get("name")) for msg in messages]))

    def search_messages(self, query: str, channel: str, limit: int = 50) -> list[dict[str, Any]]:
        """Search recent messages in one channel by name or ID."""
        resolved = self._find_channel(channel)
        rows = []
        before = None
        while len(rows) < limit:
            params: dict[str, Any] = {"limit": 100}
            if before:
                params["before"] = before
            messages = self._request("GET", f"/channels/{resolved['id']}/messages", params=params)
            if not messages:
                break
            for msg in messages:
                if query.lower() in (msg.get("content") or "").lower():
                    rows.append(self._format_message(msg, resolved.get("name")))
                    if len(rows) >= limit:
                        break
            before = messages[-1].get("id")
        return list(reversed(rows))

    def search_all(self, guild: str, query: str, limit: int = 50) -> list[dict[str, Any]]:
        """Search messages across a server by name or ID."""
        resolved = self._find_guild(guild)
        channels = [c for c in self._guild_channels(resolved["id"]) if c.get("type") == 0]
        rows = []
        for channel in channels:
            try:
                rows.extend(self.search_messages(query, channel["id"], limit=limit - len(rows)))
            except RuntimeError:
                continue
            if len(rows) >= limit:
                break
        return rows

    def get_context(
        self,
        channel: str,
        message_id: str,
        before: int = 10,
        after: int = 10,
    ) -> list[dict[str, Any]]:
        """Get messages around a specific message."""
        resolved = self._find_channel(channel)
        target = self._request("GET", f"/channels/{resolved['id']}/messages/{message_id}")
        before_msgs = self._request(
            "GET",
            f"/channels/{resolved['id']}/messages",
            params={"limit": min(before, 100), "before": message_id},
        )
        after_msgs = self._request(
            "GET",
            f"/channels/{resolved['id']}/messages",
            params={"limit": min(after, 100), "after": message_id},
        )
        messages = [*reversed(after_msgs), target, *before_msgs]
        return [self._format_message(msg, resolved.get("name")) for msg in messages]

    def post_message(
        self,
        channel: str,
        content: str,
        reply_to_message_id: str | None = None,
    ) -> dict[str, Any]:
        """Post a message to a channel by name or ID."""
        resolved = self._find_channel(channel)
        payload: dict[str, Any] = {"content": content}
        if reply_to_message_id:
            payload["message_reference"] = {"message_id": reply_to_message_id}
        msg = self._request("POST", f"/channels/{resolved['id']}/messages", json=payload)
        return self._format_message(msg, resolved.get("name"))

    def _guild_channels(self, guild_id: str) -> list[dict[str, Any]]:
        return list(self._request("GET", f"/guilds/{guild_id}/channels"))

    def _find_guild(self, guild_str: str) -> dict[str, Any]:
        guilds = list(self._request("GET", "/users/@me/guilds"))
        if guild_str.isdigit():
            for guild in guilds:
                if guild.get("id") == guild_str:
                    return {"id": str(guild["id"]), "name": guild.get("name", "")}
        for guild in guilds:
            if guild.get("name", "").lower() == guild_str.lower():
                return {"id": str(guild["id"]), "name": guild.get("name", "")}
        for guild in guilds:
            if guild_str.lower() in guild.get("name", "").lower():
                return {"id": str(guild["id"]), "name": guild.get("name", "")}
        raise RuntimeError(f"Guild not found: {guild_str}")

    def _find_channel(self, channel_str: str) -> dict[str, Any]:
        if channel_str.isdigit():
            channel = dict(self._request("GET", f"/channels/{channel_str}"))
            return {
                "id": str(channel["id"]),
                "name": channel.get("name"),
                "guild_id": str(channel.get("guild_id", "")),
                "guild_name": None,
            }
        needle = channel_str.lstrip("#").lower()
        partial = None
        for guild in self._request("GET", "/users/@me/guilds"):
            for channel in self._guild_channels(guild["id"]):
                if channel.get("type") != 0:
                    continue
                name = channel.get("name", "")
                if name.lower() == needle:
                    return {
                        "id": str(channel["id"]),
                        "name": name,
                        "guild_id": str(guild["id"]),
                        "guild_name": guild.get("name"),
                    }
                if partial is None and needle in name.lower():
                    partial = {
                        "id": str(channel["id"]),
                        "name": name,
                        "guild_id": str(guild["id"]),
                        "guild_name": guild.get("name"),
                    }
        if partial:
            return partial
        raise RuntimeError(f"Channel not found: {channel_str}")

    def _format_message(self, msg: dict[str, Any], channel_name: str | None = None) -> dict[str, Any]:
        author = msg.get("author") or {}
        return {
            "id": str(msg.get("id", "")),
            "channel_id": str(msg.get("channel_id", "")),
            "channel_name": channel_name,
            "author": author.get("global_name") or author.get("username") or "",
            "author_id": str(author.get("id", "")),
            "timestamp": _format_timestamp(msg),
            "content": msg.get("content") or "",
            "reply_to": ((msg.get("message_reference") or {}).get("message_id")),
        }


def _client() -> DiscordClient:
    return DiscordClient()


def _format_timestamp(msg: dict[str, Any]) -> str:
    timestamp = msg.get("timestamp")
    if timestamp:
        return str(timestamp)
    snowflake = msg.get("id")
    if not snowflake:
        return ""
    created_ms = (int(snowflake) >> 22) + 1420070400000
    return datetime.fromtimestamp(created_ms / 1000, tz=timezone.utc).isoformat()
