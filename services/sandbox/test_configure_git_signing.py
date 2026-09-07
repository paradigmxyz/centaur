import os
import subprocess
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("configure_git_signing.sh")
SANDBOX_IMAGE = os.environ.get("CENTAUR_SANDBOX_TEST_IMAGE")


def generate_key(
    root: Path,
    identity: str = "Centaur Test <centaur-test@example.com>",
    passphrase: str = "",
) -> tuple[Path, str]:
    source_home = root / "source-gnupg"
    source_home.mkdir(mode=0o700, exist_ok=True)
    source_env = {**os.environ, "GNUPGHOME": str(source_home)}
    subprocess.run(
        [
            "gpg",
            "--batch",
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            passphrase,
            "--quick-generate-key",
            identity,
            "ed25519",
            "sign",
            "0",
        ],
        env=source_env,
        check=True,
        capture_output=True,
    )
    fingerprint = (
        subprocess.run(
            ["gpg", "--batch", "--with-colons", "--list-secret-keys", identity],
            env=source_env,
            check=True,
            capture_output=True,
            text=True,
        )
        .stdout.split("fpr:::::::::", 1)[1]
        .split(":", 1)[0]
    )
    key_path = root / "private-key.asc"
    with key_path.open("wb") as key_file:
        subprocess.run(
            [
                "gpg",
                "--batch",
                "--pinentry-mode",
                "loopback",
                "--passphrase",
                passphrase,
                "--armor",
                "--export-secret-keys",
            ],
            env=source_env,
            check=True,
            stdout=key_file,
            stderr=subprocess.PIPE,
        )
    return key_path, fingerprint


class ConfigureGitSigningTests(unittest.TestCase):
    def run_script(
        self, root: Path, **environment: str
    ) -> subprocess.CompletedProcess[str]:
        env = os.environ.copy()
        env.update(
            {
                "GNUPGHOME": str(root / "gnupg"),
                "GIT_CONFIG_GLOBAL": str(root / "gitconfig"),
                **environment,
            }
        )
        return subprocess.run(
            [str(SCRIPT)],
            env=env,
            text=True,
            capture_output=True,
            check=False,
        )

    def test_disabled_is_a_noop(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            result = self.run_script(root)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse((root / "gnupg").exists())

    def test_enabled_requires_a_readable_key(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            result = self.run_script(
                root,
                CENTAUR_GIT_COMMIT_SIGNING_ENABLED="true",
                CENTAUR_GIT_COMMIT_SIGNING_KEY_PATH=str(root / "missing.asc"),
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("not readable", result.stderr)

    def test_rejects_an_invalid_enabled_value(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            result = self.run_script(
                Path(temp), CENTAUR_GIT_COMMIT_SIGNING_ENABLED="sometimes"
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("invalid CENTAUR_GIT_COMMIT_SIGNING_ENABLED", result.stderr)

    def test_rejects_multiple_secret_keys(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            generate_key(root, "First Test <first@example.com>")
            key_path, _ = generate_key(root, "Second Test <second@example.com>")

            result = self.run_script(
                root,
                CENTAUR_GIT_COMMIT_SIGNING_ENABLED="true",
                CENTAUR_GIT_COMMIT_SIGNING_KEY_PATH=str(key_path),
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("must contain exactly one", result.stderr)

    def test_rejects_a_passphrase_protected_key(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            key_path, _ = generate_key(root, passphrase="test-passphrase")

            result = self.run_script(
                root,
                CENTAUR_GIT_COMMIT_SIGNING_ENABLED="true",
                CENTAUR_GIT_COMMIT_SIGNING_KEY_PATH=str(key_path),
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("cannot sign non-interactively", result.stderr)

    def test_imports_key_and_signs_commits(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            key_path, fingerprint = generate_key(root)

            result = self.run_script(
                root,
                CENTAUR_GIT_COMMIT_SIGNING_ENABLED="true",
                CENTAUR_GIT_COMMIT_SIGNING_KEY_PATH=str(key_path),
            )
            self.assertEqual(result.returncode, 0, result.stderr)

            git_env = {
                **os.environ,
                "GNUPGHOME": str(root / "gnupg"),
                "GIT_CONFIG_GLOBAL": str(root / "gitconfig"),
            }
            repository = root / "repository"
            subprocess.run(
                ["git", "init", "-q", str(repository)], env=git_env, check=True
            )
            subprocess.run(
                ["git", "-C", str(repository), "config", "user.name", "Centaur Test"],
                env=git_env,
                check=True,
            )
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(repository),
                    "config",
                    "user.email",
                    "centaur-test@example.com",
                ],
                env=git_env,
                check=True,
            )
            subprocess.run(
                ["git", "-C", str(repository), "commit", "--allow-empty", "-m", "test"],
                env=git_env,
                check=True,
                capture_output=True,
            )
            subprocess.run(
                ["git", "-C", str(repository), "verify-commit", "HEAD"],
                env=git_env,
                check=True,
                capture_output=True,
            )
            self.assertEqual(
                subprocess.run(
                    ["git", "config", "--global", "user.signingKey"],
                    env=git_env,
                    check=True,
                    capture_output=True,
                    text=True,
                ).stdout.strip(),
                fingerprint,
            )


@unittest.skipUnless(
    SANDBOX_IMAGE,
    "set CENTAUR_SANDBOX_TEST_IMAGE to run the built-image integration tests",
)
class SandboxImageGitSigningTests(unittest.TestCase):
    def test_built_image_leaves_commits_unsigned_by_default(self) -> None:
        result = subprocess.run(
            [
                "docker",
                "run",
                "--rm",
                "--env",
                "CENTAUR_TOOLS_AUTO_RELOAD=false",
                SANDBOX_IMAGE or "",
                "/bin/bash",
                "-lc",
                """
set -euo pipefail
repo="$(mktemp -d)"
git init -q "$repo"
git -C "$repo" config user.name "Centaur Test"
git -C "$repo" config user.email "centaur-test@example.com"
git -C "$repo" commit --allow-empty -m "test: verify signing default" >&2
if git -C "$repo" verify-commit HEAD >/dev/null 2>&1; then
    echo "default commit was unexpectedly signed" >&2
    exit 1
fi
""",
            ],
            text=True,
            capture_output=True,
            check=False,
            timeout=120,
        )

        self.assertEqual(result.returncode, 0, result.stderr)

    def run_image(
        self, key_path: Path, script: str
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                "docker",
                "run",
                "--rm",
                "--env",
                "CENTAUR_GIT_COMMIT_SIGNING_ENABLED=true",
                "--env",
                "CENTAUR_TOOLS_AUTO_RELOAD=false",
                "--volume",
                f"{key_path}:/var/run/secrets/centaur/git-signing/private-key.asc:ro",
                SANDBOX_IMAGE or "",
                "/bin/bash",
                "-lc",
                script,
            ],
            text=True,
            capture_output=True,
            check=False,
            timeout=120,
        )

    def test_built_image_entrypoint_produces_a_verified_signed_commit(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            key_path, fingerprint = generate_key(Path(temp))
            result = self.run_image(
                key_path,
                """
set -euo pipefail
repo="$(mktemp -d)"
git init -q "$repo"
git -C "$repo" config user.name "Centaur Test"
git -C "$repo" config user.email "centaur-test@example.com"
git -C "$repo" commit --allow-empty -m "test: verify signing" >&2
git -C "$repo" verify-commit HEAD >&2
git -C "$repo" log -1 --format=%GF
""",
            )

            self.assertEqual(
                result.returncode,
                0,
                f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}",
            )
            self.assertEqual(result.stdout.strip(), fingerprint)

    def test_built_image_entrypoint_fails_closed_for_invalid_key(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            key_path = Path(temp) / "private-key.asc"
            key_path.write_text("not an OpenPGP key", encoding="utf-8")

            result = self.run_image(key_path, "exit 0")

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("not valid OpenPGP key material", result.stderr)


if __name__ == "__main__":
    unittest.main()
