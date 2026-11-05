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


def _compose_stub(content: str, calls: list[Path]):
    def compose(home_dir: Path, repo_mount: Path, target: Path) -> int:
        calls.append(target)
        target.write_text(content)
        return 0

    return compose


class PromptRecomposeTest(unittest.TestCase):
    def test_prompt_fingerprint_tracks_input_changes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            home_dir = root / "home"
            home_dir.mkdir()
            (home_dir / "AGENTS.md").write_text("baked\n")
            repo_mount = root / "github"
            overlay = repo_mount / "acme" / "overlay" / "services" / "sandbox"
            overlay.mkdir(parents=True)
            overlay_prompt = overlay / "SYSTEM_PROMPT.md"
            overlay_prompt.write_text("overlay v1\n")

            fingerprint = repo_cache_watch._prompt_fingerprint(
                home_dir, repo_mount, observability_enabled=True
            )
            self.assertIsNotNone(fingerprint)

            overlay_prompt.write_text("overlay v2\n")
            self.assertNotEqual(
                repo_cache_watch._prompt_fingerprint(
                    home_dir, repo_mount, observability_enabled=True
                ),
                fingerprint,
            )

            same_content_fingerprint = repo_cache_watch._prompt_fingerprint(
                home_dir, repo_mount, observability_enabled=True
            )
            self.assertNotEqual(
                repo_cache_watch._prompt_fingerprint(
                    home_dir, repo_mount, observability_enabled=False
                ),
                same_content_fingerprint,
            )

    def test_prompt_fingerprint_absent_without_inputs(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.assertIsNone(
                repo_cache_watch._prompt_fingerprint(
                    root / "home", root / "github", observability_enabled=True
                )
            )

    def test_recompose_prompt_replaces_only_on_content_change(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            home_dir = root / "home"
            repo_mount = root / "github"
            home_dir.mkdir()
            repo_mount.mkdir()
            target = root / "workspace" / "AGENTS.md"
            calls: list[Path] = []
            compose = _compose_stub("prompt v1\n", calls)

            with contextlib.redirect_stderr(io.StringIO()):
                recomposed = repo_cache_watch._recompose_prompt(
                    home_dir, repo_mount, target, compose
                )
            self.assertTrue(recomposed)
            self.assertEqual(target.read_text(), "prompt v1\n")
            first_inode = target.stat().st_ino

            with contextlib.redirect_stderr(io.StringIO()):
                recomposed = repo_cache_watch._recompose_prompt(
                    home_dir, repo_mount, target, compose
                )
            self.assertTrue(recomposed)
            self.assertEqual(target.stat().st_ino, first_inode)

            compose_v2 = _compose_stub("prompt v2\n", calls)
            with contextlib.redirect_stderr(io.StringIO()):
                recomposed = repo_cache_watch._recompose_prompt(
                    home_dir, repo_mount, target, compose_v2
                )
            self.assertTrue(recomposed)
            self.assertEqual(target.read_text(), "prompt v2\n")
            self.assertNotEqual(target.stat().st_ino, first_inode)
            self.assertEqual(len(calls), 3)
            self.assertTrue(
                all(call.parent == target.parent for call in calls),
                "compose must run against the target directory for atomic replace",
            )

    def test_recompose_prompt_reports_compose_failure(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            target = root / "workspace" / "AGENTS.md"

            def failing_compose(home_dir: Path, repo_mount: Path, target: Path) -> int:
                return 1

            with contextlib.redirect_stderr(io.StringIO()):
                recomposed = repo_cache_watch._recompose_prompt(
                    root / "home", root / "github", target, failing_compose
                )
            self.assertFalse(recomposed)
            self.assertFalse(target.exists())
            self.assertEqual(list(root.joinpath("workspace").iterdir()), [])

    def test_recompose_prompt_keeps_target_on_empty_compose_output(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            target = root / "workspace" / "AGENTS.md"
            target.parent.mkdir(parents=True)
            target.write_text("previously composed\n")
            calls: list[Path] = []

            with contextlib.redirect_stderr(io.StringIO()):
                recomposed = repo_cache_watch._recompose_prompt(
                    root / "home",
                    root / "github",
                    target,
                    _compose_stub("", calls),
                )
            self.assertTrue(recomposed)
            self.assertEqual(target.read_text(), "previously composed\n")

    def test_recompose_prompt_survives_replace_oserror(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            target = root / "workspace" / "AGENTS.md"
            # A directory at the target path makes os.replace raise
            # IsADirectoryError (an OSError subclass).
            target.mkdir(parents=True)
            calls: list[Path] = []

            with contextlib.redirect_stderr(io.StringIO()):
                recomposed = repo_cache_watch._recompose_prompt(
                    root / "home",
                    root / "github",
                    target,
                    _compose_stub("prompt v1\n", calls),
                )
            self.assertFalse(recomposed)
            self.assertTrue(target.is_dir())
            leftovers = [p for p in root.joinpath("workspace").iterdir()]
            self.assertEqual(
                leftovers,
                [target],
                "failed recomposition must clean up its temp file",
            )

    def test_recompose_if_changed_advances_and_skips_when_unchanged(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            home_dir = root / "home"
            repo_mount = root / "github"
            home_dir.mkdir()
            repo_mount.mkdir()
            (home_dir / "AGENTS.md").write_text("baked\n")
            target = root / "workspace" / "AGENTS.md"
            calls: list[Path] = []

            with contextlib.redirect_stderr(io.StringIO()):
                applied, recomposed = repo_cache_watch._recompose_if_changed(
                    home_dir,
                    repo_mount,
                    target,
                    None,
                    observability_enabled=True,
                    compose=_compose_stub("prompt v1\n", calls),
                )
            self.assertTrue(recomposed)
            self.assertIsNotNone(applied)

            with contextlib.redirect_stderr(io.StringIO()):
                applied, recomposed = repo_cache_watch._recompose_if_changed(
                    home_dir,
                    repo_mount,
                    target,
                    applied,
                    observability_enabled=True,
                    compose=_compose_stub("prompt v1\n", calls),
                )
            self.assertFalse(recomposed)
            self.assertEqual(len(calls), 1)

            (home_dir / "AGENTS.md").write_text("baked v2\n")
            with contextlib.redirect_stderr(io.StringIO()):
                applied, recomposed = repo_cache_watch._recompose_if_changed(
                    home_dir,
                    repo_mount,
                    target,
                    applied,
                    observability_enabled=True,
                    compose=_compose_stub("prompt v2\n", calls),
                )
            self.assertTrue(recomposed)
            self.assertEqual(len(calls), 2)
            self.assertEqual(target.read_text(), "prompt v2\n")

    def test_recompose_if_changed_retries_after_failure(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            home_dir = root / "home"
            repo_mount = root / "github"
            home_dir.mkdir()
            repo_mount.mkdir()
            (home_dir / "AGENTS.md").write_text("baked\n")
            target = root / "workspace" / "AGENTS.md"
            calls: list[Path] = []

            with contextlib.redirect_stderr(io.StringIO()):
                applied, recomposed = repo_cache_watch._recompose_if_changed(
                    home_dir,
                    repo_mount,
                    target,
                    None,
                    observability_enabled=True,
                    compose=lambda home_dir, repo_mount, target: 1,
                )
            self.assertFalse(recomposed)
            self.assertIsNone(applied)
            self.assertFalse(target.exists())

            with contextlib.redirect_stderr(io.StringIO()):
                applied, recomposed = repo_cache_watch._recompose_if_changed(
                    home_dir,
                    repo_mount,
                    target,
                    applied,
                    observability_enabled=True,
                    compose=_compose_stub("prompt v1\n", calls),
                )
            self.assertTrue(recomposed)
            self.assertIsNotNone(applied)
            self.assertEqual(target.read_text(), "prompt v1\n")

    def test_recompose_if_changed_skips_without_inputs(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            calls: list[Path] = []
            with contextlib.redirect_stderr(io.StringIO()):
                applied, recomposed = repo_cache_watch._recompose_if_changed(
                    root / "home",
                    root / "github",
                    root / "workspace" / "AGENTS.md",
                    None,
                    observability_enabled=True,
                    compose=_compose_stub("unused\n", calls),
                )
            self.assertFalse(recomposed)
            self.assertIsNone(applied)
            self.assertEqual(calls, [])


if __name__ == "__main__":
    unittest.main()
