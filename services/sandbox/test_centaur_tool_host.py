from __future__ import annotations

import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest import mock

import centaur_tool_host
import install_tool_shims


class ToolHostTest(unittest.TestCase):
    """Exercise the host, generated catalog, and a real installed CLI together."""

    @classmethod
    def setUpClass(cls) -> None:
        cls.temp = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.temp.cleanup)
        cls.root = Path(cls.temp.name)
        cls.bin_dir = cls.root / "bin"
        cls.bin_dir.mkdir()
        project = cls.root / "demo"
        package = project / "demo"
        package.mkdir(parents=True)
        (package / "__init__.py").write_text("")
        (project / "pyproject.toml").write_text(
            '[project]\nname = "centaur-host-test-demo"\nversion = "0.0.1"\n'
            '[project.scripts]\ndemo = "demo.cli:main"\n'
            '[build-system]\nrequires = ["hatchling"]\n'
            'build-backend = "hatchling.build"\n'
            '[tool.hatch.build.targets.wheel]\npackages = ["demo"]\n'
        )
        (package / "cli.py").write_text(
            "import json, os, sys, time\n"
            "def main():\n"
            "    args = sys.argv[1:]\n"
            "    if args == ['--help']:\n"
            "        print('Usage: demo [ARGS]')\n"
            "    elif args == ['fail']:\n"
            "        print('partial output')\n"
            "        print('invalid arguments', file=sys.stderr)\n"
            "        return 2\n"
            "    elif args == ['sleep']:\n"
            "        print('started', flush=True)\n"
            "        time.sleep(2)\n"
            "    else:\n"
            "        print(json.dumps({'argv': args, 'principal': os.environ.get('CENTAUR_MCP_PRINCIPAL_ID')}))\n"
        )
        index = cls.bin_dir / ".centaur-tools.json"
        index.write_text(
            json.dumps(
                [
                    {
                        "name": "demo",
                        "project_dir": str(project),
                        "package": "centaur-host-test-demo",
                        "entrypoint": "demo.cli:main",
                        "client_module": "client.py",
                    }
                ]
            )
        )
        install_tool_shims._write_catalog(
            cls.bin_dir / "centaur-tools",
            index,
            "",
        )
        cls.env = {
            **os.environ,
            "PATH": f"{cls.bin_dir}{os.pathsep}{os.environ.get('PATH', '')}",
            "CENTAUR_TOOL_ANALYTICS_LOG_PATH": str(cls.root / "analytics.jsonl"),
        }
        # Install once before exercising the host's execution timeout.
        install = subprocess.run(
            [str(cls.bin_dir / "centaur-tools"), "run", "demo", "--help"],
            env=cls.env,
            check=False,
            capture_output=True,
            text=True,
            timeout=60,
        )
        if install.returncode != 0:
            raise RuntimeError(
                f"could not install test CLI:\n{install.stdout}{install.stderr}"
            )

    def requests(self, *requests: dict) -> list[dict]:
        requests = tuple(
            {**request, "id": f"test-{index}"} for index, request in enumerate(requests)
        )
        result = subprocess.run(
            [sys.executable, str(Path(__file__).with_name("centaur_tool_host.py"))],
            input="".join(json.dumps(request) + "\n" for request in requests),
            env=self.env,
            check=True,
            capture_output=True,
            text=True,
            timeout=15,
        )
        ready, *events = result.stdout.splitlines()
        self.assertEqual(ready, "__CENTAUR_TOOL_HOST_READY")
        self.assertEqual(len(events), len(requests))
        responses = []
        for request, event in zip(requests, events, strict=True):
            envelope = json.loads(event)
            self.assertEqual(envelope["type"], "result")
            self.assertEqual(envelope["turn_id"], request["id"])
            response = json.loads(envelope["result"])
            self.assertEqual(response["id"], request["id"])
            responses.append(response)
        return responses

    def run_request(self, argv: list[str], **kwargs) -> dict:
        return {"id": "test", "mode": "v2", "tool": "demo", "argv": argv, **kwargs}

    def test_cli_preserves_literal_arguments_and_principal(self) -> None:
        sentinel = self.root / "shell-ran"
        argv = [
            "search",
            "  café messages  ",
            "",
            "--limit",
            "2",
            f"$(touch {sentinel})",
            ";",
            "|",
            ">",
        ]
        (response,) = self.requests(self.run_request(argv, principal_id="prn_test"))
        self.assertEqual(response["status"], 0, response["stderr"])
        self.assertEqual(
            json.loads(response["stdout"]), {"argv": argv, "principal": "prn_test"}
        )
        self.assertFalse(response["timed_out"])
        self.assertFalse(sentinel.exists())

    def test_cli_help_no_args_failure_and_recovery(self) -> None:
        help_result, empty, failure, recovery = self.requests(
            self.run_request(["--help"]),
            self.run_request([]),
            self.run_request(["fail"]),
            self.run_request(["--help"]),
        )
        self.assertEqual(help_result["status"], 0)
        self.assertEqual(help_result["stdout"], "Usage: demo [ARGS]\n")
        self.assertEqual(json.loads(empty["stdout"])["argv"], [])
        self.assertEqual(failure["status"], 2)
        self.assertEqual(failure["stdout"], "partial output\n")
        self.assertEqual(failure["stderr"], "invalid arguments\n")
        self.assertFalse(failure["timed_out"])
        self.assertEqual(recovery["stdout"], help_result["stdout"])

    def test_cli_timeout_preserves_partial_output(self) -> None:
        (response,) = self.requests(self.run_request(["sleep"], timeout_seconds=1))
        self.assertTrue(response["timed_out"])
        self.assertIsNone(response["status"])
        self.assertIn("started\n", response["stdout"])
        self.assertIn("timed out after 1s", response["stderr"])

    def test_catalog_rejects_unknown_tools_even_when_executable_exists(self) -> None:
        (response,) = self.requests(self.run_request([], tool="python3"))
        self.assertNotEqual(response["status"], 0)
        self.assertIn("unknown tool: python3", response["stderr"])

    def test_invalid_requests_fail_and_host_accepts_next_request(self) -> None:
        for invalid in [
            self.run_request("--help"),
            self.run_request([1]),
            self.run_request(["nul\0byte"]),
            self.run_request([], mode="unknown"),
        ]:
            with self.subTest(request=invalid):
                failure, recovery = self.requests(invalid, self.run_request(["--help"]))
                self.assertEqual(failure["status"], 1)
                self.assertEqual(recovery["status"], 0)

    def test_download_file_reads_contents(self) -> None:
        downloads = self.root / "downloads"
        downloads.mkdir()
        (downloads / "report.txt").write_bytes(b"sandbox report\n")

        with mock.patch.object(centaur_tool_host, "DOWNLOADS_ROOT", downloads):
            response = centaur_tool_host._run_tool(
                {
                    "id": "download-1",
                    "mode": "download_file",
                    "path": "report.txt",
                }
            )
        self.assertEqual(response["id"], "download-1")
        self.assertEqual(response["status"], 0)
        self.assertEqual(response["path"], "report.txt")
        self.assertEqual(response["size_bytes"], len(b"sandbox report\n"))
        self.assertEqual(
            centaur_tool_host.base64.b64decode(response["data_base64"]),
            b"sandbox report\n",
        )

        output = io.StringIO()
        with redirect_stdout(output):
            centaur_tool_host._emit_result(response, transient=True)
        envelope = json.loads(output.getvalue())
        self.assertEqual(envelope["type"], "centaur.transient_result")
        self.assertEqual(envelope["turn_id"], "download-1")
        self.assertEqual(json.loads(envelope["result"]), response)

    def test_download_file_rejects_paths_outside_downloads_root(self) -> None:
        downloads = self.root / "restricted-downloads"
        downloads.mkdir()
        (self.root / "secret.txt").write_text("secret")

        with mock.patch.object(centaur_tool_host, "DOWNLOADS_ROOT", downloads):
            for artifact_path in ["../secret.txt", "/etc/passwd"]:
                with self.subTest(path=artifact_path):
                    with self.assertRaisesRegex(ValueError, "relative"):
                        centaur_tool_host._read_download_file(artifact_path)

    def test_download_file_reads_nested_paths_and_in_root_symlinks(self) -> None:
        downloads = self.root / "nested-downloads"
        reports = downloads / "reports"
        reports.mkdir(parents=True)
        (reports / "quarterly.txt").write_text("quarterly")
        (downloads / "latest.txt").symlink_to(reports / "quarterly.txt")

        with mock.patch.object(centaur_tool_host, "DOWNLOADS_ROOT", downloads):
            self.assertEqual(
                centaur_tool_host._read_download_file("reports/quarterly.txt"),
                b"quarterly",
            )
            self.assertEqual(
                centaur_tool_host._read_download_file("latest.txt"), b"quarterly"
            )

    def test_download_file_rejects_symlinks_outside_downloads_root(self) -> None:
        downloads = self.root / "symlink-downloads"
        downloads.mkdir()
        secret = self.root / "secret.txt"
        secret.write_text("secret")
        (downloads / "report.txt").symlink_to(secret)

        with mock.patch.object(centaur_tool_host, "DOWNLOADS_ROOT", downloads):
            with self.assertRaisesRegex(ValueError, "resolves outside"):
                centaur_tool_host._read_download_file("report.txt")

    def test_download_file_reads_from_the_validated_descriptor(self) -> None:
        downloads = self.root / "descriptor-downloads"
        downloads.mkdir()
        report = downloads / "report.txt"
        report.write_text("report")
        secret = self.root / "secret.txt"
        secret.write_text("secret")

        with mock.patch.object(centaur_tool_host, "DOWNLOADS_ROOT", downloads):
            file_fd, _ = centaur_tool_host._open_download_file("report.txt")
            report.unlink()
            report.symlink_to(secret)
            with os.fdopen(file_fd, "rb") as file:
                self.assertEqual(file.read(), b"report")

    def test_download_file_rejects_symlinked_downloads_root(self) -> None:
        real_downloads = self.root / "real-downloads"
        real_downloads.mkdir()
        (real_downloads / "report.txt").write_text("report")
        downloads_link = self.root / "downloads-link"
        downloads_link.symlink_to(real_downloads, target_is_directory=True)

        with mock.patch.object(
            centaur_tool_host, "DOWNLOADS_ROOT", downloads_link
        ):
            with self.assertRaises(OSError):
                centaur_tool_host._read_download_file("report.txt")

    def test_download_file_rejects_oversized_files(self) -> None:
        downloads = self.root / "size-limited-downloads"
        downloads.mkdir()
        (downloads / "large.bin").write_bytes(b"12345")

        with (
            mock.patch.object(centaur_tool_host, "DOWNLOADS_ROOT", downloads),
            mock.patch.object(centaur_tool_host, "MAX_DOWNLOAD_BYTES", 4),
        ):
            with self.assertRaisesRegex(ValueError, "size limit"):
                centaur_tool_host._read_download_file("large.bin")


if __name__ == "__main__":
    unittest.main()
