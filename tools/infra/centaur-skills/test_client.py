import json

import httpx
import pytest
from cli import (
    add_editor,
    create,
    delete,
    edit,
    editors,
    list_skills,
    read,
    remove_editor,
    search,
)
from client import (
    SANDBOX_SKILLS_PATH,
    RepositorySkillsCatalog,
    SkillsCatalog,
    SkillsClient,
)


def json_response(payload, status_code=200):
    return httpx.Response(status_code, json=payload)


def make_client(handler, *, bearer_token=None):
    return SkillsClient(
        url="http://centaur-console:3000",
        bearer_token=bearer_token,
        transport=httpx.MockTransport(handler),
    )


def make_catalog(
    skills_dir,
    *,
    listed=None,
    searched=None,
    read_result=None,
    created=None,
):
    requests = []

    def handler(request: httpx.Request) -> httpx.Response:
        requests.append(request)
        if request.method == "POST":
            data = created
        elif request.url.path == f"{SANDBOX_SKILLS_PATH}/search":
            data = searched
        elif request.url.path == SANDBOX_SKILLS_PATH:
            data = listed
        else:
            data = read_result
        assert data is not None, f"unexpected Console request: {request.url}"
        return json_response({"data": data}, status_code=201 if created else 200)

    catalog = SkillsCatalog(
        repository=RepositorySkillsCatalog(skills_dir),
        url="http://centaur-console:3000",
        transport=httpx.MockTransport(handler),
    )
    return catalog, requests


def write_skill(skills_dir, name, description, body="# Workflow\n\nDo it.\n"):
    skill_dir = skills_dir / name
    skill_dir.mkdir(parents=True)
    document = f'---\nname: {name}\ndescription: "{description}"\n---\n\n{body}'
    (skill_dir / "SKILL.md").write_text(document)
    return document


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


def test_delete_archives_skill_by_oid():
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.method == "DELETE"
        assert request.url.path == f"{SANDBOX_SKILLS_PATH}/skl_123"
        return httpx.Response(204)

    result = make_client(handler).delete("skl_123")

    assert result is None


def test_list_add_and_remove_editors_use_editor_endpoint():
    requests = []

    def handler(request: httpx.Request) -> httpx.Response:
        requests.append(request)
        return json_response(
            {
                "data": {
                    "id": "skl_123",
                    "editors": [
                        {
                            "id": "usr_456",
                            "email": "editor@example.com",
                            "name": "Editor",
                            "status": "active",
                        }
                    ],
                    "lock_version": 3,
                }
            }
        )

    client = make_client(handler)
    listed = client.list_editors("skl_123")
    added = client.add_editor("skl_123", " editor@example.com ")
    removed = client.remove_editor("skl_123", "usr_456")

    assert listed["editors"][0]["id"] == "usr_456"
    assert added["lock_version"] == 3
    assert removed["id"] == "skl_123"
    assert [request.method for request in requests] == ["GET", "POST", "DELETE"]
    assert all(request.url.path == f"{SANDBOX_SKILLS_PATH}/skl_123/editors" for request in requests)
    assert json.loads(requests[1].content) == {"data": {"user": "editor@example.com"}}
    assert json.loads(requests[2].content) == {"data": {"user": "usr_456"}}


@pytest.mark.parametrize("method", ["add_editor", "remove_editor"])
def test_editor_mutations_require_a_user(method):
    client = make_client(lambda _request: json_response({}))

    with pytest.raises(ValueError, match="must not be empty"):
        getattr(client, method)("skl_123", " ")


def test_requests_wrap_http_errors_without_exposing_credentials():
    def handler(_request: httpx.Request) -> httpx.Response:
        return json_response({"error": {"message": "invalid sandbox token"}}, status_code=401)

    with pytest.raises(RuntimeError, match="HTTP 401"):
        make_client(handler, bearer_token="secret-token").search("anything")


def test_repository_catalog_lists_searches_and_reads_skill_documents(tmp_path):
    skills_dir = tmp_path / ".agents" / "skills"
    document = write_skill(
        skills_dir,
        "docsend",
        "Download documents and files from DocSend Spaces.",
    )
    write_skill(skills_dir, "incident-triage", "Investigate production incidents.")
    catalog = RepositorySkillsCatalog(skills_dir)

    listed = catalog.list()
    searched = catalog.search("download docsend")
    read_result = catalog.read("repo:docsend")

    assert [skill["name"] for skill in listed] == ["docsend", "incident-triage"]
    assert listed[0]["id"] == "repo:docsend"
    assert listed[0]["source"] == "repository"
    assert "document" not in listed[0]
    assert [skill["name"] for skill in searched] == ["docsend"]
    assert catalog.search("Do it") == []
    assert read_result["document"] == document


def test_repository_catalog_parses_multiline_yaml_description(tmp_path):
    skills_dir = tmp_path / ".agents" / "skills"
    skill_dir = skills_dir / "docsend"
    skill_dir.mkdir(parents=True)
    (skill_dir / "SKILL.md").write_text(
        """---
name: docsend
description: >-
  Download protected documents and files
  from DocSend Spaces.
---

# DocSend
"""
    )
    catalog = RepositorySkillsCatalog(skills_dir)

    assert catalog.list()[0]["description"] == (
        "Download protected documents and files from DocSend Spaces."
    )
    assert [skill["name"] for skill in catalog.search("protected DocSend Spaces")] == ["docsend"]


def test_merged_catalog_preserves_same_name_rows_and_reads_by_distinct_ids(tmp_path):
    skills_dir = tmp_path / ".agents" / "skills"
    local_document = write_skill(skills_dir, "docsend", "Local DocSend guidance.")
    listed = [
        {"id": "skl_duplicate", "name": "docsend", "visibility": "shared"},
        {"id": "skl_console", "name": "console-only", "visibility": "private"},
    ]
    searched = [
        {"id": "skl_duplicate", "name": "docsend", "visibility": "shared"},
        {"id": "skl_console", "name": "console-only", "visibility": "private"},
    ]

    catalog, _ = make_catalog(
        skills_dir,
        listed=listed,
        searched=searched,
        read_result={
            "id": "skl_duplicate",
            "name": "docsend",
            "document": "Console document",
        },
    )

    catalog_list = catalog.list()
    catalog_search = catalog.search("docsend")

    assert [(skill["id"], skill["name"], skill["source"]) for skill in catalog_list] == [
        ("repo:docsend", "docsend", "repository"),
        ("skl_duplicate", "docsend", "console"),
        ("skl_console", "console-only", "console"),
    ]
    assert [(skill["id"], skill["name"], skill["source"]) for skill in catalog_search] == [
        ("repo:docsend", "docsend", "repository"),
        ("skl_duplicate", "docsend", "console"),
        ("skl_console", "console-only", "console"),
    ]
    assert catalog.read("docsend")["document"] == local_document
    assert catalog.read("skl_duplicate")["source"] == "console"


def test_merged_catalog_scope_can_select_one_source(tmp_path):
    skills_dir = tmp_path / ".agents" / "skills"
    write_skill(skills_dir, "docsend", "Local DocSend guidance.")
    catalog, requests = make_catalog(
        skills_dir,
        listed=[{"id": "skl_console", "name": "console-only"}],
    )

    assert [skill["name"] for skill in catalog.list(scope="repository")] == ["docsend"]
    assert [skill["name"] for skill in catalog.list(scope="private")] == ["console-only"]
    assert len(requests) == 1
    assert dict(requests[0].url.params) == {"limit": "20", "scope": "private"}


def test_merged_catalog_limit_does_not_starve_console_results(tmp_path):
    skills_dir = tmp_path / ".agents" / "skills"
    write_skill(skills_dir, "alpha", "First local skill.")
    write_skill(skills_dir, "beta", "Second local skill.")

    catalog, _ = make_catalog(
        skills_dir,
        listed=[
            {"id": "skl_one", "name": "console-one"},
            {"id": "skl_two", "name": "console-two"},
        ],
    )

    assert [(skill["name"], skill["source"]) for skill in catalog.list(limit=2)] == [
        ("alpha", "repository"),
        ("console-one", "console"),
    ]


def test_merged_catalog_reports_missing_explicit_repository_identifier(tmp_path):
    catalog, requests = make_catalog(tmp_path / "missing")

    with pytest.raises(RuntimeError, match="repository skill not found"):
        catalog.read("repo:missing")
    assert requests == []


def test_inherited_console_mutations_remain_available(tmp_path):
    catalog, requests = make_catalog(
        tmp_path / "missing",
        created={"id": "skl_123", "name": "incident-triage"},
    )

    assert catalog.create("incident-triage", "Triage incidents.", "Do it.") == {
        "id": "skl_123",
        "name": "incident-triage",
    }
    assert requests[0].method == "POST"


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


def test_cli_delete_archives_skill(monkeypatch, capsys):
    class StubClient:
        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return None

        def delete(self, identifier):
            assert identifier == "skl_123"

    monkeypatch.setattr("cli.get_client", StubClient)

    delete("skl_123")

    assert json.loads(capsys.readouterr().out) == {"data": {"id": "skl_123", "archived": True}}


def test_cli_lists_adds_and_removes_editors(monkeypatch, capsys):
    class StubClient:
        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return None

        def list_editors(self, identifier):
            assert identifier == "skl_123"
            return {"id": identifier, "editors": [], "lock_version": 1}

        def add_editor(self, identifier, user):
            assert identifier == "skl_123"
            assert user == "editor@example.com"
            return {"id": identifier, "editors": [{"id": "usr_456"}], "lock_version": 2}

        def remove_editor(self, identifier, user):
            assert identifier == "skl_123"
            assert user == "usr_456"
            return {"id": identifier, "editors": [], "lock_version": 3}

    monkeypatch.setattr("cli.get_client", StubClient)

    editors("skl_123")
    assert json.loads(capsys.readouterr().out)["data"]["editors"] == []

    add_editor("skl_123", "editor@example.com")
    assert json.loads(capsys.readouterr().out)["data"]["editors"] == [{"id": "usr_456"}]

    remove_editor("skl_123", "usr_456")
    assert json.loads(capsys.readouterr().out)["data"]["editors"] == []
