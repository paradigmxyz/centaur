from __future__ import annotations

import os
import subprocess
import unittest
from pathlib import Path


SELECT_WORKSPACE_ROOT = Path(__file__).with_name("select_workspace_root.sh")
ENTRYPOINT = Path(__file__).with_name("entrypoint.sh")


class SelectWorkspaceRootTest(unittest.TestCase):
    def run_selector(
        self,
        *,
        home_dir: str = "/home/agent",
        state_dir: str = "/home/agent/state",
        persistent_state: str = "0",
        workspace_root: str | None = None,
    ) -> subprocess.CompletedProcess[str]:
        env = os.environ.copy()
        env.pop("CENTAUR_WORKSPACE_ROOT", None)
        if workspace_root is not None:
            env["CENTAUR_WORKSPACE_ROOT"] = workspace_root
        return subprocess.run(
            [str(SELECT_WORKSPACE_ROOT), home_dir, state_dir, persistent_state],
            capture_output=True,
            text=True,
            env=env,
            check=False,
        )

    def test_explicit_workspace_root_takes_precedence(self) -> None:
        result = self.run_selector(persistent_state="1", workspace_root="/workspace")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "/workspace")

    def test_persistent_state_falls_back_to_state_workspace(self) -> None:
        result = self.run_selector(persistent_state="1")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "/home/agent/state/workspace")

    def test_ephemeral_state_falls_back_to_home_workspace(self) -> None:
        result = self.run_selector()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "/home/agent/workspace")

    def test_explicit_workspace_root_must_be_absolute(self) -> None:
        result = self.run_selector(workspace_root="repos/project")

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("CENTAUR_WORKSPACE_ROOT must be absolute", result.stderr)

    def test_entrypoint_passes_exact_selector_arguments(self) -> None:
        entrypoint = ENTRYPOINT.read_text()

        self.assertIn(
            'WORKSPACE_DIR="$(select-workspace-root '
            '"$HOME_DIR" "$STATE_DIR" "${CENTAUR_PERSISTENT_STATE:-0}")"',
            entrypoint,
        )


if __name__ == "__main__":
    unittest.main()
