import json

import httpx
import pytest
from cli import list_skills, read, search
from client import SANDBOX_SKILLS_PATH, SkillsClient


def json_response(payload, status_code=200):
    return httpx.Response(status_code, json=payload)


def make_client(handler, *, bearer_token=None):
    return SkillsClient(
        url="http://centaur-console:3000",
        bearer_token=bearer_token,
        transport=httpx.MockTransport(handler),
    )


def test_list_and_search_use_sandbox_catalog_endpoints():
    requests = []

    def handler(request: httpx.Request) -> httpx.Response:
        requests.append(request)
        return json_response(
            {
                "data": [
                    {
                        "id": "skl_123",
                        "name": "incident-triage",
                        "visibility": "private",
                    }
                ]
            }
        )

    client = make_client(handler)
    listed = client.list(scope="private", limit=5)
    searched = client.search("incident response", limit=3)

    assert listed[0]["id"] == "skl_123"
    assert searched[0]["name"] == "incident-triage"
    assert requests[0].url.path == SANDBOX_SKILLS_PATH
    assert dict(requests[0].url.params) == {"limit": "5", "scope": "private"}
    assert requests[1].url.path == f"{SANDBOX_SKILLS_PATH}/search"
    assert dict(requests[1].url.params) == {"q": "incident response", "limit": "3"}


@pytest.mark.parametrize("identifier", ["skl_123", "incident-triage"])
def test_read_uses_skill_name_or_oid_and_returns_document(identifier):
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path == f"{SANDBOX_SKILLS_PATH}/{identifier}"
        return json_response(
            {
                "data": {
                    "id": "skl_123",
                    "name": "incident-triage",
                    "document": "---\nname: incident-triage\ndescription: Triage incidents.\n---\n\nDo it.\n",
                }
            }
        )

    result = make_client(handler).read(identifier)

    assert result["id"] == "skl_123"
    assert result["document"].startswith("---\n")


def test_requests_wrap_http_errors_without_exposing_credentials():
    def handler(_request: httpx.Request) -> httpx.Response:
        return json_response({"error": {"message": "invalid sandbox token"}}, status_code=401)

    with pytest.raises(RuntimeError, match="HTTP 401"):
        make_client(handler, bearer_token="secret-token").search("anything")


def test_cli_search_and_list_output_json(monkeypatch, capsys):
    class StubClient:
        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return None

        def search(self, query, *, limit):
            assert query == "incident response"
            assert limit == 3
            return [{"id": "skl_123", "name": "incident-triage"}]

        def list(self, *, scope, limit):
            assert scope == "private"
            assert limit == 5
            return [{"id": "skl_456", "name": "private-skill"}]

    monkeypatch.setattr("cli.get_client", StubClient)

    search("incident response", limit=3)
    assert json.loads(capsys.readouterr().out) == {
        "data": [{"id": "skl_123", "name": "incident-triage"}]
    }

    list_skills(scope="private", limit=5)
    assert json.loads(capsys.readouterr().out) == {
        "data": [{"id": "skl_456", "name": "private-skill"}]
    }


def test_cli_read_outputs_raw_skill_markdown(monkeypatch, capsys):
    class StubClient:
        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return None

        def read(self, identifier):
            assert identifier == "incident-response"
            return {"document": "# Incident Response\n"}

    monkeypatch.setattr("cli.get_client", StubClient)

    read("incident-response")

    assert capsys.readouterr().out == "# Incident Response\n"
