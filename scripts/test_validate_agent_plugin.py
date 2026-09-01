#!/usr/bin/env python3
"""Tests for the cross-client Centaur plugin validator."""

from __future__ import annotations

import json
from pathlib import Path
import shutil
import tempfile
import unittest

from validate_agent_plugin import ROOT, validate


ARTIFACTS = (
    ".agents/plugins/marketplace.json",
    ".claude-plugin/marketplace.json",
    "plugins/centaur/.codex-plugin/plugin.json",
    "plugins/centaur/.claude-plugin/plugin.json",
    "plugins/centaur/skills/centaur/SKILL.md",
    "plugins/centaur/README.md",
)


class ValidateAgentPluginTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root = Path(self.temp_dir.name)
        for relative in ARTIFACTS:
            source = ROOT / relative
            target = self.root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)

    def tearDown(self) -> None:
        self.temp_dir.cleanup()

    def load_json(self, relative: str) -> dict:
        return json.loads((self.root / relative).read_text())

    def write_json(self, relative: str, value: dict) -> None:
        (self.root / relative).write_text(json.dumps(value, indent=2) + "\n")

    def test_repository_artifacts_are_valid(self) -> None:
        self.assertEqual(validate(self.root), [])

    def test_rejects_contract_regressions(self) -> None:
        cases = (
            (
                "mismatched version",
                "plugins/centaur/.claude-plugin/plugin.json",
                lambda value: value.update(version="9.9.9"),
                "versions must match",
            ),
            (
                "stdio transport",
                "plugins/centaur/.claude-plugin/plugin.json",
                lambda value: value["mcpServers"]["centaur"].update(type="stdio"),
                "transport must be http",
            ),
            (
                "hard-coded private host",
                "plugins/centaur/.claude-plugin/plugin.json",
                lambda value: value["mcpServers"]["centaur"].update(
                    url="https://private.example.ts.net/mcp"
                ),
                "must not contain private host suffix",
            ),
            (
                "wrong marketplace path",
                ".claude-plugin/marketplace.json",
                lambda value: value["plugins"][0].update(source="./centaur"),
                "source must be ./plugins/centaur",
            ),
            (
                "wrong Codex marketplace name",
                ".agents/plugins/marketplace.json",
                lambda value: value.update(name="main"),
                "marketplace name must be centaur",
            ),
            (
                "wrong Claude marketplace name",
                ".claude-plugin/marketplace.json",
                lambda value: value.update(name="main"),
                "marketplace name must be centaur",
            ),
        )
        for label, relative, mutate, expected in cases:
            with self.subTest(label=label):
                original = (self.root / relative).read_text()
                value = self.load_json(relative)
                mutate(value)
                self.write_json(relative, value)
                self.assertTrue(any(expected in error for error in validate(self.root)))
                (self.root / relative).write_text(original)


if __name__ == "__main__":
    unittest.main()
