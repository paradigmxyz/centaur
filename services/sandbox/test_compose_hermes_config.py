from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

import yaml

from compose_hermes_config import compose_hermes_config


class ComposeHermesConfigTest(unittest.TestCase):
    def test_adds_skills_dir_and_applies_overlay(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp) / ".hermes"
            skills = Path(tmp) / "workspace" / ".agents" / "skills"

            path = compose_hermes_config(
                hermes_home=home,
                skills_dir=skills,
                overlay_raw='{"model": {"default": "claude-sonnet-5"}}',
            )

            config = yaml.safe_load(path.read_text())
            self.assertEqual(config["skills"]["external_dirs"], [str(skills)])
            self.assertEqual(config["model"]["default"], "claude-sonnet-5")

    def test_preserves_existing_config_and_is_rerunnable(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp) / ".hermes"
            home.mkdir()
            skills = Path(tmp) / "skills"
            (home / "config.yaml").write_text(
                yaml.safe_dump(
                    {
                        "model": {"default": "kept", "provider": "anthropic"},
                        "skills": {"external_dirs": ["/opt/other-skills"]},
                        "toolsets": ["hermes-cli"],
                    }
                )
            )

            for _ in range(2):
                path = compose_hermes_config(
                    hermes_home=home,
                    skills_dir=skills,
                    overlay_raw="model:\n  default: overridden\n",
                )

            config = yaml.safe_load(path.read_text())
            self.assertEqual(
                config["skills"]["external_dirs"], ["/opt/other-skills", str(skills)]
            )
            # The overlay wins over the existing value; siblings survive the merge.
            self.assertEqual(config["model"], {"default": "overridden", "provider": "anthropic"})
            self.assertEqual(config["toolsets"], ["hermes-cli"])

    def test_invalid_overlay_is_ignored(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp) / ".hermes"

            path = compose_hermes_config(
                hermes_home=home,
                skills_dir=Path(tmp) / "skills",
                overlay_raw="[not, a, mapping]",
            )

            config = yaml.safe_load(path.read_text())
            self.assertEqual(list(config), ["skills"])

    def test_no_skills_dir_leaves_skills_untouched(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp) / ".hermes"

            path = compose_hermes_config(hermes_home=home, overlay_raw=None)

            self.assertEqual(yaml.safe_load(path.read_text()), {})


if __name__ == "__main__":
    unittest.main()
