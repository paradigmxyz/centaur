from __future__ import annotations

import contextlib
import io
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import install_tool_shims


class CopyPublishedToolsTest(unittest.TestCase):
    def test_copies_tool_dirs_and_skips_duplicate_names(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            published = root / "published"
            target = root / "target"

            (target / "research" / "sensortower").mkdir(parents=True)
            (target / "research" / "sensortower" / "pyproject.toml").write_text("base\n")
            (target / "research" / "websearch").mkdir(parents=True)
            (target / "research" / "websearch" / "pyproject.toml").write_text("old project\n")
            (target / "research" / "websearch" / "old.py").write_text("old\n")

            (published / "research" / "websearch").mkdir(parents=True)
            (published / "research" / "websearch" / "pyproject.toml").write_text("new project\n")
            (published / "research" / "websearch" / "new.py").write_text("new\n")
            (published / "research" / "company").mkdir(parents=True)
            (published / "research" / "company" / "pyproject.toml").write_text("company\n")

            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                install_tool_shims._copy_published_tools(target, published)

            self.assertEqual(
                (target / "research" / "sensortower" / "pyproject.toml").read_text(),
                "base\n",
            )
            self.assertIn("skipping duplicate tool websearch", stderr.getvalue())
            self.assertEqual(
                (target / "research" / "websearch" / "pyproject.toml").read_text(),
                "old project\n",
            )
            self.assertEqual((target / "research" / "websearch" / "old.py").read_text(), "old\n")
            self.assertFalse((target / "research" / "websearch" / "new.py").exists())
            self.assertEqual((target / "research" / "company" / "pyproject.toml").read_text(), "company\n")

    def test_tool_allowlist_restricts_installed_tools(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            published = root / "published"
            target = root / "target"

            for category, name in (("research", "websearch"), ("productivity", "linear")):
                (published / category / name).mkdir(parents=True)
                (published / category / name / "pyproject.toml").write_text(f"{name}\n")

            with mock.patch.dict("os.environ", {"TOOL_ALLOWLIST": "websearch,posthog"}):
                install_tool_shims._copy_published_tools(target, published)

            # Allowlisted tool installed; unconfigured tool skipped.
            self.assertTrue((target / "research" / "websearch" / "pyproject.toml").exists())
            self.assertFalse((target / "productivity" / "linear").exists())

    def test_unset_allowlist_installs_all_tools(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            published = root / "published"
            target = root / "target"

            for category, name in (("research", "websearch"), ("productivity", "linear")):
                (published / category / name).mkdir(parents=True)
                (published / category / name / "pyproject.toml").write_text(f"{name}\n")

            with mock.patch.dict("os.environ", {"TOOL_ALLOWLIST": ""}):
                install_tool_shims._copy_published_tools(target, published)

            self.assertTrue((target / "research" / "websearch" / "pyproject.toml").exists())
            self.assertTrue((target / "productivity" / "linear" / "pyproject.toml").exists())

    def test_tool_blocklist_skips_published_tools(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            published = root / "published"
            target = root / "target"

            for category, name in (("infra", "vlogs"), ("infra", "vmetrics"), ("research", "websearch")):
                (published / category / name).mkdir(parents=True)
                (published / category / name / "pyproject.toml").write_text(f"{name}\n")

            with mock.patch.dict("os.environ", {"TOOL_BLOCKLIST": "vlogs,vmetrics"}):
                install_tool_shims._copy_published_tools(target, published)

            self.assertFalse((target / "infra" / "vlogs").exists())
            self.assertFalse((target / "infra" / "vmetrics").exists())
            self.assertTrue((target / "research" / "websearch" / "pyproject.toml").exists())

    def test_discover_scripts_respects_allowlist(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "tools"
            for category, name in (("research", "websearch"), ("productivity", "linear")):
                d = root / category / name
                d.mkdir(parents=True)
                (d / "pyproject.toml").write_text(
                    f'[project]\nname = "{name}"\n\n[project.scripts]\n{name} = "client:main"\n'
                )

            with mock.patch.dict("os.environ", {"TOOL_ALLOWLIST": "websearch,posthog"}):
                scripts = install_tool_shims._discover_scripts([root])
            self.assertIn("websearch", scripts)
            self.assertNotIn("linear", scripts)

            with mock.patch.dict("os.environ", {"TOOL_ALLOWLIST": ""}):
                scripts_all = install_tool_shims._discover_scripts([root])
            self.assertIn("websearch", scripts_all)
            self.assertIn("linear", scripts_all)

    def test_discover_scripts_respects_blocklist(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "tools"
            tools = [
                ("infra", "vlogs", "vlogs", "vlogs"),
                ("infra", "centaur_investigator", "centaur_investigator", "centaur-investigator"),
                ("research", "websearch", "websearch", "websearch"),
            ]
            for category, dirname, project, script in tools:
                d = root / category / dirname
                d.mkdir(parents=True)
                (d / "pyproject.toml").write_text(
                    f'[project]\nname = "{project}"\n\n[project.scripts]\n{script} = "client:main"\n'
                )

            with mock.patch.dict(
                "os.environ",
                {"TOOL_BLOCKLIST": "vlogs,centaur_investigator,centaur-investigator"},
            ):
                scripts = install_tool_shims._discover_scripts([root])

            self.assertNotIn("vlogs", scripts)
            self.assertNotIn("centaur-investigator", scripts)
            self.assertIn("websearch", scripts)


class GeneratedShimTest(unittest.TestCase):
    def test_tool_shim_delegates_to_centaur_tools_exec(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            bin_dir = Path(tmp)
            script = {
                "name": "websearch",
                "project_dir": "/app/tools/research/websearch",
                "package": "websearch",
                "entrypoint": "websearch.cli:app",
                "client_module": "client.py",
            }

            install_tool_shims._write_tool_shim(bin_dir / "websearch", script, "/opt/centaur")

            content = (bin_dir / "websearch").read_text()
            self.assertIn(f"exec {bin_dir / 'centaur-tools'} run websearch", content)
            self.assertNotIn("uvx --from", content)
            self.assertNotIn("/app/tools/research/websearch", content)

    def test_centaur_tools_run_uses_catalog_entry_directly(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            bin_dir = root / "bin"
            fake_bin = root / "fake-bin"
            project_dir = root / "tools" / "research" / "websearch"
            bin_dir.mkdir()
            fake_bin.mkdir()
            project_dir.mkdir(parents=True)

            index_path = bin_dir / ".centaur-tools.json"
            index_path.write_text(
                json.dumps(
                    [
                        {
                            "name": "websearch",
                            "project_dir": str(project_dir),
                            "package": "websearch",
                            "entrypoint": "websearch.cli:app",
                            "client_module": "client.py",
                        }
                    ]
                )
                + "\n"
            )
            install_tool_shims._write_catalog(
                bin_dir / "centaur-tools",
                index_path,
                os.pathsep.join(["/opt/centaur", "/opt/extra"]),
            )

            uvx_log = root / "uvx.log"
            pythonpath_log = root / "pythonpath.log"
            analytics_log = root / "tool-analytics.log"
            fake_uvx = fake_bin / "uvx"
            fake_uvx.write_text(
                "#!/usr/bin/env python3\n"
                "from pathlib import Path\n"
                "import os\n"
                "import sys\n"
                "Path(os.environ['UVX_LOG']).write_text('\\n'.join(sys.argv[1:]) + '\\n')\n"
                "Path(os.environ['PYTHONPATH_LOG']).write_text(os.environ.get('PYTHONPATH', ''))\n"
            )
            fake_uvx.chmod(0o755)

            path_websearch = fake_bin / "websearch"
            path_websearch.write_text("#!/bin/sh\nexit 42\n")
            path_websearch.chmod(0o755)

            env = os.environ.copy()
            env["PATH"] = f"{fake_bin}{os.pathsep}{env.get('PATH', '')}"
            env["UVX_LOG"] = str(uvx_log)
            env["PYTHONPATH_LOG"] = str(pythonpath_log)
            env["PYTHONPATH"] = "existing"
            env["CENTAUR_THREAD_KEY"] = "cli:test-thread"
            env["CENTAUR_TOOL_ANALYTICS_LOG_PATH"] = str(analytics_log)

            result = subprocess.run(
                [
                    str(bin_dir / "centaur-tools"),
                    "run",
                    "websearch",
                    "lookup",
                    "sensitive-payload",
                ],
                check=False,
                env=env,
                text=True,
                capture_output=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                uvx_log.read_text().splitlines(),
                [
                    "--from",
                    str(project_dir),
                    "websearch",
                    "lookup",
                    "sensitive-payload",
                ],
            )
            self.assertEqual(
                pythonpath_log.read_text(),
                f"/opt/centaur{os.pathsep}/opt/extra{os.pathsep}existing",
            )
            analytics_events = [
                json.loads(line) for line in analytics_log.read_text().splitlines()
            ]
            self.assertEqual(
                [event["event"] for event in analytics_events],
                ["tool_call_started", "tool_call_completed"],
            )
            for event in analytics_events:
                self.assertEqual(event["service"], "sandbox")
                self.assertEqual(event["component"], "tool_shim")
                self.assertEqual(event["tool_name"], "websearch")
                self.assertEqual(event["tool_method"], "cli")
                self.assertEqual(event["thread_key"], "cli:test-thread")
            self.assertEqual(analytics_events[1]["exit_code"], 0)
            self.assertEqual(analytics_events[1]["success"], "true")
            self.assertIn("duration_ms", analytics_events[1])
            serialized_analytics = json.dumps(analytics_events, sort_keys=True)
            self.assertNotIn("lookup", serialized_analytics)
            self.assertNotIn("sensitive-payload", serialized_analytics)

            result = subprocess.run(
                [str(bin_dir / "centaur-tools"), "exec", "websearch"],
                check=False,
                env=env,
                text=True,
                capture_output=True,
            )

            self.assertEqual(result.returncode, 2)
            self.assertIn("usage: centaur-tools", result.stderr)


if __name__ == "__main__":
    unittest.main()
