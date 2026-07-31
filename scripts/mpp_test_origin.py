#!/usr/bin/env python3
"""Deterministic TLS origin for local MPP charge end-to-end tests."""

from __future__ import annotations

import base64
import json
import os
import ssl
from datetime import UTC, datetime, timedelta
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4


def _token(value: object) -> str:
    raw = json.dumps(value, separators=(",", ":")).encode()
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self) -> None:
        if self.path == "/healthz":
            self._json(200, {"ok": True})
            return
        if self.path == "/registry":
            realm = os.environ["MPP_TEST_REALM"]
            service_url = os.environ.get("MPP_TEST_SERVICE_URL", f"https://{realm}")
            self._json(
                200,
                {
                    "version": "1",
                    "services": [
                        {
                            "id": "local-paid",
                            "name": "Local paid origin",
                            "description": "Deterministic MPP charge fixture",
                            "serviceUrl": service_url,
                            "realm": realm,
                            "categories": ["test"],
                            "status": "active",
                            "endpoints": [
                                {
                                    "method": "GET",
                                    "path": "/paid",
                                    "description": "Paid test response",
                                    "payment": {
                                        "intent": "charge",
                                        "method": "tempo",
                                        "amount": os.environ.get(
                                            "MPP_TEST_AMOUNT", "1"
                                        ),
                                        "currency": os.environ.get(
                                            "MPP_TEST_CURRENCY",
                                            "0x20c0000000000000000000000000000000000000",
                                        ),
                                    },
                                }
                            ],
                        }
                    ],
                },
            )
            return
        if self.path != "/paid":
            self._json(404, {"error": "not_found"})
            return
        authorization = self.headers.get("Authorization", "")
        if not authorization.startswith("Payment "):
            request = {
                "amount": os.environ.get("MPP_TEST_AMOUNT", "1"),
                "currency": os.environ.get(
                    "MPP_TEST_CURRENCY", "0x20c0000000000000000000000000000000000000"
                ),
                "recipient": os.environ.get(
                    "MPP_TEST_RECIPIENT", "0x1111111111111111111111111111111111111111"
                ),
                "methodDetails": {
                    "chainId": int(os.environ.get("MPP_TEST_CHAIN_ID", "42431")),
                    "feePayer": True,
                },
            }
            challenge = (
                f'Payment id="{uuid4()}", realm="{os.environ["MPP_TEST_REALM"]}", '
                f'method="tempo", intent="charge", request="{_token(request)}", '
                f'expires="{(datetime.now(UTC) + timedelta(minutes=5)).isoformat()}"'
            )
            self.send_response(402)
            self.send_header("WWW-Authenticate", challenge)
            self._json_headers({"error": "payment_required"})
            return
        receipt = _token(
            {
                "status": "success",
                "method": "tempo",
                "timestamp": datetime.now(UTC).isoformat(),
                "reference": f"local-{uuid4()}",
            }
        )
        self.send_response(200)
        self.send_header("Payment-Receipt", receipt)
        self._json_headers({"ok": True})

    def _json(self, status: int, value: object) -> None:
        self.send_response(status)
        self._json_headers(value)

    def _json_headers(self, value: object) -> None:
        body = json.dumps(value, separators=(",", ":")).encode()
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format: str, *args: object) -> None:
        print(
            json.dumps(
                {
                    "time": datetime.now(UTC).isoformat(),
                    "client": self.client_address[0],
                    "request": format % args,
                    "traceparent": self.headers.get("traceparent"),
                    "payment_authorization_present": self.headers.get(
                        "Authorization", ""
                    ).startswith("Payment "),
                }
            ),
            flush=True,
        )


def main() -> None:
    server = ThreadingHTTPServer(("0.0.0.0", 8443), Handler)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(os.environ["MPP_TEST_CERT"], os.environ["MPP_TEST_KEY"])
    server.socket = context.wrap_socket(server.socket, server_side=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
