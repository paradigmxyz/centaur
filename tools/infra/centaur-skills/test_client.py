import json

import httpx
import pytest
from cli import create, edit, list_skills, read, search
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


def test_create_posts_skill_fields_and_returns_author_payload():
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.method == "POST"
        assert request.url.path == SANDBOX_SKILLS_PATH
        assert json.loads(request.content) == {
            "data": {
                "name": "incident-triage",
                "description": "Triage incidents.",
                "instructions": "# Workflow\n\nInvestigate the alert.",
            }
        }
        return json_response(
            {
                "data": {
                    "id": "skl_123",
                    "name": "incident-triage",
                    "lock_version": 0,
                }
            },
            status_code=201,
        )

    result = make_client(handler).create(
        "incident-triage",
        "Triage incidents.",
        "# Workflow\n\nInvestigate the alert.",
    )

    assert result == {
        "id": "skl_123",
        "name": "incident-triage",
        "lock_version": 0,
    }


def test_edit_patches_only_provided_fields_with_lock_version():
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.method == "PATCH"
        assert request.url.path == f"{SANDBOX_SKILLS_PATH}/skl_123"
        assert json.loads(request.content) == {
            "data": {
                "description": "Updated incident guidance.",
                "lock_version": 2,
            }
        }
        return json_response(
            {
                "data": {
                    "id": "skl_123",
                    "description": "Updated incident guidance.",
                    "lock_version": 3,
                }
            }
        )

    result = make_client(handler).edit(
        "skl_123",
        description="Updated incident guidance.",
        lock_version=2,
    )

    assert result["lock_version"] == 3


def test_edit_requires_a_field():
    with pytest.raises(ValueError, match="at least one skill field"):
        make_client(lambda _request: json_response({})).edit("skl_123")


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


def test_cli_create_reads_instructions_file(monkeypatch, capsys, tmp_path):
    instructions_file = tmp_path / "instructions.md"
    instructions_file.write_text("# Workflow\n\nInvestigate the alert.\n")

    class StubClient:
        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return None

        def create(self, name, description, instructions):
            assert name == "incident-triage"
            assert description == "Triage incidents."
            assert instructions == "# Workflow\n\nInvestigate the alert.\n"
            return {"id": "skl_123", "name": name, "lock_version": 0}

    monkeypatch.setattr("cli.get_client", StubClient)

    create(
        "incident-triage",
        description="Triage incidents.",
        instructions=None,
        instructions_file=instructions_file,
    )

    assert json.loads(capsys.readouterr().out) == {
        "data": {"id": "skl_123", "name": "incident-triage", "lock_version": 0}
    }


def test_cli_edit_sends_partial_fields(monkeypatch, capsys):
    class StubClient:
        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return None

        def edit(self, identifier, *, name, description, instructions, lock_version):
            assert identifier == "skl_123"
            assert name is None
            assert description == "Updated guidance."
            assert instructions is None
            assert lock_version == 2
            return {"id": identifier, "description": description, "lock_version": 3}

    monkeypatch.setattr("cli.get_client", StubClient)

    edit(
        "skl_123",
        name=None,
        description="Updated guidance.",
        instructions=None,
        instructions_file=None,
        lock_version=2,
    )

    assert json.loads(capsys.readouterr().out) == {
        "data": {
            "id": "skl_123",
            "description": "Updated guidance.",
            "lock_version": 3,
        }
    }
