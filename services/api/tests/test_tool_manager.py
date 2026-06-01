"""Unit tests for pure functions in api.tool_manager."""

import sys
from dataclasses import dataclass
from enum import Enum
from pathlib import Path
from typing import Optional, Union

import httpx
import pytest
from fastapi import FastAPI

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from api.tool_manager import (  # noqa: E402
    _LIFECYCLE_METHODS,
    _describe_method_docstring,
    _friendly_type_name,
    _normalize_for_serialization,
    _tool_arg_validation_error,
    _to_toon,
    ToolManager,
    ToolMethod,
)


class TestDescribeMethodDocstring:
    """Generic agent-facing description extraction (no persona-specific cases).

    Tools across personas all flow through describe_tool, which exposes
    methods to the agent via `call discover <tool>`. Keeping the human-prose
    portion of the docstring (not just the first line) gives the agent
    enough signal to pick the right method without reading the source.
    """

    def test_empty_or_none_returns_empty_string(self):
        assert _describe_method_docstring(None) == ""
        assert _describe_method_docstring("") == ""
        assert _describe_method_docstring("   \n  \n") == ""

    def test_single_line_docstring_returned_verbatim(self):
        assert _describe_method_docstring("Search Slack for messages.") == "Search Slack for messages."

    def test_multi_paragraph_description_preserved(self):
        doc = """Hybrid research engine.

        This is the default entry point for any research-shaped turn. It
        does not write the final reply. Instead it returns the working set.
        """
        out = _describe_method_docstring(doc)
        assert "Hybrid research engine." in out
        assert "default entry point" in out

    def test_google_section_marker_truncates(self):
        doc = """Search the database.

        Returns ranked results matching the query.

        Args:
            query: Free-form search text.
            limit: Max results.
        """
        out = _describe_method_docstring(doc)
        assert "Search the database." in out
        assert "Returns ranked results" in out
        # Args block excluded — parameter info ships separately on the schema.
        assert "Args:" not in out
        assert "query:" not in out

    def test_truncation_respects_max_chars(self):
        long_para = "x " * 2000
        doc = f"Summary.\n\n{long_para}"
        out = _describe_method_docstring(doc)
        assert len(out) <= 1200
        assert out.endswith("\u2026")


# ---------------------------------------------------------------------------
# _normalize_for_serialization
# ---------------------------------------------------------------------------


class TestNormalizeNoneAndPrimitives:
    def test_none(self):
        assert _normalize_for_serialization(None) is None

    def test_str(self):
        assert _normalize_for_serialization("hello") == "hello"

    def test_int(self):
        assert _normalize_for_serialization(42) == 42

    def test_float(self):
        assert _normalize_for_serialization(3.14) == 3.14

    def test_bool(self):
        assert _normalize_for_serialization(True) is True


class TestNormalizeBytes:
    def test_small_bytes_base64(self):
        data = b"hello"
        result = _normalize_for_serialization(data)
        assert result["encoding"] == "base64"
        assert result["byte_length"] == 5
        assert "content_base64" in result

    def test_large_bytes_preview(self):
        data = b"x" * (2 * 1024 * 1024)  # 2 MB — exceeds default 1 MB limit
        result = _normalize_for_serialization(data)
        assert result["encoding"] == "base64_preview"
        assert result["byte_length"] == len(data)


class TestNormalizeEnum:
    def test_enum_returns_value(self):
        class Color(Enum):
            RED = "red"
            BLUE = "blue"

        assert _normalize_for_serialization(Color.RED) == "red"


class TestNormalizeDataclass:
    def test_dataclass(self):
        @dataclass
        class Point:
            x: int
            y: int

        result = _normalize_for_serialization(Point(1, 2))
        assert result == {"x": 1, "y": 2}


class TestNormalizeCollections:
    def test_dict(self):
        result = _normalize_for_serialization({"a": 1, "b": "two"})
        assert result == {"a": 1, "b": "two"}

    def test_list(self):
        result = _normalize_for_serialization([1, "two", None])
        assert result == [1, "two", None]

    def test_nested(self):
        class Status(Enum):
            OK = "ok"

        @dataclass
        class Item:
            name: str

        data = {"items": [Item("a")], "status": Status.OK}
        result = _normalize_for_serialization(data)
        assert result == {"items": [{"name": "a"}], "status": "ok"}

    def test_tuple_becomes_list(self):
        assert _normalize_for_serialization((1, 2)) == [1, 2]

    def test_set_becomes_list(self):
        result = _normalize_for_serialization({1})
        assert result == [1]


class TestNormalizeModelDump:
    def test_model_dump_fallback(self):
        class FakeModel:
            def model_dump(self):
                return {"key": "val"}

        result = _normalize_for_serialization(FakeModel())
        assert result == {"key": "val"}

    def test_to_dict_fallback(self):
        class Legacy:
            def to_dict(self):
                return {"legacy": True}

        result = _normalize_for_serialization(Legacy())
        assert result == {"legacy": True}


# ---------------------------------------------------------------------------
# _friendly_type_name
# ---------------------------------------------------------------------------


class TestFriendlyTypeName:
    def test_str(self):
        assert _friendly_type_name(str) == "string"

    def test_int(self):
        assert _friendly_type_name(int) == "integer"

    def test_float(self):
        assert _friendly_type_name(float) == "number"

    def test_bool(self):
        assert _friendly_type_name(bool) == "boolean"

    def test_list(self):
        assert _friendly_type_name(list) == "array"

    def test_dict(self):
        assert _friendly_type_name(dict) == "object"

    def test_none_type(self):
        assert _friendly_type_name(type(None)) == "null"

    def test_optional_str(self):
        result = _friendly_type_name(Optional[str])
        assert "string" in result
        assert "null" in result

    def test_list_of_str(self):
        assert _friendly_type_name(list[str]) == "array[string]"

    def test_dict_str_int(self):
        assert _friendly_type_name(dict[str, int]) == "object[string, integer]"

    def test_union_types(self):
        result = _friendly_type_name(Union[str, int])
        assert "string" in result
        assert "integer" in result

    def test_pipe_union(self):
        result = _friendly_type_name(str | int)
        assert "string" in result
        assert "integer" in result


class TestToolArgValidation:
    def test_unexpected_argument_reports_suggestion(self):
        def fn(channel: str, limit: int = 10):
            return None

        error = _tool_arg_validation_error(
            ToolMethod("upload_file", fn),
            {"channel_id": "C123", "limit": 5},
        )

        assert error == {
            "error": "tool_argument_validation_failed",
            "message": "Unexpected argument(s): channel_id",
            "unexpected_args": ["channel_id"],
            "accepted_args": ["channel", "limit"],
            "did_you_mean": {"channel_id": "channel"},
        }

    def test_missing_required_argument_reports_shape(self):
        def fn(spreadsheet_id: str, range_notation: str, values: list):
            return None

        error = _tool_arg_validation_error(
            ToolMethod("sheets_update", fn),
            {"spreadsheet_id": "sheet"},
        )

        assert error == {
            "error": "tool_argument_validation_failed",
            "message": "Missing required argument(s): range_notation, values",
            "missing_args": ["range_notation", "values"],
            "accepted_args": ["range_notation", "spreadsheet_id", "values"],
        }

    def test_var_kwargs_method_accepts_extra_arguments(self):
        def fn(**kwargs):
            return kwargs

        assert (
            _tool_arg_validation_error(ToolMethod("dynamic", fn), {"anything": True})
            is None
        )

    def test_forbidden_path_argument_is_rejected(self):
        def fn(output_path: str):
            return output_path

        error = _tool_arg_validation_error(
            ToolMethod("drive_download", fn),
            {"output_path": "/app/tools/productivity/gsuite/client.py"},
        )

        assert error == {
            "error": "tool_argument_validation_failed",
            "message": (
                "Forbidden argument(s): output_path. Tools may not write API-process "
                "files to caller-supplied paths; return Centaur attachments instead."
            ),
            "forbidden_args": ["output_path"],
        }


# ---------------------------------------------------------------------------
# _to_toon
# ---------------------------------------------------------------------------


class TestToToon:
    def test_returns_string(self):
        assert isinstance(_to_toon({"key": "value"}), str)

    def test_handles_none(self):
        assert isinstance(_to_toon(None), str)

    def test_handles_list(self):
        assert isinstance(_to_toon([1, 2, 3]), str)

    def test_handles_nested_data(self):
        data = {"users": [{"name": "Alice", "age": 30}], "count": 1}
        result = _to_toon(data)
        assert isinstance(result, str)
        assert "Alice" in result

    def test_handles_empty_dict(self):
        result = _to_toon({})
        assert isinstance(result, str)


# ---------------------------------------------------------------------------
# _LIFECYCLE_METHODS
# ---------------------------------------------------------------------------


class TestLifecycleMethods:
    def test_close_excluded(self):
        assert "close" in _LIFECYCLE_METHODS

    def test_connect_excluded(self):
        assert "connect" in _LIFECYCLE_METHODS

    def test_disconnect_excluded(self):
        assert "disconnect" in _LIFECYCLE_METHODS

    def test_shutdown_excluded(self):
        assert "shutdown" in _LIFECYCLE_METHODS

    def test_regular_method_not_excluded(self):
        assert "search" not in _LIFECYCLE_METHODS
        assert "get" not in _LIFECYCLE_METHODS


# ---------------------------------------------------------------------------
# Integrated ToolManager behavior
# ---------------------------------------------------------------------------


class _NullBackend:
    async def get(self, key: str) -> str | None:
        return None

    async def list_keys(self) -> list[str]:
        return []

    def get_sync(self, key: str) -> str | None:
        return None


def _write_tool(
    tools_dir: Path,
    name: str,
    client_code: str,
    *,
    description: str = "Fake test tool",
    secrets: list[str] | None = None,
    optional_secrets: list[str] | None = None,
    timeout_s: int | str | None = None,
) -> Path:
    if isinstance(timeout_s, str):
        timeout_line = f'timeout_s = "{timeout_s}"'
    elif timeout_s is not None:
        timeout_line = f"timeout_s = {timeout_s}"
    else:
        timeout_line = None
    tool_dir = tools_dir / name
    tool_dir.mkdir(parents=True)
    tool_dir.joinpath("pyproject.toml").write_text(
        "\n".join(
            [
                "[project]",
                f'name = "{name}"',
                'version = "0.1.0"',
                f'description = "{description}"',
                "",
                "[tool.centaur]",
                'module = "client.py"',
                'hosts = ["api.example.com"]',
                f"secrets = {secrets or []!r}",
                f"optional_secrets = {optional_secrets or []!r}",
                *([timeout_line] if timeout_line is not None else []),
                "",
            ]
        )
    )
    tool_dir.joinpath("client.py").write_text(client_code)
    return tool_dir


def _write_persona(tools_dir: Path, name: str) -> Path:
    persona_dir = tools_dir / name
    persona_dir.mkdir(parents=True)
    persona_dir.joinpath("pyproject.toml").write_text(
        "\n".join(
            [
                "[project]",
                f'name = "{name}"',
                'version = "0.1.0"',
                'description = "A fake persona"',
                "",
                "[tool.centaur]",
                'type = "persona"',
                'engine = "codex"',
                'default_repo = "/workspace/repo"',
                'prompt = "PROMPT.md"',
                "",
            ]
        )
    )
    persona_dir.joinpath("PROMPT.md").write_text("Persona prompt body")
    persona_dir.joinpath("run.py").write_text("print('custom executor')\n")
    return persona_dir


FAKE_TOOL_CLIENT = """
from centaur_sdk import secret


class FakeClient:
    def __init__(self, source="base"):
        self.source = source

    def sync_echo(self, text: str) -> dict:
        return {"mode": "sync", "text": text, "source": self.source}

    async def async_echo(self, text: str) -> dict:
        return {"mode": "async", "text": text, "source": self.source}

    def secret_values(self) -> dict:
        return {
            "required": secret("REQ_TOKEN"),
            "optional": secret("OPT_TOKEN", default="missing"),
        }

    def _private(self):
        return "hidden"

    def close(self):
        return "lifecycle"

    @property
    def computed(self):
        return "not a method"


def _client():
    return FakeClient(source="base")
"""


OVERLAY_TOOL_CLIENT = FAKE_TOOL_CLIENT.replace(
    'FakeClient(source="base")',
    'FakeClient(source="overlay")',
)


def test_discover_loads_fake_tools_with_shadowing_personas_and_failures(tmp_path: Path):
    base_tools = tmp_path / "base"
    overlay_tools = tmp_path / "overlay"

    _write_tool(base_tools, "alpha", FAKE_TOOL_CLIENT, description="Base alpha")
    _write_tool(
        overlay_tools,
        "alpha",
        OVERLAY_TOOL_CLIENT,
        description="Overlay alpha",
        secrets=["REQ_TOKEN"],
        optional_secrets=["OPT_TOKEN"],
    )
    _write_tool(
        overlay_tools,
        "broken",
        'raise RuntimeError("broken import")\n',
        description="Broken tool",
    )
    _write_persona(overlay_tools, "code-reviewer")

    manager = ToolManager([base_tools, overlay_tools])
    loaded = manager.discover()

    assert [tool.name for tool in loaded] == ["alpha"]
    assert manager.tools["alpha"].description == "Overlay alpha"
    assert [secret.name for secret in manager.tools["alpha"].secrets] == ["REQ_TOKEN"]
    assert [secret.name for secret in manager.tools["alpha"].optional_secrets] == [
        "OPT_TOKEN"
    ]
    assert {method.method_name for method in manager.tools["alpha"].methods} == {
        "async_echo",
        "secret_values",
        "sync_echo",
    }
    assert manager.load_failures == [{"name": "broken", "error": "broken import"}]

    persona = manager.get_persona("code-reviewer")
    assert persona is not None
    assert persona.description == "A fake persona"
    assert persona.engine == "codex"
    assert persona.default_repo == "/workspace/repo"
    assert persona.prompt_content == "Persona prompt body"
    assert persona.has_custom_executor is True
    assert "code-reviewer" not in manager.tools


@pytest.mark.asyncio
async def test_call_tool_invokes_sync_and_async_methods_with_secret_placeholders(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
):
    from centaur_sdk.backends import registry

    monkeypatch.setattr(registry, "_backend", _NullBackend())
    tools_dir = tmp_path / "tools"
    _write_tool(
        tools_dir,
        "alpha",
        FAKE_TOOL_CLIENT,
        secrets=["REQ_TOKEN"],
        optional_secrets=["OPT_TOKEN"],
    )
    manager = ToolManager(tools_dir)
    manager.discover()

    assert await manager.call_tool(
        "alpha",
        "sync_echo",
        {"text": "hello"},
    ) == {"mode": "sync", "text": "hello", "source": "base"}
    assert await manager.call_tool(
        "alpha",
        "async_echo",
        {"text": "hello"},
    ) == {"mode": "async", "text": "hello", "source": "base"}

    assert await manager.call_tool("alpha", "secret_values", {}) == {
        "required": "REQ_TOKEN",
        "optional": "OPT_TOKEN",
    }
    assert await manager.call_tool_raw("alpha", "secret_values", {}) == {
        "required": "REQ_TOKEN",
        "optional": "OPT_TOKEN",
    }


@pytest.mark.asyncio
async def test_call_tool_uses_tool_specific_timeout(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
):
    captured: list[float | None] = []

    async def fake_wait_for(coro, timeout=None):
        captured.append(timeout)
        return await coro

    monkeypatch.setattr("api.tool_manager.asyncio.wait_for", fake_wait_for)
    tools_dir = tmp_path / "tools"
    _write_tool(tools_dir, "alpha", FAKE_TOOL_CLIENT, timeout_s=3600)
    manager = ToolManager(tools_dir)
    manager.discover()

    await manager.call_tool("alpha", "sync_echo", {"text": "hello"})
    assert captured == [3600.0]


@pytest.mark.asyncio
async def test_call_tool_allows_disabling_outer_timeout(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
):
    captured: list[float | None] = []

    async def fake_wait_for(coro, timeout=None):
        captured.append(timeout)
        return await coro

    monkeypatch.setattr("api.tool_manager.asyncio.wait_for", fake_wait_for)
    tools_dir = tmp_path / "tools"
    _write_tool(tools_dir, "alpha", FAKE_TOOL_CLIENT, timeout_s="none")
    manager = ToolManager(tools_dir)
    manager.discover()

    await manager.call_tool("alpha", "sync_echo", {"text": "hello"})
    assert captured == [None]


@pytest.mark.asyncio
async def test_tool_rest_router_lists_describes_and_invokes_tools(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
):
    from centaur_sdk.backends import registry

    monkeypatch.setattr(registry, "_backend", _NullBackend())
    tools_dir = tmp_path / "tools"
    _write_tool(
        tools_dir,
        "alpha",
        FAKE_TOOL_CLIENT,
        description="REST alpha",
        secrets=["REQ_TOKEN"],
        optional_secrets=["OPT_TOKEN"],
    )
    manager = ToolManager(tools_dir)
    manager.discover()
    app = FastAPI()
    app.include_router(manager.create_rest_router())

    transport = httpx.ASGITransport(app=app)
    async with httpx.AsyncClient(transport=transport, base_url="http://test") as client:
        list_response = await client.get("/tools")
        assert list_response.status_code == 200
        assert list_response.json() == {
            "alpha": {
                "description": "REST alpha",
                "methods": ["async_echo", "secret_values", "sync_echo"],
            }
        }

        describe_response = await client.get("/tools/alpha")
        assert describe_response.status_code == 200
        description = describe_response.json()
        assert description["tool"] == "alpha"
        assert description["description"] == "REST alpha"
        assert [method["name"] for method in description["methods"]] == [
            "async_echo",
            "secret_values",
            "sync_echo",
        ]

        call_response = await client.post(
            "/tools/alpha/sync_echo",
            json={"text": "from rest"},
        )
        assert call_response.status_code == 200
        assert call_response.json() == {
            "tool": "alpha",
            "method": "sync_echo",
            "result": {"mode": "sync", "text": "from rest", "source": "base"},
        }

        secret_response = await client.post("/tools/alpha/secret_values", json={})
        assert secret_response.status_code == 200
        assert secret_response.json() == {
            "tool": "alpha",
            "method": "secret_values",
            "result": {"required": "REQ_TOKEN", "optional": "OPT_TOKEN"},
        }

        missing_response = await client.post("/tools/alpha/missing", json={})
        assert missing_response.status_code == 200
        assert missing_response.json()["result"] == (
            '{"error": "Method \'missing\' not found in tool \'alpha\'", '
            '"available_methods": ["async_echo", "secret_values", "sync_echo"]}'
        )


class TestHarnessSecretSelection:
    """ToolManager picks the right harness credentials for a sandbox based on
    its engine and auth-mode env vars."""

    def _names(self, secrets: list) -> set[str]:
        return {getattr(s, "name", None) for s in secrets}

    def test_claude_code_api_key_includes_anthropic_excludes_openai(self) -> None:
        tm = ToolManager.__new__(ToolManager)
        tm.tools = {}
        names = self._names(
            tm.secrets_for_sandbox("claude-code", {"CLAUDE_CODE_AUTH_MODE": "api_key"})
        )
        assert "ANTHROPIC_API_KEY" in names
        assert "OPENAI_API_KEY" not in names
        assert "anthropic-claude" not in names

    def test_claude_code_access_token_swaps_to_brokered(self) -> None:
        tm = ToolManager.__new__(ToolManager)
        tm.tools = {}
        names = self._names(
            tm.secrets_for_sandbox(
                "claude-code", {"CLAUDE_CODE_AUTH_MODE": "access_token"}
            )
        )
        assert "anthropic-claude" in names
        assert "ANTHROPIC_API_KEY" not in names
        assert "OPENAI_API_KEY" not in names

    def test_codex_api_key_includes_openai_excludes_anthropic(self) -> None:
        tm = ToolManager.__new__(ToolManager)
        tm.tools = {}
        names = self._names(
            tm.secrets_for_sandbox("codex", {"CODEX_AUTH_MODE": "api_key"})
        )
        assert "OPENAI_API_KEY" in names
        assert "ANTHROPIC_API_KEY" not in names
        assert "openai-codex" not in names

    def test_codex_access_token_swaps_to_brokered_with_account_id(self) -> None:
        tm = ToolManager.__new__(ToolManager)
        tm.tools = {}
        names = self._names(
            tm.secrets_for_sandbox("codex", {"CODEX_AUTH_MODE": "access_token"})
        )
        assert "openai-codex" in names
        assert "OPENAI_CODEX_ACCOUNT_ID" in names
        assert "OPENAI_API_KEY" not in names

    def test_unset_auth_mode_defaults_to_api_key(self) -> None:
        tm = ToolManager.__new__(ToolManager)
        tm.tools = {}
        names = self._names(tm.secrets_for_sandbox("claude-code", {}))
        assert "ANTHROPIC_API_KEY" in names
        assert "anthropic-claude" not in names

    def test_unknown_engine_gets_no_harness_extras(self) -> None:
        tm = ToolManager.__new__(ToolManager)
        tm.tools = {}
        names = self._names(
            tm.secrets_for_sandbox("amp", {"CLAUDE_CODE_AUTH_MODE": "access_token"})
        )
        assert "ANTHROPIC_API_KEY" not in names
        assert "OPENAI_API_KEY" not in names
        # Base infra secrets (AMP_API_KEY, GITHUB_TOKEN, etc.) still present.
        assert "AMP_API_KEY" in names

    def test_slack_etl_token_is_global_proxy_secret(self) -> None:
        tm = ToolManager.__new__(ToolManager)
        tm.tools = {}
        secrets = tm.collect_secrets()
        slack_etl = next(s for s in secrets if getattr(s, "name", None) == "SLACK_ETL_TOKEN")
        assert slack_etl.secret_ref == "SLACK_ETL_TOKEN"
        assert slack_etl.hosts == ("*.slack.com",)
        assert slack_etl.match_headers == ("Authorization",)

    def test_collect_secrets_returns_union_of_all_harness_variants(self) -> None:
        """The shared API-side proxy and token broker need every harness
        credential so they can manage the full set regardless of which mode
        any individual sandbox is using right now."""
        tm = ToolManager.__new__(ToolManager)
        tm.tools = {}
        names = self._names(tm.collect_secrets())
        assert "ANTHROPIC_API_KEY" in names
        assert "OPENAI_API_KEY" in names
        assert "anthropic-claude" in names
        assert "openai-codex" in names
        assert "OPENAI_CODEX_ACCOUNT_ID" in names
