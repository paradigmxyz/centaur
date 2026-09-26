from __future__ import annotations

import os
import shlex
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SANDBOX = Path(__file__).parent


class EntrypointStateTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.config = self.root / "harness" / "codex"
        self.config.mkdir(parents=True)
        (self.config / "config.toml").write_text('model = "fixture"\n')
        (self.bin / "python3").symlink_to(sys.executable)
        self.command("install-tool-shims", "exit 0\n")
        self.command(
            "compose-system-prompt",
            f"exec {shlex.quote(sys.executable)} "
            f"{shlex.quote(str(SANDBOX / 'compose_system_prompt.py'))} \"$@\"\n",
        )

    def command(self, name: str, body: str) -> None:
        path = self.bin / name
        path.write_text("#!/bin/sh\n" + body)
        path.chmod(0o755)

    def boot(self, home: Path, state: Path, command: str) -> str:
        home.mkdir(exist_ok=True)
        (home / "AGENTS.md").write_text("Provider-free entrypoint test.\n")
        result = subprocess.run(
            ["bash", str(SANDBOX / "entrypoint.sh"), "sh", "-c", command],
            env={
                "PATH": f"{self.bin}{os.pathsep}{os.environ['PATH']}",
                "HOME": str(home),
                "CENTAUR_STATE_DIR": str(state),
                "CENTAUR_HARNESS_CONFIG_DIR": str(self.config.parent),
                "CENTAUR_TOOLS_AUTO_RELOAD": "false",
                "GOOGLE_APPLICATION_CREDENTIALS": "/dev/null",
            },
            check=True,
            capture_output=True,
            text=True,
            timeout=15,
        )
        return result.stdout

    def test_omp_history_survives_replaced_home_and_repeated_boot(self) -> None:
        state = self.root / "state"
        state.mkdir()
        first = self.root / "first-home"
        second = self.root / "second-home"
        self.boot(
            first,
            state,
            'mkdir -p "$HOME/.omp/centaur-sessions/mappings"; '
            'printf "native-session-1\\n" > "$HOME/.omp/centaur-sessions/session.jsonl"; '
            'printf "thread-mapping-1\\n" > "$HOME/.omp/centaur-sessions/mappings/thread.json"',
        )
        read_history = (
            'cat "$HOME/.omp/centaur-sessions/session.jsonl" '
            '"$HOME/.omp/centaur-sessions/mappings/thread.json"'
        )
        for home in [second, first]:
            with self.subTest(home=home.name):
                self.assertEqual(
                    self.boot(home, state, read_history),
                    "native-session-1\nthread-mapping-1\n",
                )

    def test_replaced_home_without_state_volume_has_no_omp_history(self) -> None:
        state = self.root / "absent-state"
        self.boot(
            self.root / "first-home",
            state,
            'mkdir -p "$HOME/.omp/centaur-sessions"; '
            'printf "old-session\\n" > "$HOME/.omp/centaur-sessions/session.jsonl"',
        )
        self.assertEqual(
            self.boot(
                self.root / "second-home",
                state,
                'if [ -f "$HOME/.omp/centaur-sessions/session.jsonl" ]; '
                'then echo stale; else echo fresh; fi',
            ),
            "fresh\n",
        )


if __name__ == "__main__":
    unittest.main()
