from __future__ import annotations

import contextlib
import io
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


if __name__ == "__main__":
    unittest.main()
