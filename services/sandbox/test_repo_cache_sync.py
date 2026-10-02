from __future__ import annotations

import os
import subprocess
import tempfile
import threading
import unittest
from pathlib import Path
from unittest import mock

import repo_cache_sync


class RepoCacheSyncTest(unittest.TestCase):
    def make_sync(self, root: Path, **kwargs: float) -> repo_cache_sync.RepoCacheSync:
        return repo_cache_sync.RepoCacheSync(
            cache_dir=root / "cache",
            repositories=["acme/centaur"],
            repository_refs={},
            repository_visibilities={},
            sync_interval_seconds=30,
            github_token_file=root / "missing-token",
            **kwargs,
        )

    def test_from_env_rejects_invalid_git_timeouts(self) -> None:
        for name, value in (
            ("GIT_COMMAND_TIMEOUT_SECONDS", "0"),
            ("GIT_COMMAND_TIMEOUT_SECONDS", "nan"),
            ("GIT_SHUTDOWN_GRACE_SECONDS", "bad"),
        ):
            with (
                self.subTest(name=name, value=value),
                mock.patch.dict(os.environ, {name: value}),
                self.assertRaisesRegex(ValueError, name),
            ):
                repo_cache_sync.RepoCacheSync.from_env()

    def test_git_timeout_stops_child_and_reports_failure(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            fake_git = root / "git"
            fake_git.write_text("#!/bin/sh\ntrap '' TERM\nexec sleep 60\n")
            fake_git.chmod(0o755)
            sync = self.make_sync(
                root, git_command_timeout_seconds=0.2, git_shutdown_grace_seconds=0.2
            )
            sync.git_env = {**os.environ, "PATH": f"{root}:{os.environ['PATH']}"}

            with self.assertRaisesRegex(
                repo_cache_sync.GitCommandTimeout, "fetch acme/centaur timed out"
            ):
                sync._run_git(["fetch"], "fetch acme/centaur")

    def test_shutdown_stops_running_git(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            fake_git = root / "git"
            fake_git.write_text(
                '#!/bin/sh\ntouch "$REPO_CACHE_TEST_STARTED"\nexec sleep 60\n'
            )
            fake_git.chmod(0o755)
            started = root / "started"
            sync = self.make_sync(root, git_command_timeout_seconds=10)
            sync.git_env = {
                **os.environ,
                "PATH": f"{root}:{os.environ['PATH']}",
                "REPO_CACHE_TEST_STARTED": str(started),
            }

            def stop_after_start() -> None:
                while not started.exists():
                    repo_cache_sync.time.sleep(0.01)
                sync.request_shutdown(15, None)

            stopper = threading.Thread(target=stop_after_start, daemon=True)
            stopper.start()
            try:
                with self.assertRaises(repo_cache_sync.ShutdownRequested):
                    sync._run_git(["fetch"], "fetch")
            finally:
                stopper.join(timeout=2)
            self.assertFalse(stopper.is_alive())

    def test_recovers_only_regular_git_locks(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            target = root / "cache" / "private" / "acme" / "centaur"
            git_dir = target / ".git"
            git_dir.mkdir(parents=True)
            stale = git_dir / "index.lock"
            stale.write_text("stale")
            sync = self.make_sync(root)
            sync.recover_stale_git_locks(target)
            self.assertFalse(stale.exists())

            stale.symlink_to(root / "do-not-delete")
            with self.assertRaisesRegex(RuntimeError, "non-regular Git lock"):
                sync.recover_stale_git_locks(target)

    def test_second_writer_does_not_remove_readiness(self) -> None:
        import fcntl

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            sync = self.make_sync(root)
            sync.cache_dir.mkdir()
            sync.ready_file.write_text("existing")
            with (sync.cache_dir / ".repo-cache-sync.lock").open("a+") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                self.assertEqual(sync.run_forever(), 1)
            self.assertEqual(sync.ready_file.read_text(), "existing")

    def test_interrupted_sync_removes_readiness(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            sync = self.make_sync(root)
            sync.ready_file.parent.mkdir()
            sync.ready_file.write_text("previous")
            with (
                mock.patch.object(
                    sync, "sync_repo", side_effect=repo_cache_sync.ShutdownRequested
                ),
                self.assertRaises(repo_cache_sync.ShutdownRequested),
            ):
                sync.sync_once()
            self.assertFalse(sync.ready_file.exists())

    def test_repository_refs_parse_nonempty_entries(self) -> None:
        self.assertEqual(
            repo_cache_sync._repository_refs("acme/one=main bad acme/two=abc123"),
            {"acme/one": "main", "acme/two": "abc123"},
        )

    def test_repository_visibilities_default_invalid_values_to_private(self) -> None:
        self.assertEqual(
            repo_cache_sync._repository_visibilities(
                "acme/public=public acme/private=private acme/typo=internal",
                ["acme/public", "acme/private", "acme/missing", "acme/typo"],
            ),
            {
                "acme/public": "public",
                "acme/private": "private",
                "acme/missing": "private",
                "acme/typo": "private",
            },
        )

    def test_from_env_loads_repository_visibilities(self) -> None:
        old_env = os.environ.copy()
        try:
            os.environ.update(
                {
                    "REPOSITORIES": "acme/public acme/private",
                    "REPOSITORY_VISIBILITIES": "acme/public=public acme/private=bogus",
                    "SYNC_INTERVAL_SECONDS": "10",
                    "GIT_COMMAND_TIMEOUT_SECONDS": "420",
                    "GIT_SHUTDOWN_GRACE_SECONDS": "25",
                }
            )

            sync = repo_cache_sync.RepoCacheSync.from_env()

            self.assertEqual(
                sync.repository_visibilities,
                {"acme/public": "public", "acme/private": "private"},
            )
            self.assertEqual(sync.git_command_timeout_seconds, 420)
            self.assertEqual(sync.git_shutdown_grace_seconds, 25)
        finally:
            os.environ.clear()
            os.environ.update(old_env)

    def test_write_ready_preserves_readiness_format(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)

            sync = repo_cache_sync.RepoCacheSync(
                cache_dir=root / "cache",
                repositories=["acme/centaur"],
                repository_refs={"acme/centaur": "main"},
                repository_visibilities={"acme/centaur": "public"},
                sync_interval_seconds=30,
                github_token_file=root / "missing-token",
            )

            sync.write_ready()

            lines = (root / "cache" / ".repo-cache-ready").read_text().splitlines()
            self.assertEqual(lines[0], "repositories=acme/centaur")
            self.assertEqual(lines[1], "repository_refs=acme/centaur=main")
            self.assertEqual(lines[2], "repository_visibilities=acme/centaur=public")
            self.assertRegex(lines[3], r"^synced_at=\d{4}-\d{2}-\d{2}T")

    def test_check_ready_validates_fingerprint_and_repos(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            repo_path = root / "cache" / "private" / "acme" / "centaur" / ".git"
            repo_path.mkdir(parents=True)
            (root / "cache" / "acme").mkdir()
            (root / "cache" / "acme" / "centaur").symlink_to("../private/acme/centaur")
            sync = repo_cache_sync.RepoCacheSync(
                cache_dir=root / "cache",
                repositories=["acme/centaur"],
                repository_refs={"acme/centaur": "main"},
                repository_visibilities={"acme/centaur": "private"},
                sync_interval_seconds=30,
                github_token_file=root / "missing-token",
            )
            sync.write_ready()

            self.assertEqual(sync.check_ready(), 0)
            (root / "cache" / ".repo-cache-ready").write_text(
                "repositories=wrong\n"
                "repository_refs=acme/centaur=main\n"
                "repository_visibilities=acme/centaur=private\n"
            )
            self.assertEqual(sync.check_ready(), 1)

    def test_repository_targets_use_visibility_projection(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            sync = repo_cache_sync.RepoCacheSync(
                cache_dir=root / "cache",
                repositories=["acme/public", "acme/private"],
                repository_refs={},
                repository_visibilities={
                    "acme/public": "public",
                    "acme/private": "private",
                },
                sync_interval_seconds=30,
                github_token_file=root / "missing-token",
            )

            self.assertEqual(
                sync.repository_target("acme/public"),
                root / "cache" / "public" / "acme" / "public",
            )
            self.assertEqual(
                sync.repository_target("acme/private"),
                root / "cache" / "private" / "acme" / "private",
            )

    def test_legacy_link_points_to_visibility_projection(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            target = root / "cache" / "public" / "acme" / "docs"
            (target / ".git").mkdir(parents=True)
            sync = repo_cache_sync.RepoCacheSync(
                cache_dir=root / "cache",
                repositories=["acme/docs"],
                repository_refs={},
                repository_visibilities={"acme/docs": "public"},
                sync_interval_seconds=30,
                github_token_file=root / "missing-token",
            )

            sync.update_legacy_link("acme/docs", target)

            link = root / "cache" / "acme" / "docs"
            self.assertTrue(link.is_symlink())
            self.assertEqual(link.resolve(), target.resolve())

    def test_migrate_existing_checkout_moves_old_root_to_projection(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            old = root / "cache" / "acme" / "docs"
            (old / ".git").mkdir(parents=True)
            sync = repo_cache_sync.RepoCacheSync(
                cache_dir=root / "cache",
                repositories=["acme/docs"],
                repository_refs={},
                repository_visibilities={"acme/docs": "public"},
                sync_interval_seconds=30,
                github_token_file=root / "missing-token",
            )

            target = sync.repository_target("acme/docs")
            sync.migrate_existing_checkout("acme/docs", target)

            self.assertTrue((target / ".git").is_dir())
            self.assertFalse(old.exists())

    def test_sync_repo_updates_checkout_after_upstream_tag_moves(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            git_env = {
                **os.environ,
                "GIT_CONFIG_GLOBAL": str(root / "gitconfig"),
                "GIT_CONFIG_NOSYSTEM": "1",
                "GIT_AUTHOR_NAME": "test",
                "GIT_AUTHOR_EMAIL": "test@example.com",
                "GIT_COMMITTER_NAME": "test",
                "GIT_COMMITTER_EMAIL": "test@example.com",
            }

            def git(*args: str) -> str:
                return subprocess.run(
                    ["git", *args],
                    check=True,
                    env=git_env,
                    text=True,
                    capture_output=True,
                ).stdout.strip()

            remote = root / "remotes" / "acme" / "docs.git"
            git("init", "-q", "--bare", "-b", "main", str(remote))
            git(
                "config",
                "--global",
                f"url.file://{root / 'remotes'}/.insteadOf",
                "https://github.com/",
            )
            work = root / "work"
            git("clone", "-q", str(remote), str(work))
            git("-C", str(work), "commit", "-q", "--allow-empty", "-m", "one")
            git("-C", str(work), "tag", "v1")
            git("-C", str(work), "push", "-q", "origin", "HEAD:main", "v1")

            sync = repo_cache_sync.RepoCacheSync(
                cache_dir=root / "cache",
                repositories=["acme/docs"],
                repository_refs={},
                repository_visibilities={"acme/docs": "public"},
                sync_interval_seconds=30,
                github_token_file=root / "missing-token",
            )
            sync.git_env = git_env
            sync.sync_repo("acme/docs")

            git("-C", str(work), "commit", "-q", "--allow-empty", "-m", "two")
            git("-C", str(work), "tag", "-f", "v1")
            git("-C", str(work), "push", "-q", "--force", "origin", "HEAD:main", "v1")
            head = git("-C", str(work), "rev-parse", "HEAD")

            sync.sync_repo("acme/docs")

            target = sync.repository_target("acme/docs")
            self.assertEqual(git("-C", str(target), "rev-parse", "HEAD"), head)
            self.assertEqual(git("-C", str(target), "rev-parse", "v1"), head)

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
                repository_visibilities={},
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
