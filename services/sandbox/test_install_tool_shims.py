from __future__ import annotations

import contextlib
import io
import json
import os
import subprocess
import sys
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
                ("infra", "infra_probe", "infra_probe", "infra-probe"),
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
                {"TOOL_BLOCKLIST": "vlogs,infra_probe,infra-probe"},
            ):
                scripts = install_tool_shims._discover_scripts([root])

            self.assertNotIn("vlogs", scripts)
            self.assertNotIn("infra-probe", scripts)
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

    def test_centaur_tools_list_emits_analytics_events(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            index_path = bin_dir / ".centaur-tools.json"
            index_path.write_text(
                json.dumps(
                    [
                        {
                            "name": "websearch",
                            "project_dir": "/app/tools/research/websearch",
                        }
                    ]
                )
                + "\n"
            )
            install_tool_shims._write_catalog(
                bin_dir / "centaur-tools", index_path, ""
            )
            analytics_log = root / "tool-analytics.log"
            env = {
                **os.environ,
                "CENTAUR_THREAD_KEY": "cli:test-thread",
                "CENTAUR_TOOL_ANALYTICS_LOG_PATH": str(analytics_log),
            }

            result = subprocess.run(
                [str(bin_dir / "centaur-tools"), "list"],
                check=False,
                env=env,
                text=True,
                capture_output=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                result.stdout, "websearch\t/app/tools/research/websearch\n"
            )
            analytics_events = [
                json.loads(line) for line in analytics_log.read_text().splitlines()
            ]
            self.assertEqual(
                [event["event"] for event in analytics_events],
                ["tool_call_started", "tool_call_completed"],
            )
            for event in analytics_events:
                self.assertEqual(event["tool_name"], "centaur-tools")
                self.assertEqual(event["tool_method"], "list")
                self.assertEqual(event["thread_key"], "cli:test-thread")
            self.assertEqual(analytics_events[1]["exit_code"], 0)
            self.assertEqual(analytics_events[1]["success"], "true")
            self.assertIn("duration_ms", analytics_events[1])

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
                self.assertEqual(event["tool_args"], ["lookup", "sensitive-payload"])
                self.assertEqual(event["tool_args_count"], 2)
                self.assertEqual(event["thread_key"], "cli:test-thread")
            self.assertEqual(analytics_events[1]["exit_code"], 0)
            self.assertEqual(analytics_events[1]["success"], "true")
            self.assertIn("duration_ms", analytics_events[1])
            serialized_analytics = json.dumps(analytics_events, sort_keys=True)
            self.assertIn("lookup", serialized_analytics)
            self.assertIn("sensitive-payload", serialized_analytics)

            analytics_log.write_text("")
            first_arg = "a" * 400
            second_arg = "b" * 400
            result = subprocess.run(
                [
                    str(bin_dir / "centaur-tools"),
                    "run",
                    "websearch",
                    first_arg,
                    second_arg,
                ],
                check=False,
                env=env,
                text=True,
                capture_output=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            analytics_events = [
                json.loads(line) for line in analytics_log.read_text().splitlines()
            ]
            for event in analytics_events:
                self.assertEqual(event["tool_args_count"], 2)
                self.assertEqual(event["tool_args"][0], first_arg)
                self.assertEqual(event["tool_args"][1], ("b" * 109) + "...")
                self.assertEqual(sum(len(arg) for arg in event["tool_args"]), 512)
                self.assertEqual(event["tool_args_truncated"], "true")

            result = subprocess.run(
                [str(bin_dir / "centaur-tools"), "exec", "websearch"],
                check=False,
                env=env,
                text=True,
                capture_output=True,
            )

            self.assertEqual(result.returncode, 2)
            self.assertIn("usage: centaur-tools", result.stderr)

    def test_call_runner_loads_hyphenated_tool_as_normalized_package(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            bin_dir = root / "bin"
            fake_bin = root / "fake-bin"
            project_dir = root / "data-query-and-viz"
            bin_dir.mkdir()
            fake_bin.mkdir()
            project_dir.mkdir()
            (project_dir / "__init__.py").write_text("")
            (project_dir / "helper.py").write_text('VALUE = "package-loaded"\n')
            (project_dir / "client.py").write_text(
                "from .helper import VALUE\n"
                "\n"
                "class Client:\n"
                "    def ping(self, suffix):\n"
                "        return f'{VALUE}:{suffix}'\n"
                "\n"
                "def _client():\n"
                "    return Client()\n"
            )

            index_path = bin_dir / ".centaur-tools.json"
            index_path.write_text(
                json.dumps(
                    [
                        {
                            "name": "data-query-and-viz",
                            "project_dir": str(project_dir),
                            "package": "data-query-and-viz",
                            "entrypoint": "cli:app",
                            "client_module": "client.py",
                        }
                    ]
                )
                + "\n"
            )
            install_tool_shims._write_catalog(
                bin_dir / "centaur-tools",
                index_path,
                str(Path(__file__).resolve().parents[2]),
            )

            fake_uvx = fake_bin / "uvx"
            fake_uvx.write_text(
                f"#!{sys.executable}\n"
                "import subprocess\n"
                "import sys\n"
                "\n"
                "args = sys.argv[1:]\n"
                "if len(args) < 3 or args[0] != '--from' or args[2] != 'python':\n"
                "    raise SystemExit(2)\n"
                "raise SystemExit(subprocess.call([sys.executable, *args[3:]]))\n"
            )
            fake_uvx.chmod(0o755)

            env = os.environ.copy()
            env["PATH"] = f"{fake_bin}{os.pathsep}{env.get('PATH', '')}"
            env["CENTAUR_TOOL_ANALYTICS_LOG_PATH"] = "off"
            result = subprocess.run(
                [
                    str(bin_dir / "centaur-tools"),
                    "call",
                    "data-query-and-viz",
                    "ping",
                    json.dumps({"suffix": "ok"}),
                ],
                check=False,
                env=env,
                text=True,
                capture_output=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout), "package-loaded:ok")


class RefreshInstallTest(unittest.TestCase):
    def test_install_removes_stale_generated_shims(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            tool_dir = root / "tools"
            bin_dir = root / "bin"
            package_dir = tool_dir / "research" / "websearch"
            package_dir.mkdir(parents=True)
            bin_dir.mkdir()
            (package_dir / "pyproject.toml").write_text(
                '[project]\nname = "websearch"\n\n[project.scripts]\nwebsearch = "client:main"\n'
            )
            (bin_dir / ".centaur-tools.json").write_text(
                json.dumps(
                    [
                        {
                            "name": "websearch",
                            "project_dir": "/old/websearch",
                            "package": "websearch",
                            "entrypoint": "client:main",
                            "client_module": "client.py",
                        },
                        {
                            "name": "gone",
                            "project_dir": "/old/gone",
                            "package": "gone",
                            "entrypoint": "client:main",
                            "client_module": "client.py",
                        },
                    ]
                )
                + "\n"
            )
            (bin_dir / "gone").write_text(
                "#!/bin/sh\n# generated by install-tool-shims\n"
            )

            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                install_tool_shims._install_tool_shims(
                    [tool_dir], bin_dir, refresh=False
                )

            self.assertTrue((bin_dir / "websearch").exists())
            self.assertFalse((bin_dir / "gone").exists())
            index = json.loads((bin_dir / ".centaur-tools.json").read_text())
            self.assertEqual([tool["name"] for tool in index], ["websearch"])
            self.assertEqual(
                index[0]["env_key"], install_tool_shims.tool_env_key(package_dir)
            )


def _write_tool_project(project_dir: Path) -> None:
    (project_dir / "pkg").mkdir(parents=True)
    (project_dir / "pyproject.toml").write_text('[project]\nname = "websearch"\n')
    (project_dir / "pkg" / "cli.py").write_text("print('hi')\n")


class ToolEnvKeyTest(unittest.TestCase):
    def test_key_matches_verbatim_copy_and_ignores_bytecode(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "checkout" / "websearch"
            _write_tool_project(source)
            (source / "link.txt").symlink_to("pkg/cli.py")
            key = install_tool_shims.tool_env_key(source)

            copy = root / "app" / "websearch"
            import shutil

            shutil.copytree(source, copy, symlinks=True)
            (copy / "pkg" / "__pycache__").mkdir()
            (copy / "pkg" / "__pycache__" / "cli.cpython-312.pyc").write_bytes(b"x")
            (copy / "stray.pyc").write_bytes(b"x")
            (copy / "websearch.egg-info").mkdir()

            self.assertEqual(install_tool_shims.tool_env_key(copy), key)

    def test_key_changes_with_content_and_interpreter_only(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp) / "websearch"
            _write_tool_project(project)
            key = install_tool_shims.tool_env_key(project)

            self.assertNotEqual(
                install_tool_shims.tool_env_key(project, "cpython-3.13.0-x86_64"), key
            )
            # Modes and mtimes differ between checkout and copy; they don't count.
            (project / "pkg" / "cli.py").chmod(0o744)
            os.utime(project / "pkg" / "cli.py", (0, 0))
            self.assertEqual(install_tool_shims.tool_env_key(project), key)
            (project / "pkg" / "cli.py").write_text("print('changed')\n")
            self.assertNotEqual(install_tool_shims.tool_env_key(project), key)
            (project / "pkg" / "cli.py").write_text("print('hi')\n")
            (project / "pkg" / "cli.py").rename(project / "pkg" / "main.py")
            self.assertNotEqual(install_tool_shims.tool_env_key(project), key)


def _publish_fake_env(env_root: Path, key: str, *, marker_key: str | None = None) -> Path:
    """Lay out a prebuilt env the way tool-env-builder publishes one."""
    build = env_root / "builds" / f"{key}-1"
    venv_bin = build / "venv" / "bin"
    venv_bin.mkdir(parents=True)
    (venv_bin / "python").symlink_to(sys.executable)
    (build / install_tool_shims.TOOL_ENV_MARKER_NAME).write_text(
        json.dumps({"schema": install_tool_shims.TOOL_ENV_SCHEMA, "key": marker_key or key})
    )
    (env_root / key).symlink_to(f"builds/{key}-1")
    return build / "venv"


class PrebuiltEnvCatalogTest(unittest.TestCase):
    def _catalog(self, root: Path, project_dir: Path, key: str) -> Path:
        bin_dir = root / "bin"
        bin_dir.mkdir()
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
                        "env_key": key,
                    }
                ]
            )
        )
        install_tool_shims._write_catalog(bin_dir / "centaur-tools", index_path, "/opt/centaur")
        return bin_dir / "centaur-tools"

    def _env(self, root: Path, env_dirs: str) -> dict[str, str]:
        fake_bin = root / "fake-bin"
        fake_bin.mkdir(exist_ok=True)
        fake_uvx = fake_bin / "uvx"
        fake_uvx.write_text(
            "#!/bin/sh\nprintf 'uvx %s\\n' \"$*\" > \"$RUN_LOG\"\n"
        )
        fake_uvx.chmod(0o755)
        env = os.environ.copy()
        env.pop("PYTHONPATH", None)
        env["PATH"] = f"{fake_bin}{os.pathsep}{env.get('PATH', '')}"
        env["RUN_LOG"] = str(root / "run.log")
        env["CENTAUR_TOOL_ENV_DIRS"] = env_dirs
        env["CENTAUR_TOOL_ANALYTICS_LOG_PATH"] = str(root / "analytics.log")
        return env

    def test_run_uses_matching_prebuilt_env(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            env_root = root / "cache" / "private" / ".centaur-tool-envs"
            env_root.mkdir(parents=True)
            venv = _publish_fake_env(env_root, "k1")
            script = venv / "bin" / "websearch"
            script.write_text(
                "#!/bin/sh\n"
                "printf 'prebuilt %s %s %s\\n' \"$*\" \"$VIRTUAL_ENV\" \"$PYTHONPATH\" > \"$RUN_LOG\"\n"
            )
            script.chmod(0o755)
            catalog = self._catalog(root, root / "tools" / "websearch", "k1")
            env = self._env(root, f"{root / 'missing'}:{env_root}")

            result = subprocess.run(
                [str(catalog), "run", "websearch", "--help"],
                env=env,
                text=True,
                capture_output=True,
                check=False,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                (root / "run.log").read_text(),
                f"prebuilt --help {venv.resolve()} /opt/centaur\n",
            )
            events = [
                json.loads(line) for line in (root / "analytics.log").read_text().splitlines()
            ]
            self.assertEqual([event["tool_env"] for event in events], ["prebuilt", "prebuilt"])

    def test_run_falls_back_to_uvx_without_matching_env(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            env_root = root / "envs"
            env_root.mkdir()
            # Marker for a different key, and an env missing the tool script.
            _publish_fake_env(env_root, "stale", marker_key="other")
            _publish_fake_env(env_root, "noscript")
            project_dir = root / "tools" / "websearch"

            for key in ["absent", "stale", "noscript"]:
                with self.subTest(key=key):
                    (root / "analytics.log").unlink(missing_ok=True)
                    catalog_root = root / key
                    catalog_root.mkdir()
                    catalog = self._catalog(catalog_root, project_dir, key)
                    result = subprocess.run(
                        [str(catalog), "run", "websearch", "--help"],
                        env=self._env(root, str(env_root)),
                        text=True,
                        capture_output=True,
                        check=False,
                    )
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(
                        (root / "run.log").read_text(),
                        f"uvx --from {project_dir} websearch --help\n",
                    )
                    events = [
                        json.loads(line)
                        for line in (root / "analytics.log").read_text().splitlines()
                    ]
                    self.assertEqual(events[-1]["tool_env"], "uvx")

    def test_call_uses_prebuilt_python(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            project_dir = root / "tools" / "pinger"
            project_dir.mkdir(parents=True)
            (project_dir / "client.py").write_text(
                "import os, sys\n"
                "class Client:\n"
                "    def ping(self):\n"
                "        return [os.environ['VIRTUAL_ENV'], sys.executable]\n"
                "def _client():\n"
                "    return Client()\n"
            )
            env_root = root / "envs"
            env_root.mkdir()
            venv = _publish_fake_env(env_root, "k1")
            catalog = self._catalog(root, project_dir, "k1")
            env = self._env(root, str(env_root))
            env["CENTAUR_TOOL_ANALYTICS_LOG_PATH"] = "off"

            result = subprocess.run(
                [str(catalog), "call", "websearch", "ping", "{}"],
                env=env,
                text=True,
                capture_output=True,
                check=False,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            virtual_env, executable = json.loads(result.stdout)
            self.assertEqual(virtual_env, str(venv.resolve()))
            self.assertEqual(executable, str(venv.resolve() / "bin" / "python"))
            self.assertFalse((root / "run.log").exists())


if __name__ == "__main__":
    unittest.main()
