"""Unit tests for the Spaces Centaur session adapter."""

from __future__ import annotations

import io
import json
import unittest
import urllib.error
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

from spaces_adapter import SessionClient, SessionClientError


class _FakeResponse:
    def __init__(self, body: dict, status: int = 200) -> None:
        self._body = json.dumps(body).encode("utf-8")
        self.status = status

    def read(self) -> bytes:
        return self._body

    def __enter__(self) -> _FakeResponse:
        return self

    def __exit__(self, *args: object) -> None:
        return None


class SessionClientTest(unittest.TestCase):
    def test_create_session_posts_expected_body(self) -> None:
        captured: dict = {}

        def opener(request, timeout=None):  # noqa: ANN001
            captured["url"] = request.full_url
            captured["method"] = request.get_method()
            captured["body"] = json.loads(request.data.decode("utf-8"))
            captured["auth"] = request.get_header("Authorization")
            return _FakeResponse({"harness_type": "claudecode"})

        client = SessionClient("http://api.example", "tok", opener=opener)
        result = client.create_session(
            "thread/1",
            harness_type="claudecode",
            metadata={"source": "spaces"},
        )

        self.assertEqual(result["harness_type"], "claudecode")
        self.assertEqual(captured["method"], "POST")
        self.assertEqual(captured["url"], "http://api.example/v1/sessions/thread%2F1")
        self.assertEqual(captured["auth"], "Bearer tok")
        self.assertEqual(captured["body"]["harness_type"], "claudecode")
        self.assertEqual(captured["body"]["metadata"]["source"], "spaces")

    def test_http_error_raises_session_client_error(self) -> None:
        def opener(request, timeout=None):  # noqa: ANN001
            raise urllib.error.HTTPError(
                request.full_url,
                503,
                "unavailable",
                hdrs=None,
                fp=io.BytesIO(b'{"error":"busy"}'),
            )

        client = SessionClient("http://api.example", "tok", opener=opener)
        with self.assertRaises(SessionClientError) as ctx:
            client.execute(session_id="s1", input_lines=['{"type":"user_message"}'])
        self.assertEqual(ctx.exception.status, 503)
        self.assertEqual(ctx.exception.action, "execute session")


if __name__ == "__main__":
    unittest.main()
