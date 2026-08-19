from __future__ import annotations

import importlib.util
import json
import sys
import types
from pathlib import Path

from typer.testing import CliRunner

package = types.ModuleType("centaur_tool_sentry")
package.__path__ = [str(Path(__file__).parent)]
sys.modules.setdefault("centaur_tool_sentry", package)

spec = importlib.util.spec_from_file_location(
    "centaur_tool_sentry.cli", Path(__file__).with_name("cli.py")
)
assert spec and spec.loader
cli = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = cli
spec.loader.exec_module(cli)


class FakeClient:
    def __init__(self) -> None:
        self.calls: list[tuple[str, tuple, dict]] = []
        self.closed = 0

    def _record(self, method: str, *args, **kwargs):
        self.calls.append((method, args, kwargs))
        return {"method": method}

    def list_organizations(self):
        return self._record("list_organizations")

    def list_projects(self, organization_slug: str):
        return self._record("list_projects", organization_slug)

    def list_issues(self, **kwargs):
        return self._record("list_issues", **kwargs)

    def get_issue(self, organization_slug: str, issue_id: str):
        return self._record("get_issue", organization_slug, issue_id)

    def list_issue_events(self, **kwargs):
        return self._record("list_issue_events", **kwargs)

    def get_event(self, organization_slug: str, issue_id: str, event_id: str):
        return self._record("get_event", organization_slug, issue_id, event_id=event_id)

    def get_issue_tag_values(self, organization_slug: str, issue_id: str, tag_key: str):
        return self._record("get_issue_tag_values", organization_slug, issue_id, tag_key)

    def close(self) -> None:
        self.closed += 1


def test_commands_wrap_existing_client_methods(monkeypatch) -> None:
    client = FakeClient()
    monkeypatch.setattr(cli, "get_client", lambda: client)
    runner = CliRunner()

    invocations = [
        (["list-organizations"], "list_organizations"),
        (["list-projects", "splits"], "list_projects"),
        (
            [
                "list-issues",
                "splits",
                "--project",
                "server",
                "--query",
                "is:resolved level:error",
                "--sort",
                "freq",
                "--stats-period",
                "24h",
                "--limit",
                "10",
            ],
            "list_issues",
        ),
        (["get-issue", "splits", "SERVER-1"], "get_issue"),
        (
            ["list-issue-events", "splits", "SERVER-1", "--full", "--limit", "5"],
            "list_issue_events",
        ),
        (["get-event", "splits", "SERVER-1", "--event-id", "deadbeef"], "get_event"),
        (
            ["get-issue-tag-values", "splits", "SERVER-1", "release"],
            "get_issue_tag_values",
        ),
    ]

    for args, expected_method in invocations:
        result = runner.invoke(cli.app, args)
        assert result.exit_code == 0, result.output
        assert json.loads(result.output) == {"method": expected_method}

    assert client.calls == [
        ("list_organizations", (), {}),
        ("list_projects", ("splits",), {}),
        (
            "list_issues",
            (),
            {
                "organization_slug": "splits",
                "project_slug": "server",
                "query": "is:resolved level:error",
                "sort": "freq",
                "stats_period": "24h",
                "limit": 10,
            },
        ),
        ("get_issue", ("splits", "SERVER-1"), {}),
        (
            "list_issue_events",
            (),
            {
                "organization_slug": "splits",
                "issue_id": "SERVER-1",
                "full": True,
                "limit": 5,
            },
        ),
        ("get_event", ("splits", "SERVER-1"), {"event_id": "deadbeef"}),
        ("get_issue_tag_values", ("splits", "SERVER-1", "release"), {}),
    ]
    assert client.closed == len(invocations)
