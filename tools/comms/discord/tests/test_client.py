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


def _thread_response(thread_type=11):
    return {
        "id": "99",
        "name": "design chat",
        "parent_id": "11",
        "guild_id": "1",
        "owner_id": "5",
        "type": thread_type,
        "thread_metadata": {"archived": False},
    }


def test_create_thread_from_message_branches_off_starter(monkeypatch):
    client = DiscordClient(token="unused")
    posts = []

    def fake_request(method, endpoint, **kwargs):
        if method == "POST":
            posts.append((endpoint, kwargs.get("json")))
        if endpoint == "/channels/11":
            return {"id": "11", "name": "general", "guild_id": "1"}
        if endpoint == "/channels/11/messages/123/threads":
            return _thread_response()
        raise AssertionError(f"{method} {endpoint}")

    monkeypatch.setattr(client, "_request", fake_request)

    result = client.create_thread("11", "design chat", from_message_id="123")

    # Exactly one POST: branch the thread off the message, no seed message.
    assert posts == [("/channels/11/messages/123/threads", {"name": "design chat"})]
    assert result == {
        "id": "99",
        "name": "design chat",
        "parent_id": "11",
        "guild_id": "1",
        "owner_id": "5",
        "type": "public_thread",
        "archived": False,
        "url": "https://discord.com/channels/1/99",
    }


def test_create_thread_standalone_posts_initial_content(monkeypatch):
    client = DiscordClient(token="unused")
    posts = []

    def fake_request(method, endpoint, **kwargs):
        if method == "POST":
            posts.append((endpoint, kwargs.get("json")))
        if endpoint == "/channels/11":
            return {"id": "11", "name": "general", "guild_id": "1"}
        if endpoint == "/channels/11/threads":
            return _thread_response(thread_type=12)
        if endpoint == "/channels/99/messages":
            return {"id": "1000"}
        raise AssertionError(f"{method} {endpoint}")

    monkeypatch.setattr(client, "_request", fake_request)

    result = client.create_thread("11", "design chat", content="kickoff", private=True)

    assert posts[0] == ("/channels/11/threads", {"name": "design chat", "type": 12})
    assert posts[1] == ("/channels/99/messages", {"content": "kickoff"})
    assert result["id"] == "99"
    assert result["type"] == "private_thread"
