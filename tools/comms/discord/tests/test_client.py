import sys
from pathlib import Path
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from client import DiscordClient


def test_join_server_posts_invite_code(monkeypatch):
    client = DiscordClient(token="unused")

    def fake_request(method, endpoint, **kwargs):
        assert method == "POST"
        assert endpoint == "/invites/abc123"
        return {"code": "abc123", "guild": {"name": "Test"}}

    monkeypatch.setattr(client, "_request", fake_request)

    assert client.join_server("https://discord.gg/abc123")["guild"]["name"] == "Test"


def test_find_guild_exact_then_partial_name():
    client = DiscordClient(token="unused")
    discord_client = SimpleNamespace(
        guilds=[
            SimpleNamespace(id=1, name="General"),
            SimpleNamespace(id=2, name="Eth R&D"),
        ],
        get_guild=lambda guild_id: None,
    )

    assert client._find_guild(discord_client, "Eth R&D").id == 2
    assert client._find_guild(discord_client, "eth").id == 2


def test_find_channel_supports_hash_prefix_and_partial_name():
    client = DiscordClient(token="unused")
    channel = SimpleNamespace(id=11, name="announcements")
    discord_client = SimpleNamespace(
        guilds=[SimpleNamespace(text_channels=[channel])],
        get_channel=lambda channel_id: None,
    )

    assert client._find_channel(discord_client, "#announcements").id == 11
    assert client._find_channel(discord_client, "announce").id == 11
