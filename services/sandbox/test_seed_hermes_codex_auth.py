from __future__ import annotations

import base64
import json
import stat
import tempfile
import unittest
from pathlib import Path

from seed_hermes_codex_auth import seed


class SeedHermesCodexAuthTest(unittest.TestCase):
    def test_bundled_access_token_exposes_account_id_for_pi(self) -> None:
        auth = json.loads(Path(__file__).with_name("codex-auth.json").read_text())
        payload = auth["tokens"]["access_token"].split(".")[1]
        claims = json.loads(base64.urlsafe_b64decode(payload + "=" * (-len(payload) % 4)))
        self.assertEqual(
            claims["https://api.openai.com/auth"]["chatgpt_account_id"],
            auth["tokens"]["account_id"],
        )

    def test_seeds_placeholder_and_preserves_other_providers(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            codex = root / "codex.json"
            hermes = root / ".hermes" / "auth.json"
            pi = root / ".pi" / "agent" / "auth.json"
            codex.write_text(json.dumps({
                "tokens": {
                    "access_token": "dummy-access",
                    "refresh_token": "dummy-refresh",
                    "account_id": "dummy-account",
                },
                "last_refresh": "2025-01-01T00:00:00Z",
            }))
            hermes.parent.mkdir()
            hermes.write_text(json.dumps({"version": 1, "providers": {"other": {"api_key": "placeholder"}}}))

            seed(codex, hermes, pi)

            auth = json.loads(hermes.read_text())
            self.assertEqual(auth["providers"]["other"]["api_key"], "placeholder")
            self.assertEqual(auth["providers"]["openai-codex"]["tokens"], {
                "access_token": "dummy-access",
                "refresh_token": "dummy-refresh",
            })
            self.assertEqual(stat.S_IMODE(hermes.stat().st_mode), 0o600)
            pi_auth = json.loads(pi.read_text())["openai-codex"]
            self.assertEqual(pi_auth["access"], "dummy-access")
            self.assertEqual(pi_auth["accountId"], "dummy-account")
            self.assertGreater(pi_auth["expires"], 4_000_000_000_000)
            self.assertEqual(stat.S_IMODE(pi.stat().st_mode), 0o600)


if __name__ == "__main__":
    unittest.main()
