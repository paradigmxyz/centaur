#!/usr/bin/env python3
"""Seed Hermes with Centaur's broker-only Codex credential placeholder."""

from __future__ import annotations

import json
import os
import sys
import tempfile
from pathlib import Path


def seed(codex_auth_path: Path, hermes_auth_path: Path) -> None:
    codex = json.loads(codex_auth_path.read_text(encoding="utf-8"))
    tokens = codex.get("tokens")
    if not isinstance(tokens, dict) or not all(tokens.get(key) for key in ("access_token", "refresh_token")):
        raise ValueError("Codex auth must contain access_token and refresh_token placeholders")

    try:
        auth = json.loads(hermes_auth_path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        auth = {"version": 1, "providers": {}}
    if not isinstance(auth, dict) or not isinstance(auth.setdefault("providers", {}), dict):
        raise ValueError("Hermes auth must be a JSON object with a providers object")

    auth["providers"]["openai-codex"] = {
        "tokens": {
            "access_token": tokens["access_token"],
            "refresh_token": tokens["refresh_token"],
        },
        "last_refresh": codex.get("last_refresh"),
        "auth_mode": "chatgpt",
    }

    hermes_auth_path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(dir=hermes_auth_path.parent, prefix="auth.json.")
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            json.dump(auth, handle, indent=2)
            handle.write("\n")
        os.chmod(temporary, 0o600)
        os.replace(temporary, hermes_auth_path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


if __name__ == "__main__":
    seed(Path(sys.argv[1]), Path(sys.argv[2]))
