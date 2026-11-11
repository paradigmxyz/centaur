from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

import compose_system_prompt


class ComposeSystemPromptTest(unittest.TestCase):
    def test_appends_multiple_overlay_prompts_in_order(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            home = root / "home"
            workspace = root / "workspace"
            home.mkdir()
            workspace.mkdir()
            (home / "AGENTS.md").write_text("base\n")

            repo_mount = home / "github"
            first = repo_mount / "acme" / "first" / "services" / "sandbox" / "SYSTEM_PROMPT.md"
            second = repo_mount / "acme" / "second" / "services" / "sandbox" / "SYSTEM_PROMPT.md"
            first.parent.mkdir(parents=True)
            second.parent.mkdir(parents=True)
            first.write_text("first overlay\n")
            second.write_text("second overlay\n")

            target = workspace / "AGENTS.md"
            compose_system_prompt.compose_system_prompt(
                home_dir=home,
                target_prompt=target,
                repo_mount=repo_mount,
            )

            self.assertEqual(
                target.read_text(),
                "base\n\n\n---\n\nfirst overlay\n\n\n---\n\nsecond overlay\n",
            )

    def test_uses_agents_base_and_appends_home_overlay_before_repo_overlays(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            home = root / "home"
            workspace = root / "workspace"
            home.mkdir()
            workspace.mkdir()
            (home / "AGENTS.md").write_text("baked\n")
            (home / "AGENTS_BASE.md").write_text("persona base\n")
            (home / "AGENTS_OVERLAY.md").write_text("home overlay\n")

            repo_mount = home / "github"
            prompt = repo_mount / "acme" / "overlay" / "services" / "sandbox" / "SYSTEM_PROMPT.md"
            prompt.parent.mkdir(parents=True)
            prompt.write_text("repo overlay\n")

            target = workspace / "AGENTS.md"
            compose_system_prompt.compose_system_prompt(
                home_dir=home,
                target_prompt=target,
                repo_mount=repo_mount,
            )

            self.assertEqual(
                target.read_text(),
                "persona base\n\n\n---\n\nhome overlay\n\n\n---\n\nrepo overlay\n",
            )

    def test_appends_persona_after_deployment_overlays(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            home = root / "home"
            workspace = root / "workspace"
            home.mkdir()
            workspace.mkdir()
            (home / "AGENTS.md").write_text("base\n")
            (home / "AGENTS_OVERLAY.md").write_text("home overlay\n")

            repo_mount = home / "github"
            repo_overlay = (
                repo_mount
                / "acme"
                / "overlay"
                / "services"
                / "sandbox"
                / "SYSTEM_PROMPT.md"
            )
            repo_overlay.parent.mkdir(parents=True)
            repo_overlay.write_text("repo overlay\n")

            persona_prompt = "engineering persona\n"
            (home / "AGENTS_PERSONA.md").write_text(persona_prompt)
            target = workspace / "AGENTS.md"
            compose_system_prompt.compose_system_prompt(
                home_dir=home,
                target_prompt=target,
                repo_mount=repo_mount,
            )

            self.assertEqual(
                target.read_text(),
                "base\n\n\n---\n\nhome overlay\n\n\n---\n\nrepo overlay\n"
                "\n\n---\n\nengineering persona\n",
            )

    def test_skips_mounted_copy_of_baked_root_prompt(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            home = root / "home"
            workspace = root / "workspace"
            home.mkdir()
            workspace.mkdir()
            (home / "AGENTS.md").write_text("base\n")

            repo_mount = home / "github"
            root_prompt = (
                repo_mount
                / "paradigmxyz"
                / "centaur"
                / "services"
                / "sandbox"
                / "SYSTEM_PROMPT.md"
            )
            overlay_prompt = (
                repo_mount
                / "acme"
                / "overlay"
                / "services"
                / "sandbox"
                / "SYSTEM_PROMPT.md"
            )
            root_prompt.parent.mkdir(parents=True)
            overlay_prompt.parent.mkdir(parents=True)
            root_prompt.write_text("base\n")
            overlay_prompt.write_text("overlay\n")

            target = workspace / "AGENTS.md"
            compose_system_prompt.compose_system_prompt(
                home_dir=home,
                target_prompt=target,
                repo_mount=repo_mount,
            )

            self.assertEqual(
                target.read_text(),
                "base\n\n\n---\n\noverlay\n",
            )


class FirstTurnPromptHookTest(unittest.TestCase):
    def _compose(
        self,
        root: Path,
        hooks: dict[str, str],
        *,
        overlay_dirs: list[str] | None = None,
        env: dict[str, str] | None = None,
        first_turn_prompt_hooks: bool = True,
        hooks_timeout: float = 10.0,
    ) -> str:
        home = root / "home"
        workspace = root / "workspace"
        home.mkdir()
        workspace.mkdir()
        (home / "AGENTS.md").write_text("base\n")
        (home / "AGENTS_PERSONA.md").write_text("persona\n")
        repo_mount = home / "github"
        for repo, script in hooks.items():
            hook = (
                repo_mount
                / repo
                / "services"
                / "sandbox"
                / "hooks"
                / "first-turn-prompt.sh"
            )
            hook.parent.mkdir(parents=True)
            hook.write_text(script)
        if overlay_dirs is None:
            overlay_dirs = list(hooks)
        hook_env = {
            compose_system_prompt.OVERLAY_REPO_DIRS_ENV: ":".join(
                str(repo_mount / repo) for repo in overlay_dirs
            ),
            **(env or {}),
        }

        target = workspace / "AGENTS.md"
        with mock.patch.dict(os.environ, hook_env), mock.patch("sys.stderr"):
            compose_system_prompt.compose_system_prompt(
                home_dir=home,
                target_prompt=target,
                repo_mount=repo_mount,
                first_turn_prompt_hooks=first_turn_prompt_hooks,
                hooks_timeout=hooks_timeout,
            )
        return target.read_text()

    def test_appends_hook_output_in_overlay_order_before_persona(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            prompt = self._compose(
                Path(tmp),
                {
                    "acme/second": 'echo "from $(basename "$PWD")"\n',
                    "acme/first": "echo from first\n",
                },
                overlay_dirs=["acme/first", "acme/second"],
            )

            self.assertEqual(
                prompt,
                "base\n\n\n---\n\nfrom first\n\n\n---\n\nfrom second\n"
                "\n\n---\n\npersona\n",
            )

    def test_does_not_run_hooks_without_the_flag(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            prompt = self._compose(
                Path(tmp),
                {"acme/overlay": "echo hooked\n"},
                first_turn_prompt_hooks=False,
            )

            self.assertEqual(prompt, "base\n\n\n---\n\npersona\n")

    def test_ignores_hooks_in_repos_that_are_not_overlays(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            prompt = self._compose(
                Path(tmp),
                {"acme/overlay": "echo overlay\n", "acme/other": "echo other\n"},
                overlay_dirs=["acme/overlay"],
            )

            self.assertEqual(prompt, "base\n\n\n---\n\noverlay\n\n\n---\n\npersona\n")

    def test_skips_hook_that_resolves_outside_its_repo(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            outside = root / "outside.sh"
            outside.write_text("echo escaped\n")
            home = root / "home"
            hook_dir = (
                home / "github" / "acme" / "overlay" / "services" / "sandbox" / "hooks"
            )
            hook_dir.mkdir(parents=True)
            (hook_dir / "first-turn-prompt.sh").symlink_to(outside)
            workspace = root / "workspace"
            workspace.mkdir()
            (home / "AGENTS.md").write_text("base\n")
            target = workspace / "AGENTS.md"
            env = {
                compose_system_prompt.OVERLAY_REPO_DIRS_ENV: str(
                    home / "github" / "acme" / "overlay"
                )
            }
            with mock.patch.dict(os.environ, env), mock.patch("sys.stderr"):
                compose_system_prompt.compose_system_prompt(
                    home_dir=home,
                    target_prompt=target,
                    repo_mount=home / "github",
                    first_turn_prompt_hooks=True,
                )

            self.assertEqual(target.read_text(), "base\n")

    def test_skips_failing_empty_and_oversized_hooks(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            prompt = self._compose(
                Path(tmp),
                {
                    "acme/empty": "echo '   '\n",
                    "acme/failing": "echo partial\nexit 3\n",
                    "acme/large": "head -c 100 /dev/zero | tr '\\0' x\n",
                    "acme/ok": "echo ok\n",
                },
                overlay_dirs=["acme/empty", "acme/failing", "acme/large", "acme/ok"],
                env={compose_system_prompt.HOOK_MAX_BYTES_ENV: "50"},
            )

            self.assertEqual(prompt, "base\n\n\n---\n\nok\n\n\n---\n\npersona\n")

    def test_skips_hook_that_times_out_and_kills_its_children(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            prompt = self._compose(
                Path(tmp),
                {"acme/slow": "echo late\nsleep 30 & wait\n"},
                hooks_timeout=0.2,
            )

            self.assertEqual(prompt, "base\n\n\n---\n\npersona\n")


    def test_timeout_does_not_wait_for_children_that_left_the_group(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            pid_file = root / "escaped.pid"
            started = time.monotonic()
            prompt = self._compose(
                root,
                {
                    "acme/escape": (
                        f"setsid sleep 30 & echo $! > {pid_file}\nsleep 30\n"
                    ),
                    "acme/exited": "setsid sleep 30 &\nexit 0\n",
                },
                hooks_timeout=0.3,
            )
            elapsed = time.monotonic() - started
            escaped = int(pid_file.read_text())
            os.kill(escaped, signal.SIGKILL)

            self.assertLess(elapsed, 5)
            self.assertEqual(prompt, "base\n\n\n---\n\npersona\n")

    def test_one_time_limit_covers_every_hook(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            started = time.monotonic()
            prompt = self._compose(
                Path(tmp),
                {
                    "acme/a": "sleep 0.4\necho a\n",
                    "acme/b": "sleep 0.4\necho b\n",
                    "acme/c": "echo c\n",
                },
                overlay_dirs=["acme/a", "acme/b", "acme/c"],
                hooks_timeout=0.6,
            )
            elapsed = time.monotonic() - started

            # a fits; b runs out the shared limit; nothing is left for c.
            self.assertEqual(prompt, "base\n\n\n---\n\na\n\n\n---\n\npersona\n")
            self.assertLess(elapsed, 2)

    def test_skips_listed_overlays_that_are_not_mounted(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            # A public-only session mounts only public repos, so a private
            # overlay listed in the env has no hook on disk.
            prompt = self._compose(
                Path(tmp),
                {"acme/public": "echo public\n"},
                overlay_dirs=["acme/private", "acme/public"],
            )

            self.assertEqual(prompt, "base\n\n\n---\n\npublic\n\n\n---\n\npersona\n")

    def test_sigterm_kills_the_running_hook(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            home = root / "home"
            workspace = root / "workspace"
            workspace.mkdir()
            hook_dir = home / "github" / "acme" / "slow" / "services/sandbox/hooks"
            hook_dir.mkdir(parents=True)
            (home / "AGENTS.md").write_text("base\n")
            pid_file = root / "hook.pid"
            (hook_dir / "first-turn-prompt.sh").write_text(
                f"echo $$ > {pid_file}\nsleep 30\n"
            )
            env = {
                **os.environ,
                compose_system_prompt.OVERLAY_REPO_DIRS_ENV: str(
                    home / "github" / "acme" / "slow"
                ),
            }
            composer = subprocess.Popen(
                [
                    sys.executable,
                    compose_system_prompt.__file__,
                    "--home-dir",
                    str(home),
                    "--target-prompt",
                    str(workspace / "AGENTS.md"),
                    "--first-turn-prompt-hooks",
                ],
                env=env,
                stderr=subprocess.DEVNULL,
            )
            deadline = time.monotonic() + 5
            while not pid_file.exists() and time.monotonic() < deadline:
                time.sleep(0.05)
            hook_pid = int(pid_file.read_text())

            composer.send_signal(signal.SIGTERM)
            self.assertEqual(composer.wait(timeout=5), 128 + signal.SIGTERM)
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                try:
                    os.kill(hook_pid, 0)
                except ProcessLookupError:
                    break
                time.sleep(0.05)
            with self.assertRaises(ProcessLookupError):
                os.kill(hook_pid, 0)

    def test_reports_each_hook_run_as_json(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            home = root / "home"
            workspace = root / "workspace"
            workspace.mkdir()
            home.mkdir()
            (home / "AGENTS.md").write_text("base\n")
            overlays = {"acme/ok": "echo ok\n", "acme/failing": "exit 4\n"}
            for repo, script in overlays.items():
                hook = home / "github" / repo / "services/sandbox/hooks"
                hook.mkdir(parents=True)
                (hook / "first-turn-prompt.sh").write_text(script)
            env = {
                **os.environ,
                compose_system_prompt.OVERLAY_REPO_DIRS_ENV: ":".join(
                    str(home / "github" / repo) for repo in overlays
                ),
            }

            completed = subprocess.run(
                [
                    sys.executable,
                    compose_system_prompt.__file__,
                    "--home-dir",
                    str(home),
                    "--target-prompt",
                    str(workspace / "AGENTS.md"),
                    "--first-turn-prompt-hooks",
                    "--report-json",
                ],
                env=env,
                capture_output=True,
                text=True,
                check=True,
            )

            hooks = json.loads(completed.stdout)["hooks"]
            self.assertEqual(
                [
                    (h["repo"].rsplit("/", 1)[1], h["status"], h["exit_code"])
                    for h in hooks
                ],
                [("ok", "ok", 0), ("failing", "failed", 4)],
            )
            self.assertEqual(hooks[0]["output_bytes"], 3)
            for hook in hooks:
                self.assertLessEqual(hook["started_at"], hook["ended_at"])
            self.assertEqual(
                (workspace / "AGENTS.md").read_text(), "base\n\n\n---\n\nok\n"
            )

if __name__ == "__main__":
    unittest.main()
