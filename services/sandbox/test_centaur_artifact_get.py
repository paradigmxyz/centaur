from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import centaur_artifact_get


class ArtifactGetTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root = Path(self.temp_dir.name)

    def tearDown(self) -> None:
        self.temp_dir.cleanup()

    def test_reads_file(self) -> None:
        downloads = self.root / "downloads"
        downloads.mkdir()
        (downloads / "report.txt").write_bytes(b"sandbox report\n")

        with mock.patch.object(centaur_artifact_get, "DOWNLOADS_ROOT", downloads):
            self.assertEqual(
                centaur_artifact_get.read_artifact("report.txt"),
                b"sandbox report\n",
            )

    def test_rejects_paths_outside_downloads_root(self) -> None:
        downloads = self.root / "restricted-downloads"
        downloads.mkdir()
        (self.root / "secret.txt").write_text("secret")

        with mock.patch.object(centaur_artifact_get, "DOWNLOADS_ROOT", downloads):
            for artifact_path in ["../secret.txt", "/etc/passwd"]:
                with self.subTest(path=artifact_path):
                    with self.assertRaisesRegex(ValueError, "relative"):
                        centaur_artifact_get.read_artifact(artifact_path)

    def test_reads_nested_paths_and_in_root_symlinks(self) -> None:
        downloads = self.root / "nested-downloads"
        reports = downloads / "reports"
        reports.mkdir(parents=True)
        (reports / "quarterly.txt").write_text("quarterly")
        (downloads / "latest.txt").symlink_to(reports / "quarterly.txt")

        with mock.patch.object(centaur_artifact_get, "DOWNLOADS_ROOT", downloads):
            self.assertEqual(
                centaur_artifact_get.read_artifact("reports/quarterly.txt"),
                b"quarterly",
            )
            self.assertEqual(
                centaur_artifact_get.read_artifact("latest.txt"), b"quarterly"
            )

    def test_rejects_symlinks_outside_downloads_root(self) -> None:
        downloads = self.root / "symlink-downloads"
        downloads.mkdir()
        secret = self.root / "secret.txt"
        secret.write_text("secret")
        (downloads / "report.txt").symlink_to(secret)

        with mock.patch.object(centaur_artifact_get, "DOWNLOADS_ROOT", downloads):
            with self.assertRaisesRegex(ValueError, "resolves outside"):
                centaur_artifact_get.read_artifact("report.txt")

    def test_reads_from_validated_descriptor(self) -> None:
        downloads = self.root / "descriptor-downloads"
        downloads.mkdir()
        report = downloads / "report.txt"
        report.write_text("report")
        secret = self.root / "secret.txt"
        secret.write_text("secret")

        with mock.patch.object(centaur_artifact_get, "DOWNLOADS_ROOT", downloads):
            file_fd = centaur_artifact_get._open_artifact("report.txt")
            report.unlink()
            report.symlink_to(secret)
            with os.fdopen(file_fd, "rb") as file:
                self.assertEqual(file.read(), b"report")

    def test_rejects_symlinked_downloads_root(self) -> None:
        real_downloads = self.root / "real-downloads"
        real_downloads.mkdir()
        (real_downloads / "report.txt").write_text("report")
        downloads_link = self.root / "downloads-link"
        downloads_link.symlink_to(real_downloads, target_is_directory=True)

        with mock.patch.object(
            centaur_artifact_get, "DOWNLOADS_ROOT", downloads_link
        ):
            with self.assertRaises(OSError):
                centaur_artifact_get.read_artifact("report.txt")

    def test_rejects_oversized_files(self) -> None:
        downloads = self.root / "size-limited-downloads"
        downloads.mkdir()
        (downloads / "large.bin").write_bytes(b"12345")

        with (
            mock.patch.object(centaur_artifact_get, "DOWNLOADS_ROOT", downloads),
            mock.patch.object(centaur_artifact_get, "MAX_DOWNLOAD_BYTES", 4),
        ):
            with self.assertRaisesRegex(ValueError, "size limit"):
                centaur_artifact_get.read_artifact("large.bin")


if __name__ == "__main__":
    unittest.main()
