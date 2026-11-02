import json
import tempfile
import tomllib
import unittest
from pathlib import Path
from unittest.mock import patch

from configure_codex_models import configure, custom_models


class ConfigureCodexModelsTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.config = Path(self.temp.name) / "config.toml"
        self.config.write_text('model = "existing"\n')
        self.instructions = Path(self.temp.name) / "instructions.md"
        self.instructions.write_text("Native generic coding instructions")
        self.providers = {
            "custom": {
                "models": {
                    "custom-model": {
                        "displayName": "Custom Model",
                        "reasoningEfforts": ["low", "max"],
                        "defaultReasoning": "max",
                        "contextWindow": 1000000,
                        "inputModalities": ["text"],
                    }
                }
            }
        }

    @patch("configure_codex_models.subprocess.check_output")
    def test_preserves_bundle_and_generates_only_declared_capabilities(self, command):
        bundled = {
            "slug": "existing",
            "use_responses_lite": True,
            "context_window": 272000,
        }
        command.return_value = json.dumps({"models": [bundled]})
        configure(self.config, self.providers, instructions_path=self.instructions)
        config = tomllib.loads(self.config.read_text())
        models = json.loads(Path(config["model_catalog_json"]).read_text())["models"]
        self.assertEqual(models[0], bundled)
        self.assertEqual(
            models[1]["base_instructions"], "Native generic coding instructions"
        )
        self.assertEqual(models[1]["context_window"], 1000000)
        self.assertEqual(models[1]["max_context_window"], 1000000)
        self.assertEqual(models[1]["input_modalities"], ["text"])
        self.assertEqual(models[1]["default_reasoning_level"], "max")
        self.assertEqual(models[1]["experimental_supported_tools"], [])
        self.assertEqual(models[1]["service_tiers"], [])
        self.assertFalse(models[1]["use_responses_lite"])
        self.assertEqual(config["model"], "existing")
        command.assert_called_once_with(
            ["codex", "debug", "models", "--bundled"], text=True, timeout=30
        )

    @patch("configure_codex_models.subprocess.check_output")
    def test_operator_catalog_takes_precedence(self, command):
        contents = 'model_catalog_json = "/operator/models.json"\n'
        self.config.write_text(contents)
        configure(self.config, self.providers, instructions_path=self.instructions)
        self.assertEqual(self.config.read_text(), contents)
        command.assert_not_called()

    @patch("configure_codex_models.subprocess.check_output")
    def test_profile_override_does_not_disable_default_catalog(self, command):
        command.return_value = json.dumps({"models": []})
        self.config.write_text(
            '[profiles.custom]\nmodel_catalog_json = "/operator/models.json"\n'
        )
        configure(self.config, self.providers, instructions_path=self.instructions)
        config = tomllib.loads(self.config.read_text())
        self.assertEqual(
            config["profiles"]["custom"]["model_catalog_json"], "/operator/models.json"
        )
        self.assertEqual(
            config["model_catalog_json"],
            str(self.config.with_name("centaur-models.json")),
        )

    def test_shared_model_ids_require_identical_native_metadata(self):
        self.providers["second"] = self.providers["custom"]
        self.assertEqual(len(custom_models(self.providers)), 1)
        self.providers["second"] = {
            "models": {"custom-model": {"inputModalities": ["text"]}}
        }
        with self.assertRaisesRegex(ValueError, "conflicting native metadata"):
            custom_models(self.providers)

    @patch("configure_codex_models.subprocess.check_output")
    def test_cannot_replace_bundled_model(self, command):
        command.return_value = json.dumps({"models": [{"slug": "custom-model"}]})
        with self.assertRaisesRegex(ValueError, "conflict with bundled"):
            configure(self.config, self.providers, instructions_path=self.instructions)
        self.assertEqual(self.config.read_text(), 'model = "existing"\n')

    @patch("configure_codex_models.subprocess.check_output")
    def test_providers_without_model_metadata_do_not_change_config(self, command):
        configure(self.config, {"legacy": {"name": "Legacy"}})
        self.assertEqual(self.config.read_text(), 'model = "existing"\n')
        command.assert_not_called()

    def test_invalid_capabilities_rejected(self):
        for metadata in [
            {"contextWindow": True},
            {"contextWindow": -1},
            {"inputModalities": ["audio"]},
        ]:
            with self.assertRaises(ValueError):
                custom_models({"p": {"models": {"m": metadata}}})


if __name__ == "__main__":
    unittest.main()
