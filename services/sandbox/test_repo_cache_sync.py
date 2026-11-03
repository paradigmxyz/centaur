from __future__ import annotations

import base64
import hashlib
import hmac
from http.server import ThreadingHTTPServer
import json
import os
import subprocess
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.request
from pathlib import Path

import repo_cache_sync

JWT_SECRET = b"test-signing-secret"


def _b64(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def make_jwt(
    claims: dict[str, object], *, alg: str = "HS256", secret: bytes = JWT_SECRET
) -> str:
    header = _b64(json.dumps({"alg": alg, "typ": "JWT"}).encode())
    payload = _b64(json.dumps(claims).encode())
    signature = hmac.new(
        secret, f"{header}.{payload}".encode(), hashlib.sha256
    ).digest()
    return f"{header}.{payload}.{_b64(signature)}"


def api_claims(**overrides: object) -> dict[str, object]:
    now = int(time.time())
    return {
        "iss": "centaur-console",
        "aud": "centaur-api",
        "iat": now,
        "exp": now + 3600,
        "sub": "prn_test",
        "repo_cache": "all",
        **overrides,
    }


class RepoCacheServerTest(unittest.TestCase):
    def verify(self, token: str) -> dict[str, object] | None:
        return repo_cache_sync.verify_api_jwt(
            token,
            secret=JWT_SECRET,
            audience="centaur-api",
            issuer="centaur-console",
            now=time.time(),
        )

    def test_verify_api_jwt_applies_api_rs_rules(self) -> None:
        self.assertEqual(self.verify(make_jwt(api_claims()))["sub"], "prn_test")
        now = int(time.time())
        header, payload, signature = make_jwt(api_claims()).split(".")
        forged = _b64(json.dumps(api_claims(sub="prn_other")).encode())
        for token in [
            make_jwt(api_claims(), secret=b"other-secret"),
            make_jwt(api_claims(), alg="none"),
            f"{header}.{forged}.{signature}",
            make_jwt(api_claims(exp=now - 120)),
            make_jwt(api_claims(iat=now + 120)),
            make_jwt(api_claims(nbf=now + 120)),
            make_jwt(api_claims(aud="centaur-console-sandbox-entitlements")),
            make_jwt(api_claims(iss="someone-else")),
            make_jwt(api_claims(sub=" ")),
            make_jwt({k: v for k, v in api_claims().items() if k != "exp"}),
            "not-a-jwt",
        ]:
            self.assertIsNone(self.verify(token), token)

    def test_server_requires_token_and_scopes_clones_to_access(self) -> None:
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
                "http_proxy": "",
            }

            def git(*args: str) -> subprocess.CompletedProcess[str]:
                return subprocess.run(
                    ["git", *args], env=git_env, text=True, capture_output=True
                )

            sync = repo_cache_sync.RepoCacheSync(
                cache_dir=root / "cache",
                repositories=["acme/docs", "acme/secret"],
                repository_refs={},
                repository_visibilities={"acme/docs": "public"},
                sync_interval_seconds=30,
                github_token_file=root / "missing-token",
            )
            for repo in sync.repositories:
                checkout = sync.repository_target(repo)
                git("init", "-q", "-b", "main", str(checkout))
                git("-C", str(checkout), "commit", "-q", "--allow-empty", "-m", "one")
            server = repo_cache_sync.RepoCacheServer(
                sync=sync,
                secret=JWT_SECRET,
                audience="centaur-api",
                issuer="centaur-console",
            )
            httpd = ThreadingHTTPServer(
                ("127.0.0.1", 0), repo_cache_sync._handler_class(server)
            )
            threading.Thread(target=httpd.serve_forever, daemon=True).start()
            self.addCleanup(httpd.server_close)
            self.addCleanup(httpd.shutdown)
            base_url = f"http://127.0.0.1:{httpd.server_address[1]}"
            opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

            def status(path: str, token: str | None) -> int:
                headers = {"Authorization": f"Bearer {token}"} if token else {}
                try:
                    with opener.open(
                        urllib.request.Request(base_url + path, headers=headers)
                    ) as response:
                        response.read()
                        return response.status
                except urllib.error.HTTPError as error:
                    return error.code

            def clone(repo: str, token: str) -> int:
                return git(
                    "-c",
                    f"http.extraHeader=Authorization: Bearer {token}",
                    "clone",
                    "-q",
                    f"{base_url}/{repo}.git",
                    str(root / "clones" / token[-8:] / repo),
                ).returncode

            public = make_jwt(api_claims(repo_cache="public"))
            full = make_jwt(api_claims(repo_cache="all"))
            unknown = make_jwt(api_claims(repo_cache="internal"))
            self.assertEqual(status("/repos", None), 403)
            self.assertEqual(
                status("/acme/docs.git/info/refs?service=git-upload-pack", None), 403
            )
            self.assertEqual(status("/acme/docs.tar", None), 403)
            self.assertEqual(status("/acme/docs.tar", public), 200)
            self.assertEqual(status("/acme/secret.tar", public), 404)
            self.assertEqual(status("/acme/secret.tar", full), 200)
            self.assertEqual(clone("acme/docs", public), 0)
            self.assertNotEqual(clone("acme/secret", public), 0)
            self.assertNotEqual(clone("acme/docs", unknown), 0)
            self.assertEqual(clone("acme/secret", full), 0)
            self.assertEqual(
                status("/acme/secret.git/info/refs?service=git-receive-pack", full), 404
            )
            self.assertEqual(status("/acme/secret.git/HEAD", full), 404)


class RepoCacheSyncTest(unittest.TestCase):
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
                }
            )

            sync = repo_cache_sync.RepoCacheSync.from_env()

            self.assertEqual(
                sync.repository_visibilities,
                {"acme/public": "public", "acme/private": "private"},
            )
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

    def test_git_askpass_reads_token_written_after_env_is_built(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            token_file = Path(tmp) / "token"
            sync = repo_cache_sync.RepoCacheSync(
                cache_dir=Path(tmp) / "cache",
                repositories=["acme/private"],
                repository_refs={},
                repository_visibilities={"acme/private": "private"},
                sync_interval_seconds=30,
                github_token_file=token_file,
            )
            askpass = sync._git_env()["GIT_ASKPASS"]

            def password() -> str:
                return subprocess.run(
                    [askpass, "Password for 'https://x-access-token@github.com': "],
                    check=True,
                    text=True,
                    capture_output=True,
                ).stdout.strip()

            self.assertEqual(password(), "")
            token_file.write_text("first-token\n")
            self.assertEqual(password(), "first-token")
            token_file.write_text("rotated-token\n")
            self.assertEqual(password(), "rotated-token")

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
