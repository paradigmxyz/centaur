from __future__ import annotations

from centaur_sdk.backends import StubBackend, configure

from centaur_tool_preqin.client import OPERATIONAL_TOKEN_PLACEHOLDER, PreqinClient


class FakeResponse:
    def __init__(self, payload: dict, status_code: int = 200):
        self._payload = payload
        self.status_code = status_code
        self.text = ""

    def json(self) -> dict:
        return self._payload

    def raise_for_status(self) -> None:
        if self.status_code >= 400:
            raise RuntimeError(self.status_code)


class FakeHttpClient:
    def __init__(self):
        self.posts: list[dict] = []
        self.gets: list[dict] = []

    def post(self, url: str, **kwargs):
        self.posts.append({"url": url, **kwargs})
        return FakeResponse({"access_token": "token-123"})

    def get(self, url: str, **kwargs):
        self.gets.append({"url": url, **kwargs})
        return FakeResponse({"data": []})


def test_credential_status_does_not_treat_stub_placeholders_as_present():
    configure(StubBackend())
    status = PreqinClient().credential_status()

    assert status["PREQIN_USERNAME"]["present"] is False
    assert status["PREQIN_API_KEY"]["present"] is False


def test_auth_uses_username_and_api_key_multipart_form():
    fake = FakeHttpClient()
    client = PreqinClient(username="user", api_key="api-key")
    client._client = fake

    assert client._operational_access_token(force_refresh=True) == "token-123"

    request = fake.posts[0]
    assert request["url"] == "https://api.preqin.com/connect/token"
    assert request["files"] == {"username": (None, "user"), "apikey": (None, "api-key")}
    assert request["headers"] == {"Accept": "application/json"}


def test_operational_get_uses_proxy_token_placeholder_before_direct_auth():
    configure(StubBackend())
    fake = FakeHttpClient()
    client = PreqinClient()
    client._client = fake

    client.get_funds(fund_name="Paradigm", size=1)

    assert not fake.posts
    assert fake.gets[0]["headers"]["Authorization"] == f"Bearer {OPERATIONAL_TOKEN_PLACEHOLDER}"


def test_operational_get_without_proxy_token_or_direct_credentials_leaves_auth_to_proxy(monkeypatch):
    monkeypatch.setenv(OPERATIONAL_TOKEN_PLACEHOLDER, "")
    configure(StubBackend())
    fake = FakeHttpClient()
    client = PreqinClient()
    client._client = fake

    client.get_funds(fund_name="Paradigm", size=1)

    assert not fake.posts
    assert fake.gets[0]["headers"] == {"Accept": "application/json"}


def test_auth_health_uses_injected_auth_without_direct_credentials(monkeypatch):
    monkeypatch.setenv(OPERATIONAL_TOKEN_PLACEHOLDER, "")
    configure(StubBackend())
    fake = FakeHttpClient()
    client = PreqinClient()
    client._client = fake

    result = client.auth_health()

    assert result["ok"] is True
    assert result["method"] == "operational_get"
    assert not fake.posts
    assert fake.gets[0]["url"] == "https://api.preqin.com/api/FundManager"
    assert fake.gets[0]["params"] == {"Size": 1, "Page": 1}
    assert fake.gets[0]["headers"] == {"Accept": "application/json"}


def test_auth_health_uses_direct_credentials_for_local_runs(monkeypatch):
    monkeypatch.setenv(OPERATIONAL_TOKEN_PLACEHOLDER, "")
    fake = FakeHttpClient()
    client = PreqinClient(username="user", api_key="api-key")
    client._client = fake

    result = client.auth_health()

    assert result["ok"] is True
    assert result["method"] == "operational_get"
    assert fake.posts[0]["url"] == "https://api.preqin.com/connect/token"
    assert fake.posts[0]["files"] == {"username": (None, "user"), "apikey": (None, "api-key")}
    assert fake.gets[0]["url"] == "https://api.preqin.com/api/FundManager"
    assert fake.gets[0]["headers"]["Authorization"] == "Bearer token-123"
