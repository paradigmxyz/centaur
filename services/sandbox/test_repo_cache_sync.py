from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path

import repo_cache_sync


class RepoCacheSyncTest(unittest.TestCase):
    def test_repository_refs_parse_nonempty_entries(self) -> None:
        self.assertEqual(
            repo_cache_sync._repository_refs("acme/one=main bad acme/two=abc123"),
            {"acme/one": "main", "acme/two": "abc123"},
        )

    def test_write_ready_preserves_readiness_format(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)

            sync = repo_cache_sync.RepoCacheSync(
                cache_dir=root / "cache",
                repositories=["acme/centaur"],
                repository_refs={"acme/centaur": "main"},
                sync_interval_seconds=30,
                github_token_file=root / "missing-token",
            )

            sync.write_ready()

            lines = (root / "cache" / ".repo-cache-ready").read_text().splitlines()
            self.assertEqual(lines[0], "repositories=acme/centaur")
            self.assertEqual(lines[1], "repository_refs=acme/centaur=main")
            self.assertRegex(lines[2], r"^synced_at=\d{4}-\d{2}-\d{2}T")

    def test_check_ready_validates_fingerprint_and_repos(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            repo_path = root / "cache" / "acme" / "centaur" / ".git"
            repo_path.mkdir(parents=True)
            sync = repo_cache_sync.RepoCacheSync(
                cache_dir=root / "cache",
                repositories=["acme/centaur"],
                repository_refs={"acme/centaur": "main"},
                sync_interval_seconds=30,
                github_token_file=root / "missing-token",
            )
            sync.write_ready()

            self.assertEqual(sync.check_ready(), 0)
            (root / "cache" / ".repo-cache-ready").write_text(
                "repositories=wrong\nrepository_refs=acme/centaur=main\n"
            )
            self.assertEqual(sync.check_ready(), 1)

    def test_run_forever_restores_repo_cache_umask(self) -> None:
        class StopAfterUmask(repo_cache_sync.RepoCacheSync):
            def configure_git(self) -> None:
                raise RuntimeError("stop")

        old_umask = os.umask(0o077)
        try:
            sync = StopAfterUmask(
                cache_dir=Path("/tmp"),
                repositories=["acme/centaur"],
                repository_refs={},
                sync_interval_seconds=30,
                github_token_file=Path("/tmp/missing-token"),
            )
            with self.assertRaises(RuntimeError):
                sync.run_forever()
            current_umask = os.umask(old_umask)
            self.assertEqual(current_umask, 0o022)
        finally:
            os.umask(old_umask)


if __name__ == "__main__":
    unittest.main()
