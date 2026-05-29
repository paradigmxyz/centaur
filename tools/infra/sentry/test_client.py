from __future__ import annotations

import importlib.util
from pathlib import Path
from typing import Any

spec = importlib.util.spec_from_file_location("sentry_client", Path(__file__).with_name("client.py"))
assert spec and spec.loader
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
SentryClient = module.SentryClient


class RecordingSentryClient(SentryClient):
    """Captures requests instead of hitting the network."""

    def __init__(self) -> None:
        super().__init__(url="https://sentry.io", auth_token="t")
        self.calls: list[dict[str, Any]] = []
        self.response: Any = []

    def _request(self, method: str, path: str, params: dict | None = None) -> Any:
        clean = {k: v for k, v in (params or {}).items() if v is not None}
        self.calls.append({"method": method, "path": path, "params": clean})
        return self.response

    @property
    def last(self) -> dict[str, Any]:
        return self.calls[-1]


def test_base_url_appends_api_prefix() -> None:
    assert SentryClient(url="https://sentry.io").base_url == "https://sentry.io/api/0"
    assert SentryClient(url="sentry.example.com").base_url == "https://sentry.example.com/api/0"


def test_list_issues_project_scoped() -> None:
    client = RecordingSentryClient()

    client.list_issues("acme", project_slug="web", query="is:unresolved level:error")

    assert client.last["path"] == "/projects/acme/web/issues/"
    assert client.last["params"] == {
        "query": "is:unresolved level:error",
        "sort": "date",
        "statsPeriod": "14d",
        "limit": 25,
    }
    assert "project" not in client.last["params"]


def test_list_issues_org_scoped_sets_all_projects() -> None:
    client = RecordingSentryClient()

    client.list_issues("acme", sort="freq", stats_period="24h", limit=10)

    assert client.last["path"] == "/organizations/acme/issues/"
    assert client.last["params"] == {
        "query": "is:unresolved",
        "sort": "freq",
        "statsPeriod": "24h",
        "limit": 10,
        "project": -1,
    }


def test_get_event_defaults_to_latest() -> None:
    client = RecordingSentryClient()

    client.get_event("acme", "ISSUE-1")

    assert client.last["path"] == "/organizations/acme/issues/ISSUE-1/events/latest/"


def test_get_event_accepts_specific_id() -> None:
    client = RecordingSentryClient()

    client.get_event("acme", "ISSUE-1", event_id="deadbeef")

    assert client.last["path"] == "/organizations/acme/issues/ISSUE-1/events/deadbeef/"


def test_list_issue_events_serializes_full_flag() -> None:
    client = RecordingSentryClient()

    client.list_issue_events("acme", "ISSUE-1", full=True, limit=5)

    assert client.last["path"] == "/organizations/acme/issues/ISSUE-1/events/"
    assert client.last["params"] == {"full": "true", "per_page": 5}


def test_get_issue_tag_values_url_encodes_tag() -> None:
    client = RecordingSentryClient()

    client.get_issue_tag_values("acme", "ISSUE-1", "server name")

    assert client.last["path"] == "/organizations/acme/issues/ISSUE-1/tags/server%20name/"
