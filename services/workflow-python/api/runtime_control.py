from __future__ import annotations

import json
from typing import Any


class ControlPlaneError(RuntimeError):
    def __init__(
        self,
        code: str,
        message: str | None = None,
        status_code: int = 500,
        details: Any = None,
    ) -> None:
        self.code = str(code)
        self.message = str(message or code)
        self.status_code = int(status_code)
        self.details = details
        super().__init__(f"{self.code}: {self.message}")

    def to_dict(self) -> dict[str, Any]:
        payload: dict[str, Any] = {
            "code": self.code,
            "message": self.message,
            "status_code": self.status_code,
        }
        if self.details is not None:
            payload["details"] = self.details
        return payload


def canonical_json(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), default=str)


def decode_jsonb(value: Any, fallback: Any) -> Any:
    if value is None:
        return fallback
    if isinstance(value, str):
        try:
            return json.loads(value)
        except json.JSONDecodeError:
            return fallback
    return value
