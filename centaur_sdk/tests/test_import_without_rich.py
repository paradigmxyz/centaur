"""Regression: the SDK must import in the isolated tool runner, which has no `rich`.

Centaur's generated tool runner imports `centaur_sdk.tool_sdk` with PYTHONPATH
pointing at /opt/centaur, inside a uvx env built from the tool's own (often
stdlib-only) dependencies. Importing the package must not pull in `rich`.
"""

from __future__ import annotations

import subprocess
import sys

_SNIPPET = """
import importlib.abc, importlib.machinery, sys


class _BlockRich(importlib.abc.MetaPathFinder):
    def find_spec(self, name, path, target=None):
        if name == "rich" or name.startswith("rich."):
            raise ModuleNotFoundError("No module named 'rich'")
        return None


sys.meta_path.insert(0, _BlockRich())

# Importing the package and the tool SDK must work with rich absent.
import centaur_sdk
from centaur_sdk.tool_sdk import secret  # noqa: F401

# render_text_table is pure stdlib and must stay reachable.
assert centaur_sdk.render_text_table(["a"], [["1"]])

# Table is rich-backed and must only fail when actually accessed.
try:
    centaur_sdk.Table
except ModuleNotFoundError:
    pass
else:
    raise AssertionError("expected centaur_sdk.Table to require rich")

print("ok")
"""


def test_tool_sdk_imports_without_rich():
    result = subprocess.run(
        [sys.executable, "-c", _SNIPPET],
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "ok"
