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


if __name__ == "__main__":
    unittest.main()
