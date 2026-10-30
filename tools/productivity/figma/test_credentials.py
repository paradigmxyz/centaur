"""Credential contract tests for the Figma tool.

Run from this directory:
    uv run --no-project --python 3.11 --with pytest pytest test_credentials.py
"""

from __future__ import annotations

import importlib.util
import json
import sys
import tomllib
from pathlib import Path
from typing import Any

import pytest

TOOL_DIR = Path(__file__).parent
REPO_ROOT = TOOL_DIR.parents[2]
sys.path.insert(0, str(REPO_ROOT))


def _load_client_module():
    client_path = TOOL_DIR / "client.py"
    spec = importlib.util.spec_from_file_location("test_figma_client_module", client_path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def test_manifest_limits_token_injection_to_figma_api() -> None:
    metadata = tomllib.loads((TOOL_DIR / "pyproject.toml").read_text())
    secrets = metadata["tool"]["centaur"]["secrets"]

    assert secrets == [
        {
            "type": "http",
            "name": "FIGMA_ACCESS_TOKEN",
            "mode": "inject",
            "inject_header": "X-Figma-Token",
            "hosts": ["api.figma.com"],
        }
    ]


def test_example_environment_uses_declared_secret_name() -> None:
    example = (TOOL_DIR / ".env.example").read_text().split("=", maxsplit=1)

    assert example[0] == "FIGMA_ACCESS_TOKEN"


def test_client_resolves_only_the_declared_secret(monkeypatch) -> None:
    client_module = _load_client_module()
    requested_secrets: list[tuple[str, str]] = []

    def fake_secret(name: str, default: str) -> str:
        requested_secrets.append((name, default))
        return "placeholder-token"

    monkeypatch.setattr(client_module, "secret", fake_secret)

    client = client_module.FigmaClient()

    assert client.token == "placeholder-token"
    assert requested_secrets == [("FIGMA_ACCESS_TOKEN", "")]


def test_client_does_not_resolve_undeclared_legacy_secret(monkeypatch) -> None:
    client_module = _load_client_module()
    requested_secrets: list[tuple[str, str]] = []

    def fake_secret(name: str, default: str) -> str:
        requested_secrets.append((name, default))
        return "legacy-token" if name == "FIGMA" else default

    monkeypatch.setattr(client_module, "secret", fake_secret)

    with pytest.raises(ValueError, match="Set FIGMA_ACCESS_TOKEN or pass token"):
        client_module.FigmaClient()

    assert requested_secrets == [("FIGMA_ACCESS_TOKEN", "")]


def test_client_sends_token_only_to_figma_api(monkeypatch) -> None:
    client_module = _load_client_module()
    captured_request: dict[str, Any] = {}

    class FakeResponse:
        def __enter__(self):
            return self

        def __exit__(self, *_args: object) -> None:
            return None

        def read(self) -> bytes:
            return json.dumps({"id": "test-user"}).encode()

    def fake_urlopen(request, timeout: int):
        captured_request["url"] = request.full_url
        captured_request["token"] = request.get_header("X-figma-token")
        captured_request["timeout"] = timeout
        return FakeResponse()

    monkeypatch.setattr(client_module, "urlopen", fake_urlopen)

    result = client_module.FigmaClient(token="placeholder-token")._request("/me")

    assert result == {"id": "test-user"}
    assert captured_request == {
        "url": "https://api.figma.com/v1/me",
        "token": "placeholder-token",
        "timeout": 60,
    }
