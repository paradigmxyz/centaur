from __future__ import annotations

from http.server import ThreadingHTTPServer
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest
from unittest import mock

import repo_cache_fetch
import repo_cache_sync

GIT_ENV = {
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_AUTHOR_NAME": "test",
    "GIT_AUTHOR_EMAIL": "test@example.com",
    "GIT_COMMITTER_NAME": "test",
    "GIT_COMMITTER_EMAIL": "test@example.com",
}


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], check=True, text=True, capture_output=True
    ).stdout.strip()


def commit(repo: Path, name: str) -> str:
    (repo / name).write_text(name)
    git("-C", str(repo), "add", name)
    git("-C", str(repo), "commit", "-q", "-m", name)
    return git("-C", str(repo), "rev-parse", "HEAD")


class RepoCacheFetchTest(unittest.TestCase):
    """Copy from a real repo-cache server; the token's principal and access
    level are supplied directly, standing in for what iron-proxy injects."""

    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        env = mock.patch.dict(
            os.environ,
            {
                **GIT_ENV,
                "GIT_CONFIG_GLOBAL": str(self.root / "gitconfig"),
                "http_proxy": "",
                "HTTP_PROXY": "",
            },
        )
        env.start()
        self.addCleanup(env.stop)
        sync = repo_cache_sync.RepoCacheSync(
            cache_dir=self.root / "cache",
            repositories=["acme/docs", "acme/secret"],
            repository_refs={},
            repository_visibilities={"acme/docs": "public", "acme/secret": "private"},
            sync_interval_seconds=30,
            github_token_file=self.root / "missing-token",
        )
        self.checkouts = {}
        for repo in sync.repositories:
            checkout = sync.repository_target(repo)
            git("init", "-q", "-b", "main", str(checkout))
            commit(checkout, "one")
            self.checkouts[repo] = checkout

        self.principal = "prn_warm"
        self.access = "all"
        test = self

        class Server(repo_cache_sync.RepoCacheServer):
            def claims(self, authorization: str | None) -> dict[str, object]:
                return {"sub": test.principal, "repo_cache": test.access}

        server = Server(sync=sync, secret=b"unused", audience="a", issuer="i")
        httpd = ThreadingHTTPServer(
            ("127.0.0.1", 0), repo_cache_sync._handler_class(server)
        )
        httpd.daemon_threads = True
        threading.Thread(target=httpd.serve_forever, daemon=True).start()
        self.addCleanup(httpd.server_close)
        self.addCleanup(httpd.shutdown)
        self.base_url = f"http://127.0.0.1:{httpd.server_address[1]}"
        self.dest = self.root / "github"

    def test_copy_follows_cache_commits_and_token_access(self) -> None:
        changed, failures = repo_cache_fetch.sync_once(self.base_url, self.dest)

        self.assertEqual((changed, failures), (True, []))
        docs = self.dest / "acme" / "docs"
        self.assertEqual((docs / "one").read_text(), "one")
        self.assertTrue((self.dest / "acme" / "secret" / ".git").is_dir())
        shared = self.root / "shared"
        git("clone", "-q", "--shared", str(docs), str(shared))

        head = commit(self.checkouts["acme/docs"], "two")
        self.assertEqual(
            repo_cache_fetch.sync_once(self.base_url, self.dest), (False, [])
        )
        self.assertEqual(git("-C", str(docs), "rev-parse", "HEAD"), head)
        self.assertEqual((docs / "two").read_text(), "two")
        self.assertEqual(git("-C", str(docs), "status", "--porcelain"), "")
        # In-place updates keep the objects --shared clones borrow.
        git("-C", str(shared), "fsck", "--connectivity-only")

        self.access = "public"
        self.assertEqual(
            repo_cache_fetch.sync_once(self.base_url, self.dest), (True, [])
        )
        self.assertFalse((self.dest / "acme" / "secret").exists())

        self.access = "none"
        repo_cache_fetch.sync_once(self.base_url, self.dest)
        self.assertFalse((self.dest / "acme").exists())

    def test_bind_applies_only_the_expected_principals_access(self) -> None:
        repo_cache_fetch.sync_once(self.base_url, self.dest)
        self.access = "public"

        # The proxy still presents the warm-pool principal: change nothing.
        with self.assertRaisesRegex(RuntimeError, "expected 'prn_claimed'"):
            repo_cache_fetch.sync_once(
                self.base_url, self.dest, expect_principal="prn_claimed", update=False
            )
        self.assertTrue((self.dest / "acme" / "secret" / ".git").is_dir())

        self.principal = "prn_claimed"
        changed, _ = repo_cache_fetch.sync_once(
            self.base_url, self.dest, expect_principal="prn_claimed", update=False
        )
        self.assertTrue(changed)
        self.assertFalse((self.dest / "acme" / "secret").exists())
        self.assertTrue((self.dest / "acme" / "docs" / ".git").is_dir())


if __name__ == "__main__":
    unittest.main()
