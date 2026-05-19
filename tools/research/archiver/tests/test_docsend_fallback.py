from __future__ import annotations

import importlib
import sys
import types
import unittest
from dataclasses import dataclass
from pathlib import Path
from unittest.mock import patch


REPO_ROOT = Path(__file__).resolve().parents[4]


def _clear_archiver_modules() -> None:
    for name in list(sys.modules):
        if name.startswith("tools.research.archiver"):
            sys.modules.pop(name)


def _stub_modules() -> dict[str, types.ModuleType]:
    docsend_module = types.ModuleType("tools.research.archiver.download.docsend")

    async def route_all_docsends(*args, **kwargs):  # noqa: ARG001
        return []

    docsend_module.route_all_docsends = route_all_docsends

    google_module = types.ModuleType("tools.research.archiver.download.google")

    @dataclass
    class DownloadResult:
        status: str = "ok"
        output_path: str | None = None
        title: str | None = None
        error: str | None = None

    google_module.DownloadResult = DownloadResult
    google_module.download_doc = lambda *args, **kwargs: DownloadResult()  # noqa: ARG005,E731
    google_module.download_drive_file = lambda *args, **kwargs: DownloadResult()  # noqa: ARG005,E731
    google_module.download_folder = lambda *args, **kwargs: []  # noqa: ARG005,E731
    google_module.parse_google_url = lambda source_url: None  # noqa: ARG005,E731

    parse_module = types.ModuleType("tools.research.archiver.ingest.parse")
    parse_module.parse_manifest = lambda manifest_path: {  # noqa: E731
        "status": "ok",
        "source": str(manifest_path),
        "files": [],
    }

    return {
        docsend_module.__name__: docsend_module,
        google_module.__name__: google_module,
        parse_module.__name__: parse_module,
    }


class DocsendFallbackTest(unittest.TestCase):
    def test_extract_source_surfaces_docsend_blocker_and_manifest(self) -> None:
        _clear_archiver_modules()
        stubs = _stub_modules()

        sys.path.insert(0, str(REPO_ROOT))
        try:
            with patch.dict(sys.modules, stubs):
                client_module = importlib.import_module("tools.research.archiver.client")
                orchestrator = importlib.import_module("tools.research.archiver.download.orchestrator")

                fallback_payload = {
                    "status": "error",
                    "error": (
                        "This document is password-protected. Ask the user for the "
                        "passcode and retry with the passcode parameter."
                    ),
                    "files": [],
                    "docsend": {
                        "status": "passcode_required",
                        "strategy": "standalone_client",
                        "total_pages": None,
                        "downloaded": None,
                        "failed_slides": [],
                    },
                    "blocker": {
                        "kind": "passcode_required",
                        "message": (
                            "This document is password-protected. Ask the user for the "
                            "passcode and retry with the passcode parameter."
                        ),
                        "required_input": "password",
                    },
                }

                with patch.object(
                    orchestrator,
                    "_run_standalone_docsend_fallback",
                    return_value=fallback_payload,
                ):
                    def fake_attach(path, **kwargs):
                        return {
                            "attachment_id": f"att-{Path(path).name}",
                            "filename": kwargs.get("name") or Path(path).name,
                            "mime_type": kwargs.get("mime_type") or "application/octet-stream",
                            "size_bytes": Path(path).stat().st_size,
                        }

                    with patch.object(client_module, "save_attachment_from_path", side_effect=fake_attach):
                        client = client_module.ArchiverClient()
                        result = client.extract_source(
                            source_url="https://docsend.com/view/example",
                            company="Example",
                        )

                self.assertEqual(result["status"], "error")
                self.assertIn("password-protected", result["error"])
                self.assertNotEqual(result["error"], "Download stage failed")
                self.assertEqual(result["download"]["blocker"]["kind"], "passcode_required")
                self.assertEqual(result["download"]["blocker"]["required_input"], "password")
                self.assertEqual(
                    result["download"]["manifest_attachment"]["attachment_id"],
                    "att-manifest.json",
                )
        finally:
            sys.path = [entry for entry in sys.path if entry != str(REPO_ROOT)]
            _clear_archiver_modules()


if __name__ == "__main__":
    unittest.main()
