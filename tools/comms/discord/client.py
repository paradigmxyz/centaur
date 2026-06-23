"""Discord self-token client."""

import asyncio
import re
from typing import Any

import httpx

import discord
from centaur_sdk import secret

BASE_URL = "https://discord.com/api/v10"
INVITE_RE = re.compile(r"(?:https?://)?(?:discord(?:\.gg|\.com/invite)/)?([A-Za-z0-9-]+)")


def _run(coro):
    return asyncio.run(coro)


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
        if response.status_code == 204:
            return {}
        if response.status_code >= 400:
            try:
                message = response.json().get("message", response.text)
            except Exception:
                message = response.text
            raise RuntimeError(f"Discord API error ({response.status_code}): {message}")
        return response.json()

    async def _with_client(self, action):
        client = discord.Client()
        done = asyncio.Event()
        result: Any = None
        error: BaseException | None = None

        @client.event
        async def on_ready():
            nonlocal result, error
            try:
                result = await action(client)
            except BaseException as exc:
                error = exc
            finally:
                done.set()
                await client.close()

        await client.start(self._get_token())
        await done.wait()
        if error:
            raise error
        return result

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

        async def action(client):
            rows = []
            for guild in client.guilds:
                if query and query.lower() not in guild.name.lower():
                    continue
                rows.append({"id": str(guild.id), "name": guild.name, "member_count": guild.member_count})
                if len(rows) >= limit:
                    break
            return rows

        return _run(self._with_client(action))

    def list_channels(self, guild: str, query: str = "") -> list[dict[str, Any]]:
        """List text channels in a server by name or ID."""

        async def action(client):
            resolved = self._find_guild(client, guild)
            rows = []
            for channel in resolved.text_channels:
                if query and query.lower().lstrip("#") not in channel.name.lower():
                    continue
                rows.append(
                    {
                        "id": str(channel.id),
                        "name": channel.name,
                        "guild_id": str(resolved.id),
                        "guild_name": resolved.name,
                    }
                )
            return rows

        return _run(self._with_client(action))

    def get_messages(self, channel: str, limit: int = 50) -> list[dict[str, Any]]:
        """Get recent messages from a channel by name or ID."""

        async def action(client):
            resolved = self._find_channel(client, channel)
            messages = [self._format_message(msg) async for msg in resolved.history(limit=limit)]
            return list(reversed(messages))

        return _run(self._with_client(action))

    def search_messages(self, query: str, channel: str, limit: int = 50) -> list[dict[str, Any]]:
        """Search recent messages in one channel by name or ID."""

        async def action(client):
            resolved = self._find_channel(client, channel)
            rows = []
            async for msg in resolved.history(limit=500):
                if query.lower() in (msg.content or "").lower():
                    rows.append(self._format_message(msg))
                    if len(rows) >= limit:
                        break
            return list(reversed(rows))

        return _run(self._with_client(action))

    def search_all(self, guild: str, query: str, limit: int = 50) -> list[dict[str, Any]]:
        """Search messages across a server by name or ID."""

        async def action(client):
            resolved = self._find_guild(client, guild)
            rows = []
            async for msg in resolved.search(query, limit=limit):
                rows.append(self._format_message(msg, channel_name=f"#{msg.channel.name}"))
            return rows

        return _run(self._with_client(action))

    def get_context(
        self,
        channel: str,
        message_id: str,
        before: int = 10,
        after: int = 10,
    ) -> list[dict[str, Any]]:
        """Get messages around a specific message."""

        async def action(client):
            resolved = self._find_channel(client, channel)
            target = await resolved.fetch_message(int(message_id))
            after_msgs = [msg async for msg in resolved.history(limit=after, after=target)]
            before_msgs = [msg async for msg in resolved.history(limit=before, before=target)]
            return [self._format_message(msg) for msg in [*reversed(after_msgs), target, *before_msgs]]

        return _run(self._with_client(action))

    def post_message(
        self,
        channel: str,
        content: str,
        reply_to_message_id: str | None = None,
    ) -> dict[str, Any]:
        """Post a message to a channel by name or ID."""

        async def action(client):
            resolved = self._find_channel(client, channel)
            reference = None
            if reply_to_message_id:
                reference = await resolved.fetch_message(int(reply_to_message_id))
            msg = await resolved.send(content, reference=reference)
            return self._format_message(msg)

        return _run(self._with_client(action))

    def _find_guild(self, client, guild_str: str):
        if guild_str.isdigit():
            guild = client.get_guild(int(guild_str))
            if guild:
                return guild
        for guild in client.guilds:
            if guild.name.lower() == guild_str.lower():
                return guild
        for guild in client.guilds:
            if guild_str.lower() in guild.name.lower():
                return guild
        raise RuntimeError(f"Guild not found: {guild_str}")

    def _find_channel(self, client, channel_str: str):
        if channel_str.isdigit():
            channel = client.get_channel(int(channel_str))
            if channel:
                return channel
        needle = channel_str.lstrip("#").lower()
        for guild in client.guilds:
            for channel in guild.text_channels:
                if channel.name.lower() == needle:
                    return channel
        for guild in client.guilds:
            for channel in guild.text_channels:
                if needle in channel.name.lower():
                    return channel
        raise RuntimeError(f"Channel not found: {channel_str}")

    def _format_message(self, msg, channel_name: str | None = None) -> dict[str, Any]:
        return {
            "id": str(msg.id),
            "channel_id": str(msg.channel.id),
            "channel_name": channel_name or getattr(msg.channel, "name", None),
            "author": getattr(msg.author, "display_name", None) or str(msg.author),
            "author_id": str(msg.author.id),
            "timestamp": msg.created_at.isoformat(),
            "content": msg.content or "",
            "reply_to": str(msg.reference.message_id) if msg.reference else None,
        }


def _client() -> DiscordClient:
    return DiscordClient()
