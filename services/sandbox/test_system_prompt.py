from __future__ import annotations

import unittest
from pathlib import Path


SYSTEM_PROMPT = Path(__file__).with_name("SYSTEM_PROMPT.md")


class SystemPromptTest(unittest.TestCase):
    def test_mpp_fallback_discovery_guidance_is_present(self) -> None:
        prompt = SYSTEM_PROMPT.read_text()

        self.assertIn("[MPP fallback discovery]", prompt)
        self.assertIn("centaur-tools list", prompt)
        self.assertIn('mpp services search "<sanitized task capability>" --limit 5', prompt)
        self.assertIn("mpp services show <service-id>", prompt)
        self.assertIn("Current MPP support discovers candidates only", prompt)

    def test_runtime_discovery_and_vlogs_examples_match_available_surfaces(self) -> None:
        prompt = SYSTEM_PROMPT.read_text()

        self.assertNotIn("[Active deployment]", prompt)
        self.assertIn("$CENTAUR_HARNESS_TYPE", prompt)
        self.assertIn("centaur-tools call vlogs thread_logs", prompt)
        self.assertIn("centaur-tools call vlogs thread_trace", prompt)
        self.assertNotIn("|  vlogs thread_logs", prompt)
        self.assertNotIn("|  vlogs thread_trace", prompt)

    def test_model_and_harness_switching_answer_guidance_is_present(self) -> None:
        prompt = SYSTEM_PROMPT.read_text()

        self.assertIn("[Model and Harness Switching Answers]", prompt)
        self.assertIn("`--codex`, `--claude` or `--claude-code`, and `--amp`", prompt)
        self.assertIn("`--model <model-id-or-alias>`", prompt)
        self.assertIn("`--model=<model-id-or-alias>`", prompt)
        self.assertIn("`--fable`, `--opus`, `--sonnet`, and `--haiku`", prompt)
        self.assertIn("`--claude --model=fable fix this`", prompt)
        self.assertIn("`--codex --model=gpt-5.2 investigate this`", prompt)
        self.assertIn("`--meta` selects Codex with the Meta provider", prompt)
        self.assertIn("`--bedrock` selects Codex with the Bedrock provider", prompt)
        self.assertIn("`-rsn <effort>` sets Codex reasoning effort", prompt)

if __name__ == "__main__":
    unittest.main()
