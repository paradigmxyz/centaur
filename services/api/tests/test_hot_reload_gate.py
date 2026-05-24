"""Unit tests for the PLUGIN_WATCHER_ENABLED env gate."""

from __future__ import annotations

import asyncio
import os
import sys
from pathlib import Path
from unittest.mock import MagicMock

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

os.environ.setdefault("DATABASE_URL", "postgres://centaur:test@localhost:5432/test")

from api.app import _plugin_watcher_enabled, _watch_tools, _watch_workflows  # noqa: E402


@pytest.mark.asyncio
async def test_watch_tools_returns_immediately_when_disabled(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("PLUGIN_WATCHER_ENABLED", "0")

    pm = MagicMock()
    pm.tools_dirs = [Path("/nonexistent")]

    await asyncio.wait_for(_watch_tools(pm), timeout=0.5)

    pm.reload.assert_not_called()


@pytest.mark.asyncio
async def test_watch_workflows_returns_immediately_when_disabled(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("PLUGIN_WATCHER_ENABLED", "0")

    await asyncio.wait_for(_watch_workflows(), timeout=0.5)


@pytest.mark.parametrize(
    "value,expected",
    [
        ("0", False),
        ("false", False),
        ("no", False),
        ("FALSE", False),
        ("1", True),
        ("true", True),
        ("yes", True),
        ("", True),
    ],
)
def test_plugin_watcher_enabled_parses_truthy_values(
    monkeypatch: pytest.MonkeyPatch, value: str, expected: bool
) -> None:
    monkeypatch.setenv("PLUGIN_WATCHER_ENABLED", value)

    assert _plugin_watcher_enabled() is expected


def test_plugin_watcher_enabled_defaults_to_true(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("PLUGIN_WATCHER_ENABLED", raising=False)

    assert _plugin_watcher_enabled() is True
