from __future__ import annotations

import os
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path


SANDBOX_DIR = Path(__file__).parent
GIT_BRANCH = SANDBOX_DIR / "git-branch.sh"
COMMIT_MSG_HOOK = SANDBOX_DIR / "git-hooks" / "commit-msg"


class GitBranchTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tempdir = tempfile.TemporaryDirectory()
        self.root = Path(self.tempdir.name)
        self.home = self.root / "home"
        self.source = self.home / "github" / "acme" / "centaur"
        self.source.mkdir(parents=True)
        self._git("init", "--initial-branch=main", str(self.source))
        self._git("-C", str(self.source), "config", "user.name", "Seed User")
        self._git("-C", str(self.source), "config", "user.email", "seed@example.com")
        (self.source / "README.md").write_text("seed\n")
        self._git("-C", str(self.source), "add", "README.md")
        self._git("-C", str(self.source), "commit", "-m", "chore: seed repository")

    def tearDown(self) -> None:
        self.tempdir.cleanup()

    def _git(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["git", *args],
            check=True,
            capture_output=True,
            text=True,
            env={**os.environ, "GIT_CONFIG_NOSYSTEM": "1"},
        )

    def _run_git_branch(self, extra_env: dict[str, str]) -> Path:
        result = subprocess.run(
            [str(GIT_BRANCH), "acme/centaur", "fix-attribution"],
            check=True,
            capture_output=True,
            text=True,
            env={
                **os.environ,
                "GIT_CONFIG_NOSYSTEM": "1",
                "HOME": str(self.home),
                **extra_env,
            },
        )
        return Path(result.stdout.strip())

    def test_uses_authenticated_github_identity(self) -> None:
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        gh = bin_dir / "gh"
        gh.write_text("#!/bin/sh\nprintf 'Perry Dime\\tsvc_ai@paradigm.xyz\\n'\n")
        gh.chmod(gh.stat().st_mode | stat.S_IXUSR)

        destination = self._run_git_branch(
            {
                "GITHUB_TOKEN": "placeholder",
                "PATH": f"{bin_dir}:{os.environ['PATH']}",
            }
        )

        self.assertEqual(
            self._git("-C", str(destination), "config", "user.name").stdout.strip(),
            "Perry Dime",
        )
        self.assertEqual(
            self._git("-C", str(destination), "config", "user.email").stdout.strip(),
            "svc_ai@paradigm.xyz",
        )
        (destination / "CHANGELOG.md").write_text("fixed\n")
        self._git("-C", str(destination), "add", "CHANGELOG.md")
        self._git("-C", str(destination), "commit", "-m", "fix: test attribution")
        commit = self._git(
            "-C",
            str(destination),
            "show",
            "-s",
            "--format=%an <%ae>%n%B",
            "HEAD",
        ).stdout
        self.assertTrue(commit.startswith("Perry Dime <svc_ai@paradigm.xyz>\n"))
        self.assertNotIn("Co-authored-by:", commit)

    def test_explicit_identity_does_not_require_github(self) -> None:
        destination = self._run_git_branch(
            {
                "CENTAUR_GIT_USER_NAME": "Release Bot",
                "CENTAUR_GIT_USER_EMAIL": "release@example.com",
                "GITHUB_TOKEN": "",
            }
        )

        self.assertEqual(
            self._git("-C", str(destination), "config", "user.name").stdout.strip(),
            "Release Bot",
        )
        self.assertEqual(
            self._git("-C", str(destination), "config", "user.email").stdout.strip(),
            "release@example.com",
        )

    def test_refreshes_identity_when_reusing_clone(self) -> None:
        destination = self._run_git_branch(
            {
                "CENTAUR_GIT_USER_NAME": "Old Bot",
                "CENTAUR_GIT_USER_EMAIL": "old@example.com",
                "GITHUB_TOKEN": "",
            }
        )
        reused = self._run_git_branch(
            {
                "CENTAUR_GIT_USER_NAME": "New Bot",
                "CENTAUR_GIT_USER_EMAIL": "new@example.com",
                "GITHUB_TOKEN": "",
            }
        )

        self.assertEqual(reused, destination)
        self.assertEqual(
            self._git("-C", str(destination), "config", "user.name").stdout.strip(),
            "New Bot",
        )
        self.assertEqual(
            self._git("-C", str(destination), "config", "user.email").stdout.strip(),
            "new@example.com",
        )

    def test_rejects_partial_explicit_identity(self) -> None:
        result = subprocess.run(
            [str(GIT_BRANCH), "acme/centaur", "fix-attribution"],
            check=False,
            capture_output=True,
            text=True,
            env={
                **os.environ,
                "GIT_CONFIG_NOSYSTEM": "1",
                "HOME": str(self.home),
                "CENTAUR_GIT_USER_NAME": "Release Bot",
                "CENTAUR_GIT_USER_EMAIL": "",
                "GITHUB_TOKEN": "",
            },
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must be set together", result.stderr)


class CommitMessageHookTest(unittest.TestCase):
    def test_rejects_centaur_ai_coauthor(self) -> None:
        with tempfile.NamedTemporaryFile(mode="w") as message:
            message.write(
                "fix: remove generated attribution\n\n"
                "Co-authored-by: Centaur AI <ai@centaur.local>\n"
            )
            message.flush()

            result = subprocess.run(
                [str(COMMIT_MSG_HOOK), message.name],
                check=False,
                capture_output=True,
                text=True,
            )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("AI co-author attribution", result.stderr)


if __name__ == "__main__":
    unittest.main()
