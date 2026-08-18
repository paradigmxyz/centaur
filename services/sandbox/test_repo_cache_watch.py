from __future__ import annotations

import contextlib
import io
import json
import subprocess
import tempfile
import unittest
from pathlib import Path

import repo_cache_watch


def _write_metadata(tool_dir: Path, repo_path: Path) -> None:
    tool_dir.mkdir(parents=True, exist_ok=True)
    (tool_dir / repo_cache_watch.TOOLS_METADATA_NAME).write_text(
        json.dumps(
            {
                "sources": [
                    {
                        "repo": "acme/centaur",
                        "source": "repo_cache",
                        "source_subdir": "tools",
                        "repo_cache_repo_path": str(repo_path),
                    }
                ]
            }
        )
    )


def _init_repo(repo_path: Path) -> None:
    repo_path.mkdir(parents=True)
    subprocess.run(["git", "init", "-q", "-b", "test-branch", str(repo_path)], check=True)
    subprocess.run(
        ["git", "-C", str(repo_path), "config", "user.email", "test@example.com"],
        check=True,
    )
    subprocess.run(
        ["git", "-C", str(repo_path), "config", "user.name", "Test"],
        check=True,
    )


def _commit(repo_path: Path, content: str) -> str:
    (repo_path / "tools").mkdir(exist_ok=True)
    (repo_path / "tools" / "example.txt").write_text(content)
    subprocess.run(["git", "-C", str(repo_path), "add", "tools"], check=True)
    subprocess.run(
        ["git", "-C", str(repo_path), "commit", "-q", "-m", "update"],
        check=True,
    )
    return subprocess.check_output(
        ["git", "-C", str(repo_path), "rev-parse", "HEAD"],
        text=True,
    ).strip()


class RepoCacheWatchTest(unittest.TestCase):
    def test_fingerprint_uses_repo_cache_commit(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            tool_dir = root / "tools"
            repo_path = root / "cache" / "acme" / "centaur"
            _init_repo(repo_path)
            commit = _commit(repo_path, "hello\n")
            _write_metadata(tool_dir, repo_path)

            entries = json.loads(repo_cache_watch._repo_cache_fingerprint([tool_dir]))
            self.assertEqual(
                entries,
                [
                    {
                        "commit": commit,
                        "repo": "acme/centaur",
                        "repo_cache_repo_path": str(repo_path),
                    }
                ],
            )

            fingerprint = repo_cache_watch._repo_cache_fingerprint([tool_dir])
            _commit(repo_path, "goodbye\n")
            self.assertNotEqual(
                repo_cache_watch._repo_cache_fingerprint([tool_dir]),
                fingerprint,
            )

    def test_refresh_if_changed_calls_refresh_and_advances_on_success(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            tool_dir = root / "tools"
            repo_path = root / "cache" / "acme" / "centaur"
            _init_repo(repo_path)
            _commit(repo_path, "hello\n")
            _write_metadata(tool_dir, repo_path)
            calls = 0

            def refresh() -> int:
                nonlocal calls
                calls += 1
                return 0

            with contextlib.redirect_stderr(io.StringIO()):
                applied, refreshed = repo_cache_watch._refresh_if_changed(
                    [tool_dir], None, refresh
                )
            self.assertTrue(refreshed)
            self.assertEqual(calls, 1)

            with contextlib.redirect_stderr(io.StringIO()):
                applied, refreshed = repo_cache_watch._refresh_if_changed(
                    [tool_dir], applied, refresh
                )
            self.assertFalse(refreshed)
            self.assertEqual(calls, 1)

            _commit(repo_path, "goodbye\n")
            with contextlib.redirect_stderr(io.StringIO()):
                applied, refreshed = repo_cache_watch._refresh_if_changed(
                    [tool_dir], applied, refresh
                )
            self.assertTrue(refreshed)
            self.assertEqual(calls, 2)
            self.assertEqual(
                applied,
                repo_cache_watch._repo_cache_fingerprint([tool_dir]),
            )

    def test_refresh_if_changed_retries_after_failure(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            tool_dir = root / "tools"
            repo_path = root / "cache" / "acme" / "centaur"
            _init_repo(repo_path)
            _commit(repo_path, "hello\n")
            _write_metadata(tool_dir, repo_path)

            with contextlib.redirect_stderr(io.StringIO()):
                applied, refreshed = repo_cache_watch._refresh_if_changed(
                    [tool_dir], None, lambda: 1
                )

            self.assertFalse(refreshed)
            self.assertIsNone(applied)


if __name__ == "__main__":
    unittest.main()
