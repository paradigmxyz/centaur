from __future__ import annotations

import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

import install_tool_shims
import tool_env_builder

# Stands in for uv: `venv` lays out bin/python, `pip install` records the
# staged source and adds the project's script, `cache prune` is logged.
FAKE_UV = f"""#!{sys.executable}
import os, sys
from pathlib import Path
args = sys.argv[1:]
log = os.environ.get("FAKE_UV_LOG")
if log:
    with open(log, "a") as f:
        f.write(" ".join(args) + "\\n")
if os.environ.get("FAKE_UV_FAIL") and args[:2] == ["pip", "install"]:
    print("resolution failed")
    raise SystemExit(1)
if args[0] == "venv":
    venv = Path(args[-1])
    (venv / "bin").mkdir(parents=True)
    (venv / "bin" / "python").symlink_to(sys.executable)
elif args[:2] == ["pip", "install"]:
    venv = Path(args[args.index("--python") + 1]).parent.parent
    source = Path(args[-1])
    (venv / "installed-from").write_text((source / "cli.py").read_text())
    site = venv / "lib" / "python3.12" / "site-packages"
    site.mkdir(parents=True)
    shared = site / "shared_dep.py"
    shared.write_text("VALUE = 1\\n")
    os.utime(shared, (1_700_000_000, 1_700_000_000))
    (site / (source.name + "_only.py")).write_text("NAME = 1\\n")
    (venv / "bin" / source.name).write_text("#!/bin/sh\\n")
"""


class BuilderTestCase(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.cache = self.root / "cache"
        fake_uv = self.root / "uv"
        fake_uv.write_text(FAKE_UV)
        fake_uv.chmod(0o755)
        self.uv_log = self.root / "uv.log"
        env = mock.patch.dict(
            os.environ,
            {"FAKE_UV_LOG": str(self.uv_log)},
        )
        env.start()
        self.addCleanup(env.stop)
        for name in ["TOOL_ALLOWLIST", "TOOL_BLOCKLIST", "UV_CACHE_DIR"]:
            os.environ.pop(name, None)
        self.clock = 1_000_000.0
        clock = mock.patch.object(tool_env_builder, "_now", lambda: self.clock)
        clock.start()
        self.addCleanup(clock.stop)
        self.fake_uv = str(fake_uv)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def builder(self, **overrides) -> tool_env_builder.ToolEnvBuilder:
        options = dict(
            cache_dir=self.cache,
            sources=[("acme/tools", "tools")],
            interval_seconds=60,
            max_age_seconds=86400,
            retention_seconds=3600,
            grace_seconds=600,
            failure_backoff_seconds=300,
            build_timeout_seconds=60,
            uv=self.fake_uv,
        )
        options.update(overrides)
        return tool_env_builder.ToolEnvBuilder(**options)

    def checkout(self, visibility: str = "private") -> Path:
        checkout = self.cache / visibility / "acme" / "tools"
        (checkout / ".git").mkdir(parents=True, exist_ok=True)
        (self.cache / visibility / tool_env_builder.TOOL_ENVS_DIR_NAME).mkdir(
            parents=True, exist_ok=True
        )
        return checkout

    def write_tool(self, checkout: Path, name: str, body: str = "v1") -> Path:
        project = checkout / "tools" / "research" / name
        project.mkdir(parents=True, exist_ok=True)
        (project / "pyproject.toml").write_text(f'[project]\nname = "{name}"\n')
        (project / "cli.py").write_text(body)
        return project

    def envs(self, visibility: str = "private") -> Path:
        return self.cache / visibility / tool_env_builder.TOOL_ENVS_DIR_NAME


class BuildTest(BuilderTestCase):
    def test_publishes_marker_and_symlink_per_key(self) -> None:
        project = self.write_tool(self.checkout(), "websearch")
        key = install_tool_shims.tool_env_key(project)

        self.assertEqual(self.builder().sync_once(), 1)

        link = self.envs() / key
        self.assertTrue(link.is_symlink())
        self.assertFalse(os.readlink(link).startswith("/"))
        build = link.resolve()
        marker = json.loads((build / install_tool_shims.TOOL_ENV_MARKER_NAME).read_text())
        self.assertEqual(marker["schema"], install_tool_shims.TOOL_ENV_SCHEMA)
        self.assertEqual(marker["key"], key)
        self.assertEqual(marker["tool"], "websearch")
        self.assertEqual((build / "venv" / "installed-from").read_text(), "v1")
        self.assertTrue((build / "venv" / "bin" / "websearch").is_file())
        self.assertFalse((build / "src").exists())
        self.assertEqual(list((self.envs() / "builds").glob(".tmp-*")), [])
        venv_call, install_call = self.uv_log.read_text().splitlines()
        self.assertIn("venv --quiet --relocatable --python", venv_call)
        self.assertIn("pip install --quiet --python", install_call)
        self.assertNotIn("--compile-bytecode", install_call)  # compiled by compileall

    def test_unchanged_tools_are_not_rebuilt(self) -> None:
        self.write_tool(self.checkout(), "websearch")
        builder = self.builder()
        builder.sync_once()
        self.uv_log.unlink()

        self.assertEqual(builder.sync_once(), 0)
        self.assertFalse(self.uv_log.exists())

    def test_changed_tool_gets_new_key_and_keeps_old_until_retention(self) -> None:
        checkout = self.checkout()
        project = self.write_tool(checkout, "websearch")
        old_key = install_tool_shims.tool_env_key(project)
        builder = self.builder()
        builder.sync_once()

        self.write_tool(checkout, "websearch", "v2")
        new_key = install_tool_shims.tool_env_key(project)
        self.assertEqual(builder.sync_once(), 1)
        self.assertTrue((self.envs() / old_key).is_symlink())
        self.assertEqual(
            ((self.envs() / new_key).resolve() / "venv" / "installed-from").read_text(), "v2"
        )

        self.clock += 3600
        # Symlink mtimes come from the real clock; age the retired one.
        os.utime(self.envs() / old_key, (0, 0), follow_symlinks=False)
        builder.sync_once()
        self.assertFalse((self.envs() / old_key).exists())
        self.assertTrue((self.envs() / new_key).is_symlink())
        self.assertIn("cache prune", self.uv_log.read_text())

    def test_max_age_rebuild_swaps_link_and_orphans_old_build_for_grace(self) -> None:
        project = self.write_tool(self.checkout(), "websearch")
        key = install_tool_shims.tool_env_key(project)
        builder = self.builder()
        builder.sync_once()
        first = (self.envs() / key).resolve()

        self.clock += 86400
        self.assertEqual(builder.sync_once(), 1)
        second = (self.envs() / key).resolve()
        self.assertNotEqual(first, second)
        self.assertTrue((first / tool_env_builder.ORPHANED_MARKER).exists())

        self.clock += 600
        os.utime(first / tool_env_builder.ORPHANED_MARKER, (0, 0))
        builder.sync_once()
        self.assertFalse(first.exists())
        self.assertTrue(second.exists())

    def test_failed_build_backs_off_and_records_output(self) -> None:
        project = self.write_tool(self.checkout(), "websearch")
        key = install_tool_shims.tool_env_key(project)
        builder = self.builder()

        with mock.patch.dict(os.environ, {"FAKE_UV_FAIL": "1"}):
            self.assertEqual(builder.sync_once(), 0)
        failure = self.envs() / tool_env_builder.FAILURES_DIR_NAME / key
        self.assertIn("resolution failed", failure.read_text())
        self.assertFalse((self.envs() / key).exists())
        self.assertEqual(list((self.envs() / "builds").iterdir()), [])

        os.utime(failure, (self.clock, self.clock))
        self.assertEqual(builder.sync_once(), 0)
        self.clock += 300
        self.assertEqual(builder.sync_once(), 1)
        self.assertFalse(failure.exists())

    def test_builds_per_visibility_and_skips_unprepared_roots(self) -> None:
        self.write_tool(self.checkout("public"), "websearch")
        private_checkout = self.cache / "private" / "acme" / "tools"
        (private_checkout / ".git").mkdir(parents=True)
        self.write_tool(private_checkout, "secret")

        self.assertEqual(self.builder().sync_once(), 1)
        self.assertEqual(len([p for p in self.envs("public").iterdir() if p.is_symlink()]), 1)
        self.assertFalse(self.envs("private").exists())

    def test_allowlist_limits_prebuilt_tools(self) -> None:
        checkout = self.checkout()
        self.write_tool(checkout, "websearch")
        self.write_tool(checkout, "gsuite")

        with mock.patch.dict(os.environ, {"TOOL_ALLOWLIST": "gsuite"}):
            builder = self.builder()
        self.assertEqual(builder.sync_once(), 1)
        published = [p.resolve() for p in self.envs().iterdir() if p.is_symlink()]
        markers = [
            json.loads((p / install_tool_shims.TOOL_ENV_MARKER_NAME).read_text())["tool"]
            for p in published
        ]
        self.assertEqual(markers, ["gsuite"])

    def test_source_changed_while_staging_is_not_published(self) -> None:
        project = self.write_tool(self.checkout(), "websearch")
        key = install_tool_shims.tool_env_key(project)
        builder = self.builder()
        original_copytree = tool_env_builder.shutil.copytree

        def copy_then_edit(source, target, **kwargs):
            result = original_copytree(source, target, **kwargs)
            (Path(target) / "cli.py").write_text("mid-build edit")
            return result

        with mock.patch.object(tool_env_builder.shutil, "copytree", copy_then_edit):
            self.assertFalse(builder.build(self.envs(), project, key))
        self.assertFalse((self.envs() / key).exists())
        self.assertEqual(list((self.envs() / "builds").iterdir()), [])


class BytecodeTest(BuilderTestCase):
    def test_identical_pyc_files_share_one_inode_across_envs(self) -> None:
        checkout = self.checkout()
        websearch = install_tool_shims.tool_env_key(self.write_tool(checkout, "websearch"))
        gsuite = install_tool_shims.tool_env_key(self.write_tool(checkout, "gsuite"))
        builder = self.builder()
        self.assertEqual(builder.sync_once(), 2)

        def site(key: str) -> Path:
            return (self.envs() / key).resolve() / "venv" / "lib" / "python3.12" / "site-packages"

        def pyc(key: str, module: str) -> Path:
            return next((site(key) / "__pycache__").glob(f"{module}.*.pyc"))

        shared_a, shared_b = pyc(websearch, "shared_dep"), pyc(gsuite, "shared_dep")
        self.assertEqual(shared_a.stat().st_ino, shared_b.stat().st_ino)
        self.assertEqual(shared_a.stat().st_nlink, 3)  # two envs + the store
        self.assertNotEqual(
            pyc(websearch, "websearch_only").stat().st_ino,
            pyc(gsuite, "gsuite_only").stat().st_ino,
        )
        # Compiled under the fixed prefix, not the per-build path.
        self.assertIn(tool_env_builder.PYC_COMPILE_PREFIX.encode(), shared_a.read_bytes())
        self.assertNotIn(str(self.cache).encode(), shared_a.read_bytes())

        # Retiring gsuite drops its links; store entries nobody uses go away.
        store = self.envs() / tool_env_builder.PYC_STORE_DIR_NAME
        gsuite_only = pyc(gsuite, "gsuite_only")
        gsuite_digest_count = sum(1 for _ in store.rglob("*") if _.is_file())
        build = (self.envs() / gsuite).resolve()
        for path in (checkout / "tools" / "research" / "gsuite").iterdir():
            path.unlink()
        (checkout / "tools" / "research" / "gsuite").rmdir()
        os.utime(self.envs() / gsuite, (0, 0), follow_symlinks=False)
        builder.sync_once()  # retires the link, marks the build orphaned
        os.utime(build / tool_env_builder.ORPHANED_MARKER, (0, 0))
        self.clock += 3600
        builder.sync_once()
        self.assertFalse(build.exists())
        self.assertFalse(gsuite_only.exists())
        self.assertLess(sum(1 for _ in store.rglob("*") if _.is_file()), gsuite_digest_count)
        self.assertEqual(pyc(websearch, "shared_dep").stat().st_nlink, 2)


class SandboxAgreementTest(BuilderTestCase):
    def test_sandbox_copy_resolves_the_env_built_from_the_checkout(self) -> None:
        checkout = self.checkout()
        project = self.write_tool(checkout, "websearch")
        (project / "cli.py").chmod(0o744)  # owner-only exec, as root owns the checkout
        (project / "data").mkdir()
        (project / "data" / "schema.json").write_text("{}")
        (project / "README.md").symlink_to("cli.py")
        (project / "pyproject.toml").write_text(
            '[project]\nname = "websearch"\n\n[project.scripts]\nwebsearch = "cli:app"\n'
        )
        self.assertEqual(self.builder().sync_once(), 1)

        # The sandbox side: tools-bootstrap's copy, then the shim index.
        app_tools = self.root / "app" / "tools"
        install_tool_shims._copy_published_tools(app_tools, checkout / "tools")
        copied = app_tools / "research" / "websearch"
        (copied / "cli.py").chmod(0o600)
        os.utime(copied / "data" / "schema.json", (0, 0))
        (copied / "__pycache__").mkdir()
        (copied / "__pycache__" / "cli.cpython-312.pyc").write_bytes(b"stale")
        scripts = install_tool_shims._discover_scripts([app_tools])

        env_key = scripts["websearch"]["env_key"]
        link = self.envs() / env_key
        self.assertTrue(link.is_symlink(), "sandbox key has no published env")
        marker = json.loads((link.resolve() / install_tool_shims.TOOL_ENV_MARKER_NAME).read_text())
        self.assertEqual(marker["key"], env_key)


class ParseTest(unittest.TestCase):
    def test_tool_sources(self) -> None:
        self.assertEqual(
            tool_env_builder._tool_sources("acme/tools=tools acme/extra=pkg/tools/ acme/bare"),
            [("acme/tools", "tools"), ("acme/extra", "pkg/tools"), ("acme/bare", "tools")],
        )


if __name__ == "__main__":
    unittest.main()
