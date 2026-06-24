import sys
from pathlib import Path

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


def test_list_servers_uses_rest(monkeypatch):
    client = DiscordClient(token="unused")

    def fake_request(method, endpoint, **kwargs):
        assert method == "GET"
        assert endpoint == "/users/@me/guilds"
        return [
            {"id": "1", "name": "General", "approximate_member_count": 10},
            {"id": "2", "name": "Eth R&D", "approximate_member_count": 20},
        ]

    monkeypatch.setattr(client, "_request", fake_request)

    assert client.list_servers("eth") == [{"id": "2", "name": "Eth R&D", "member_count": 20}]


def test_find_guild_exact_then_partial_name(monkeypatch):
    client = DiscordClient(token="unused")

    def fake_request(method, endpoint, **kwargs):
        assert method == "GET"
        assert endpoint == "/users/@me/guilds"
        return [
            {"id": "1", "name": "General"},
            {"id": "2", "name": "Eth R&D"},
        ]

    monkeypatch.setattr(client, "_request", fake_request)

    assert client._find_guild("Eth R&D")["id"] == "2"
    assert client._find_guild("eth")["id"] == "2"


def test_find_channel_supports_hash_prefix_and_partial_name(monkeypatch):
    client = DiscordClient(token="unused")

    def fake_request(method, endpoint, **kwargs):
        assert method == "GET"
        if endpoint == "/users/@me/guilds":
            return [{"id": "1", "name": "General"}]
        if endpoint == "/guilds/1/channels":
            return [{"id": "11", "name": "announcements", "type": 0}]
        raise AssertionError(endpoint)

    monkeypatch.setattr(client, "_request", fake_request)

    assert client._find_channel("#announcements")["id"] == "11"
    assert client._find_channel("announce")["id"] == "11"


def test_find_channel_by_id_uses_channel_endpoint(monkeypatch):
    client = DiscordClient(token="unused")

    def fake_request(method, endpoint, **kwargs):
        assert method == "GET"
        assert endpoint == "/channels/11"
        return {"id": "11", "name": "announcements", "guild_id": "1"}

    monkeypatch.setattr(client, "_request", fake_request)

    assert client._find_channel("11") == {
        "id": "11",
        "name": "announcements",
        "guild_id": "1",
        "guild_name": None,
    }
