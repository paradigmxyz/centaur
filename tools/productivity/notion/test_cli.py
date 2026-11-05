"""Tests for the Notion CLI's page property updates against a mocked Notion API."""

import json

import httpx
import pytest
from centaur_tool_notion import cli
from centaur_tool_notion.client import API_BASE, NotionClient
from typer.testing import CliRunner

PAGE_ID = "0123456789abcdef0123456789abcdef"
UPDATED_PAGE = {
    "object": "page",
    "id": "01234567-89ab-cdef-0123-456789abcdef",
    "url": f"https://www.notion.so/Task-{PAGE_ID}",
    "properties": {"Name": {"type": "title", "title": [{"plain_text": "Task"}]}},
}

runner = CliRunner()


@pytest.fixture
def notion_api(monkeypatch):
    """Route the CLI's Notion client through a handler and record each request."""
    requests: list[httpx.Request] = []
    state = {"response": httpx.Response(200, json=UPDATED_PAGE)}

    def handler(request: httpx.Request) -> httpx.Response:
        requests.append(request)
        return state["response"]

    client = NotionClient(api_key="test-key")
    client._http = httpx.Client(
        base_url=API_BASE, headers=client._http.headers, transport=httpx.MockTransport(handler)
    )
    monkeypatch.setattr(cli, "get_client", lambda: client)
    return requests, state


def _invoke(*args: str):
    return runner.invoke(cli.app, ["update-page", *args])


def test_update_page_patches_properties_as_given(notion_api):
    requests, _ = notion_api
    properties = {
        "Priority": {"select": {"name": "P0"}},
        "Status": {"status": {"name": "In progress"}},
        "Tags": {"multi_select": [{"name": "api"}, {"name": "docs"}]},
        "Due": {"date": {"start": "2026-01-31"}},
        "Owner": {"people": [{"id": "user-1"}]},
        "Related": {"relation": [{"id": "page-2"}]},
        "Link": {"url": "https://example.com"},
        "Done": {"checkbox": True},
    }

    result = _invoke(PAGE_ID, "--properties", json.dumps(properties))

    assert result.exit_code == 0, result.output
    assert "Updated: Task" in result.output
    [request] = requests
    assert request.method == "PATCH"
    assert request.url.path == f"/v1/pages/{PAGE_ID}"
    assert request.headers["Notion-Version"] == "2022-06-28"
    assert json.loads(request.content) == {"properties": properties}


def test_update_page_sends_clearing_values(notion_api):
    requests, _ = notion_api
    properties = {"Priority": {"select": None}, "Due": {"date": None}, "Tags": {"multi_select": []}}

    result = _invoke(PAGE_ID, "--properties", json.dumps(properties))

    assert result.exit_code == 0, result.output
    assert json.loads(requests[0].content) == {"properties": properties}


def test_update_page_json_output_returns_updated_page(notion_api):
    result = _invoke(PAGE_ID, "--properties", '{"Done": {"checkbox": true}}', "--json")

    assert result.exit_code == 0, result.output
    assert json.loads(result.stdout) == UPDATED_PAGE


def test_update_page_accepts_page_url(notion_api):
    requests, _ = notion_api

    result = _invoke(
        f"https://app.notion.com/p/Task-{PAGE_ID}?pvs=4",
        "--properties",
        '{"Done": {"checkbox": true}}',
    )

    assert result.exit_code == 0, result.output
    assert requests[0].url.path == f"/v1/pages/{PAGE_ID}"


def test_update_page_reports_notion_validation_message(notion_api):
    _, state = notion_api
    message = 'Invalid status option. Status option "Shipped" does not exist'
    state["response"] = httpx.Response(
        400,
        json={"object": "error", "status": 400, "code": "validation_error", "message": message},
    )

    result = _invoke(PAGE_ID, "--properties", '{"Status": {"status": {"name": "Shipped"}}}')

    assert result.exit_code == 1
    assert result.stderr.strip() == f"Notion API error 400: {message}"
    assert "Traceback" not in result.output


def test_update_page_reports_status_without_error_body(notion_api):
    _, state = notion_api
    state["response"] = httpx.Response(502, text="Bad gateway")

    result = _invoke(PAGE_ID, "--properties", '{"Done": {"checkbox": true}}')

    assert result.exit_code == 1
    assert "502 Bad Gateway" in result.stderr


@pytest.mark.parametrize("value", ["{not json", "[]", "{}", '"P0"'])
def test_update_page_rejects_non_object_properties(notion_api, value):
    requests, _ = notion_api

    result = _invoke(PAGE_ID, "--properties", value)

    assert result.exit_code == 2
    assert "--properties" in result.output
    assert requests == []


@pytest.mark.parametrize(
    "url_or_id",
    [
        PAGE_ID,
        "01234567-89ab-cdef-0123-456789abcdef",
        f"https://www.notion.so/workspace/Task-{PAGE_ID}",
        f"https://example.notion.site/Task-{PAGE_ID}",
        f"https://app.notion.com/p/Task-{PAGE_ID}",
        f"https://app.notion.com/p/Multi-Word-Title-{PAGE_ID}?pvs=4&x=a-b",
        f"https://www.notion.so/{PAGE_ID}#fedcba9876543210fedcba9876543210",
    ],
)
def test_extract_id(url_or_id):
    assert cli.extract_id(url_or_id) == PAGE_ID
