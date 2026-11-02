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
    """Fetch from a real repo-cache server; the token's access level is
    supplied directly, standing in for the claim iron-proxy injects."""

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

        self.access = "all"
        test = self

        class Server(repo_cache_sync.RepoCacheServer):
            def claims(self, authorization: str | None) -> dict[str, object]:
                return {"sub": "prn_test", "repo_cache": test.access}

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

    def test_fetch_follows_cache_commits_and_token_access(self) -> None:
        repo_cache_fetch.sync_once(self.base_url, self.dest)

        docs = self.dest / "acme" / "docs"
        self.assertEqual((docs / "one").read_text(), "one")
        self.assertTrue((self.dest / "acme" / "secret" / ".git").is_dir())
        shared = self.root / "shared"
        git("clone", "-q", "--shared", str(docs), str(shared))

        head = commit(self.checkouts["acme/docs"], "two")
        self.access = "public"
        repo_cache_fetch.sync_once(self.base_url, self.dest)

        self.assertEqual(git("-C", str(docs), "rev-parse", "HEAD"), head)
        self.assertEqual((docs / "two").read_text(), "two")
        self.assertFalse((self.dest / "acme" / "secret").exists())
        # In-place updates keep the objects --shared clones borrow.
        git("-C", str(shared), "fsck", "--connectivity-only")

        self.access = "none"
        repo_cache_fetch.sync_once(self.base_url, self.dest)
        self.assertFalse((self.dest / "acme").exists())


if __name__ == "__main__":
    unittest.main()
