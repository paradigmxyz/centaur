from __future__ import annotations

import hashlib
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import repo_cache_sync


class RepoCacheSyncTest(unittest.TestCase):
    def make_sync(
        self,
        root: Path,
        *,
        repositories: list[str] | None = None,
        repository_clone_urls: dict[str, str] | None = None,
        git_username: str = "x-access-token",
    ) -> repo_cache_sync.RepoCacheSync:
        return repo_cache_sync.RepoCacheSync(
            cache_dir=root / "cache",
            repositories=repositories or ["acme/centaur"],
            repository_refs={},
            repository_visibilities={},
            repository_clone_urls=repository_clone_urls or {},
            sync_interval_seconds=30,
            git_username=git_username,
            git_token_file=root / "missing-token",
        )

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
                    "REPOSITORY_CLONE_URLS": json.dumps(
                        {
                            "acme/public": "http://git.example.test:82/acme/public.git",
                        }
                    ),
                    "REPOSITORY_VISIBILITIES": "acme/public=public acme/private=bogus",
                    "GIT_USERNAME": "oauth2",
                    "GIT_TOKEN_FILE": "/git-credentials/token",
                    "SYNC_INTERVAL_SECONDS": "10",
                }
            )

            sync = repo_cache_sync.RepoCacheSync.from_env()

            self.assertEqual(
                sync.repository_visibilities,
                {"acme/public": "public", "acme/private": "private"},
            )
            self.assertEqual(
                sync.repository_clone_urls,
                {"acme/public": "http://git.example.test:82/acme/public.git"},
            )
            self.assertEqual(sync.git_username, "oauth2")
            self.assertEqual(sync.git_token_file, Path("/git-credentials/token"))
        finally:
            os.environ.clear()
            os.environ.update(old_env)

    def test_repository_clone_url_uses_explicit_http_url(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            sync = self.make_sync(
                root,
                repository_clone_urls={
                    "acme/centaur": "http://git.example.test:82/acme/centaur.git",
                },
            )

            self.assertEqual(
                sync.repository_clone_url("acme/centaur"),
                "http://git.example.test:82/acme/centaur.git",
            )

    def test_repository_clone_url_falls_back_to_github(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            sync = self.make_sync(Path(tmp))

            self.assertEqual(
                sync.repository_clone_url("acme/centaur"),
                "https://github.com/acme/centaur.git",
            )

    def test_repository_clone_url_rejects_embedded_credentials(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            sync = self.make_sync(
                Path(tmp),
                repository_clone_urls={
                    "acme/centaur": "http://oauth2:secret@git.example.test/acme/centaur.git",
                },
            )

            with self.assertRaisesRegex(ValueError, "must not contain credentials"):
                sync.repository_clone_url("acme/centaur")

    def test_repository_clone_url_rejects_non_http_transport(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            sync = self.make_sync(
                Path(tmp),
                repository_clone_urls={
                    "acme/centaur": "ssh://git@git.example.test/acme/centaur.git",
                },
            )

            with self.assertRaisesRegex(ValueError, "must be an HTTP or HTTPS URL"):
                sync.repository_clone_url("acme/centaur")

    def test_repository_clone_url_rejects_query_and_fragment(self) -> None:
        for clone_url in (
            "https://git.example.test/acme/centaur.git?access_token=secret",
            "https://git.example.test/acme/centaur.git#secret",
            "https://git.example.test/acme/centaur.git?",
            "https://git.example.test/acme/centaur.git#",
        ):
            with self.subTest(clone_url=clone_url), tempfile.TemporaryDirectory() as tmp:
                sync = self.make_sync(
                    Path(tmp),
                    repository_clone_urls={"acme/centaur": clone_url},
                )

                with self.assertRaisesRegex(ValueError, "must not contain a query or fragment"):
                    sync.repository_clone_url("acme/centaur")

    def test_git_askpass_uses_configured_username_and_token_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            token_file = root / "token"
            token_file.write_text("not-a-real-token")
            sync = repo_cache_sync.RepoCacheSync(
                cache_dir=root / "cache",
                repositories=["acme/centaur"],
                repository_refs={},
                repository_visibilities={},
                repository_clone_urls={},
                sync_interval_seconds=30,
                git_username="gitlab-deploy-token",
                git_token_file=token_file,
            )

            env = sync._git_env()
            askpass = Path(env["GIT_ASKPASS"])
            self.addCleanup(askpass.unlink, missing_ok=True)
            content = askpass.read_text()

            self.assertIn("gitlab-deploy-token", content)
            self.assertIn(str(token_file), content)
            self.assertNotIn("not-a-real-token", content)

    def test_readiness_fingerprint_hashes_clone_urls(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            clone_url = "http://git.example.test:82/acme/centaur.git"
            sync = self.make_sync(
                root,
                repository_clone_urls={"acme/centaur": clone_url},
            )

            fingerprint = sync.repository_fingerprint()

            self.assertIn(hashlib.sha256(clone_url.encode()).hexdigest(), fingerprint)
            self.assertNotIn(clone_url, fingerprint)

    def test_git_failure_redacts_clone_url(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            clone_url = "http://git.example.test:82/acme/centaur.git"
            sync = self.make_sync(
                Path(tmp),
                repository_clone_urls={"acme/centaur": clone_url},
            )
            failure = subprocess.CalledProcessError(
                128,
                ["git", "clone"],
                stderr=f"fatal: unable to access '{clone_url}': connection refused",
            )

            with mock.patch("repo_cache_sync.subprocess.run", side_effect=failure):
                with self.assertRaises(RuntimeError) as raised:
                    sync._run_git(["clone", clone_url], "clone acme/centaur")

            message = str(raised.exception)
            self.assertIn("<redacted-clone-url>", message)
            self.assertNotIn(clone_url, message)

    def test_clone_url_change_replaces_cached_checkout_before_sync(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            old_url = "http://git-old.example.test/acme/centaur.git"
            new_url = "http://git-new.example.test/acme/centaur.git"
            sync = self.make_sync(
                root,
                repository_clone_urls={"acme/centaur": new_url},
            )
            target = sync.repository_target("acme/centaur")
            (target / ".git").mkdir(parents=True)
            stale_object = target / ".git" / "objects" / "stale-object"
            stale_object.parent.mkdir()
            stale_object.write_text("belongs to old remote")
            commands: list[list[str]] = []

            def fake_run_git(
                args: list[str], _label: str
            ) -> subprocess.CompletedProcess[str]:
                commands.append(args)
                if args[:2] == ["clone", "--quiet"]:
                    (Path(args[-1]) / ".git").mkdir(parents=True)
                return subprocess.CompletedProcess(["git", *args], 0, "", "")

            with (
                mock.patch.object(
                    sync,
                    "_git_ok",
                    side_effect=lambda repo_path, *_args: (repo_path / ".git").is_dir(),
                ),
                mock.patch.object(sync, "_git_output", return_value=old_url),
                mock.patch.object(sync, "_run_git", side_effect=fake_run_git),
                mock.patch.object(sync, "checkout_repo"),
            ):
                sync.sync_repo("acme/centaur")

            self.assertIn(["clone", "--quiet", new_url, str(target) + ".tmp"], commands)
            self.assertFalse(stale_object.exists())

    def test_clone_url_change_preserves_cached_checkout_when_reclone_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            new_url = "http://git-new.example.test/acme/centaur.git"
            sync = self.make_sync(
                root,
                repository_clone_urls={"acme/centaur": new_url},
            )
            target = sync.repository_target("acme/centaur")
            (target / ".git").mkdir(parents=True)
            stale_object = target / ".git" / "objects" / "stale-object"
            stale_object.parent.mkdir()
            stale_object.write_text("belongs to old remote")

            with (
                mock.patch.object(
                    sync,
                    "_git_ok",
                    side_effect=lambda repo_path, *_args: (repo_path / ".git").is_dir(),
                ),
                mock.patch.object(
                    sync,
                    "_git_output",
                    return_value="http://git-old.example.test/acme/centaur.git",
                ),
                mock.patch.object(
                    sync,
                    "_run_git",
                    side_effect=RuntimeError("clone acme/centaur failed"),
                ),
            ):
                with self.assertRaisesRegex(RuntimeError, "clone acme/centaur failed"):
                    sync.sync_repo("acme/centaur")

            self.assertEqual(stale_object.read_text(), "belongs to old remote")

    def test_write_ready_preserves_readiness_format(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)

            sync = repo_cache_sync.RepoCacheSync(
                cache_dir=root / "cache",
                repositories=["acme/centaur"],
                repository_refs={"acme/centaur": "main"},
                repository_visibilities={"acme/centaur": "public"},
                repository_clone_urls={},
                sync_interval_seconds=30,
                git_username="x-access-token",
                git_token_file=root / "missing-token",
            )

            sync.write_ready()

            lines = (root / "cache" / ".repo-cache-ready").read_text().splitlines()
            self.assertEqual(lines[0], "repositories=acme/centaur")
            self.assertEqual(lines[1], "repository_refs=acme/centaur=main")
            self.assertEqual(lines[2], "repository_visibilities=acme/centaur=public")
            self.assertRegex(
                lines[3], r"^repository_clone_url_hashes=acme/centaur=[0-9a-f]{64}$"
            )
            self.assertRegex(lines[4], r"^synced_at=\d{4}-\d{2}-\d{2}T")

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
                repository_clone_urls={},
                sync_interval_seconds=30,
                git_username="x-access-token",
                git_token_file=root / "missing-token",
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
                repository_clone_urls={},
                sync_interval_seconds=30,
                git_username="x-access-token",
                git_token_file=root / "missing-token",
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
                repository_clone_urls={},
                sync_interval_seconds=30,
                git_username="x-access-token",
                git_token_file=root / "missing-token",
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
                repository_clone_urls={},
                sync_interval_seconds=30,
                git_username="x-access-token",
                git_token_file=root / "missing-token",
            )

            target = sync.repository_target("acme/docs")
            sync.migrate_existing_checkout("acme/docs", target)

            self.assertTrue((target / ".git").is_dir())
            self.assertFalse(old.exists())

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
                repository_clone_urls={},
                sync_interval_seconds=30,
                git_username="x-access-token",
                git_token_file=Path("/tmp/missing-token"),
            )
            with self.assertRaises(RuntimeError):
                sync.run_forever()
            current_umask = os.umask(old_umask)
            self.assertEqual(current_umask, 0o022)
        finally:
            os.umask(old_umask)


if __name__ == "__main__":
    unittest.main()
