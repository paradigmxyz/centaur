"""Tests for the Linear CLI's label listing.

Run from this directory:
    uv run --no-project --with pytest --with typer --with rich pytest test_cli.py
"""

from __future__ import annotations

import importlib.util
import sys
import types
from pathlib import Path
from typing import Any

# cli.py imports Table from the packaged SDK and reaches into .client only at
# call time. Stub the SDK so the module loads as a standalone file; the recorded
# rows let us assert on what the command would render.
RECORDED_ROWS: list[tuple[str, ...]] = []

if "centaur_sdk" not in sys.modules:
    sdk_mod = types.ModuleType("centaur_sdk")

    class Table:
        def __init__(self, *args: Any, **kwargs: Any) -> None:
            pass

        def add_column(self, *args: Any, **kwargs: Any) -> None:
            pass

        def add_row(self, *cells: str) -> None:
            RECORDED_ROWS.append(cells)

    sdk_mod.Table = Table
    sys.modules["centaur_sdk"] = sdk_mod

spec = importlib.util.spec_from_file_location(
    "linear_cli", Path(__file__).with_name("cli.py")
)
assert spec and spec.loader
cli = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cli)


class FakeClient:
    def __init__(self, labels: list[dict[str, Any]]) -> None:
        self._labels = labels

    def labels(self, team_key: str | None = None) -> list[dict[str, Any]]:
        return self._labels


def _run_labels(monkeypatch, labels: list[dict[str, Any]]):
    from typer.testing import CliRunner

    RECORDED_ROWS.clear()
    monkeypatch.setattr(cli, "get_client", lambda: FakeClient(labels))
    return CliRunner().invoke(cli.app, ["labels"])


def test_labels_renders_org_wide_label_without_crashing(monkeypatch):
    # An org-wide label arrives with team explicitly None (PE-7945 repro).
    result = _run_labels(
        monkeypatch,
        [
            {"name": "team-bug", "team": {"key": "PE"}},
            {"name": "org-wide", "team": None},
        ],
    )

    assert result.exit_code == 0, result.output
    assert ("org", "org-wide") in RECORDED_ROWS
    assert ("PE", "team-bug") in RECORDED_ROWS


def test_labels_handles_missing_team_key(monkeypatch):
    # Defensive: a label with no team key at all must also not crash.
    result = _run_labels(monkeypatch, [{"name": "loose"}])

    assert result.exit_code == 0, result.output
    assert ("org", "loose") in RECORDED_ROWS
