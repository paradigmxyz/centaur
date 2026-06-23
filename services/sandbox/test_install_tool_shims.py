from __future__ import annotations

import contextlib
import io
import tempfile
import unittest
from pathlib import Path

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


if __name__ == "__main__":
    unittest.main()
