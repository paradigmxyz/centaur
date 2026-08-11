import httpx
import pytest
from cli import (
    _catalog_metadata,
    _merge_skill_results,
    _parse_builtin,
    _skill_document,
    skills_read,
)
from client import (
    SANDBOX_OAUTH_APPS_PATH,
    SANDBOX_PERMISSIONS_PATH,
    SANDBOX_SKILLS_PATH,
    ConsoleClient,
)


def json_response(payload, status_code=200):
    return httpx.Response(status_code, json=payload)


def make_client(handler, *, bearer_token=None):
    return ConsoleClient(
        url="http://centaur-console:3000",
        bearer_token=bearer_token,
        transport=httpx.MockTransport(handler),
    )


def test_sandbox_permissions_fetches_and_unwraps_data():
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.method == "GET"
        assert request.url.path == SANDBOX_PERMISSIONS_PATH
        assert request.headers["Accept"] == "application/json"
        return json_response(
            {
                "data": {
                    "sandbox_id": "sandbox-1",
                    "principal_id": "prn_123",
                    "permissions": {"secrets": []},
                }
            }
        )

    result = make_client(handler).sandbox_permissions()

    assert result["sandbox_id"] == "sandbox-1"
    assert result["principal_id"] == "prn_123"
    assert result["permissions"] == {"secrets": []}


def test_sandbox_permissions_sends_debug_bearer_token_when_provided():
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.headers["Authorization"] == "Bearer test-token"
        return json_response({"data": {"sandbox_id": "sandbox-1"}})

    assert make_client(handler, bearer_token="test-token").permissions()["sandbox_id"] == "sandbox-1"


def test_sandbox_permissions_wraps_http_errors():
    def handler(_request: httpx.Request) -> httpx.Response:
        return json_response({"error": {"message": "invalid sandbox token"}}, status_code=401)

    with pytest.raises(RuntimeError, match="HTTP 401"):
        make_client(handler).sandbox_permissions()


def test_sandbox_oauth_apps_fetches_and_unwraps_data():
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.method == "GET"
        assert request.url.path == SANDBOX_OAUTH_APPS_PATH
        assert request.headers["Accept"] == "application/json"
        return json_response(
            {
                "data": [
                    {
                        "slug": "google",
                        "provider": "google",
                        "start_url": "https://console.example/oauth/google/start",
                    }
                ]
            }
        )

    result = make_client(handler).sandbox_oauth_apps()

    assert result == [
        {
            "slug": "google",
            "provider": "google",
            "start_url": "https://console.example/oauth/google/start",
        }
    ]


def test_sandbox_oauth_apps_wraps_http_errors():
    def handler(_request: httpx.Request) -> httpx.Response:
        return json_response({"error": {"message": "invalid sandbox token"}}, status_code=401)

    with pytest.raises(RuntimeError, match="HTTP 401"):
        make_client(handler).sandbox_oauth_apps()


def test_skills_list_and_search_use_sandbox_catalog_endpoints():
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
    listed = client.skills_list(scope="private", limit=5)
    searched = client.skills_search("incident response", limit=3)

    assert listed[0]["id"] == "skl_123"
    assert searched[0]["name"] == "incident-triage"
    assert requests[0].url.path == SANDBOX_SKILLS_PATH
    assert dict(requests[0].url.params) == {"limit": "5", "scope": "private"}
    assert requests[1].url.path == f"{SANDBOX_SKILLS_PATH}/search"
    assert dict(requests[1].url.params) == {"q": "incident response", "limit": "3"}


def test_skill_read_uses_skill_id_and_returns_document():
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path == f"{SANDBOX_SKILLS_PATH}/skl_123"
        return json_response(
            {
                "data": {
                    "id": "skl_123",
                    "name": "incident-triage",
                    "document": "---\nname: incident-triage\ndescription: Triage incidents.\n---\n\nDo it.\n",
                }
            }
        )

    result = make_client(handler).skill_read("skl_123")

    assert result["id"] == "skl_123"
    assert result["document"].startswith("---\n")


def test_skills_wrap_http_errors_without_exposing_credentials():
    def handler(_request: httpx.Request) -> httpx.Response:
        return json_response({"error": {"message": "invalid sandbox token"}}, status_code=401)

    with pytest.raises(RuntimeError, match="HTTP 401"):
        make_client(handler, bearer_token="secret-token").skills_search("anything")


def test_builtin_skill_parsing_and_catalog_metadata(tmp_path):
    skill_dir = tmp_path / "incident-triage"
    skill_dir.mkdir()
    skill_file = skill_dir / "SKILL.md"
    skill_file.write_text(
        "---\nname: incident-triage\ndescription: Triage incidents.\n---\n\nDo it.\n",
        encoding="utf-8",
    )

    parsed = _parse_builtin(skill_file)

    assert parsed is not None
    assert parsed["name"] == "incident-triage"
    assert "ref" not in parsed
    assert len(parsed["checksum"]) == 64
    assert _catalog_metadata(parsed) == {
        key: value for key, value in parsed.items() if key not in {"content", "path"}
    }


def test_builtin_skill_name_cannot_conflict_with_console_oid(tmp_path):
    skill_dir = tmp_path / "invalid"
    skill_dir.mkdir()
    skill_file = skill_dir / "SKILL.md"
    skill_file.write_text(
        "---\nname: skl_123\ndescription: Looks like an OID.\n---\n\nDo it.\n",
        encoding="utf-8",
    )

    assert _parse_builtin(skill_file) is None


def test_skills_read_accepts_builtin_name(monkeypatch, capsys):
    monkeypatch.setattr(
        "cli._builtin_skills",
        lambda: [{"name": "incident-response", "content": "# Incident Response\n"}],
    )

    skills_read("incident-response", json_output=False, markdown_output=False)

    assert capsys.readouterr().out == "# Incident Response\n"


def test_catalog_merge_interleaves_builtin_and_remote_results():
    builtin = [{"name": "one"}, {"name": "two"}]
    remote = [{"id": "skl_one"}, {"id": "skl_two"}]

    assert [item.get("id") or item.get("name") for item in _merge_skill_results(builtin, remote, limit=3)] == [
        "one",
        "skl_one",
        "two",
    ]


def test_skill_document_supports_builtin_and_console_payloads():
    assert _skill_document({"content": "builtin document"}) == "builtin document"
    assert _skill_document({"document": "Console document"}) == "Console document"


def test_health_returns_identity_details():
    def handler(_request: httpx.Request) -> httpx.Response:
        return json_response(
            {
                "data": {
                    "sandbox_id": "sandbox-1",
                    "proxy_id": "proxy-1",
                    "principal_id": "principal-1",
                }
            }
        )

    result = make_client(handler).health()

    assert result == {
        "ok": True,
        "tool": "centaur-console",
        "error": None,
        "details": {
            "sandbox_id": "sandbox-1",
            "proxy_id": "proxy-1",
            "principal_id": "principal-1",
        },
    }
