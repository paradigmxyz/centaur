from __future__ import annotations

import json
import stat
import tempfile
import unittest
from pathlib import Path

from seed_hermes_codex_auth import seed


class SeedHermesCodexAuthTest(unittest.TestCase):
    def test_seeds_placeholder_and_preserves_other_providers(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            codex = root / "codex.json"
            hermes = root / ".hermes" / "auth.json"
            codex.write_text(json.dumps({
                "tokens": {"access_token": "dummy-access", "refresh_token": "dummy-refresh"},
                "last_refresh": "2025-01-01T00:00:00Z",
            }))
            hermes.parent.mkdir()
            hermes.write_text(json.dumps({"version": 1, "providers": {"other": {"api_key": "placeholder"}}}))

            seed(codex, hermes)

            auth = json.loads(hermes.read_text())
            self.assertEqual(auth["providers"]["other"]["api_key"], "placeholder")
            self.assertEqual(auth["providers"]["openai-codex"]["tokens"], {
                "access_token": "dummy-access",
                "refresh_token": "dummy-refresh",
            })
            self.assertEqual(stat.S_IMODE(hermes.stat().st_mode), 0o600)


if __name__ == "__main__":
    unittest.main()
