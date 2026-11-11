from __future__ import annotations

import os
import tempfile
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

    def _compose_with_overlays(
        self, root: Path, overlays: dict[str, dict[str, str]]
    ) -> str:
        home = root / "home"
        workspace = root / "workspace"
        home.mkdir()
        workspace.mkdir()
        (home / "AGENTS.md").write_text("base\n")
        repo_mount = home / "github"
        for repo, files in overlays.items():
            sandbox_dir = repo_mount / repo / "services" / "sandbox"
            sandbox_dir.mkdir(parents=True)
            for name, text in files.items():
                (sandbox_dir / name).write_text(text)

        target = workspace / "AGENTS.md"
        compose_system_prompt.compose_system_prompt(
            home_dir=home,
            target_prompt=target,
            repo_mount=repo_mount,
        )
        return target.read_text()

    def test_appends_hook_output_after_its_repo_prompt(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            prompt = self._compose_with_overlays(
                Path(tmp),
                {
                    "acme/first": {
                        "SYSTEM_PROMPT.md": "first overlay\n",
                        "SYSTEM_PROMPT.sh": 'echo "hook in $(basename "$PWD")"\n',
                    },
                    "acme/second": {"SYSTEM_PROMPT.md": "second overlay\n"},
                },
            )

            self.assertEqual(
                prompt,
                "base\n\n\n---\n\nfirst overlay\n\n\n---\n\nhook in first\n"
                "\n\n---\n\nsecond overlay\n",
            )

    def test_hook_without_static_prompt_is_appended(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            prompt = self._compose_with_overlays(
                Path(tmp),
                {"acme/overlay": {"SYSTEM_PROMPT.sh": "printf 'generated\\n'\n"}},
            )

            self.assertEqual(prompt, "base\n\n\n---\n\ngenerated\n")

    def test_skips_failing_and_empty_hooks(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            with mock.patch("sys.stderr"):
                prompt = self._compose_with_overlays(
                    Path(tmp),
                    {
                        "acme/empty": {"SYSTEM_PROMPT.sh": "echo '   '\n"},
                        "acme/failing": {
                            "SYSTEM_PROMPT.md": "failing overlay\n",
                            "SYSTEM_PROMPT.sh": "echo partial\nexit 3\n",
                        },
                    },
                )

            self.assertEqual(prompt, "base\n\n\n---\n\nfailing overlay\n")

    def test_skips_hook_that_times_out(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            with (
                mock.patch.dict(
                    os.environ,
                    {compose_system_prompt.HOOK_TIMEOUT_ENV: "0.2"},
                ),
                mock.patch("sys.stderr"),
            ):
                prompt = self._compose_with_overlays(
                    Path(tmp),
                    {"acme/slow": {"SYSTEM_PROMPT.sh": "echo late\nsleep 30 & wait\n"}},
                )

            self.assertEqual(prompt, "base\n")


if __name__ == "__main__":
    unittest.main()
